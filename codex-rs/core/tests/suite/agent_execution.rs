use anyhow::Context;
use anyhow::Result;
use codex_core::AgentTreeShutdownFailure;
use codex_core::AgentTreeShutdownFailureReason;
use codex_features::Feature;
use codex_protocol::models::PermissionProfile;
use codex_protocol::models::PermissionProfileSnapshot;
use codex_protocol::protocol::EnvironmentConfig;
use codex_protocol::protocol::EnvironmentConfigState;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::protocol::TurnEnvironmentRequests;
use core_test_support::apps_test_server::AppsTestServer;
use core_test_support::responses::ResponseMock;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::submit_thread_settings;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use futures::FutureExt;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use test_case::test_case;

const FIRST_PROMPT: &str = "spawn the first worker";
const FIRST_TASK: &str = "first worker task";
const SECOND_TASK: &str = "second worker task";
const MULTI_AGENT_V2_NAMESPACE: &str = "collaboration";

fn body_contains(request: &wiremock::Request, text: &str) -> bool {
    serde_json::from_slice::<serde_json::Value>(&request.body)
        .is_ok_and(|body| body.to_string().contains(text))
}

fn has_function_call_output(request: &wiremock::Request, call_id: &str) -> bool {
    serde_json::from_slice::<serde_json::Value>(&request.body).is_ok_and(|body| {
        body.get("input")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("type").and_then(serde_json::Value::as_str)
                        == Some("function_call_output")
                        && item.get("call_id").and_then(serde_json::Value::as_str) == Some(call_id)
                })
            })
    })
}

async fn mount_root_collaboration_call(
    server: &wiremock::MockServer,
    prompt: &'static str,
    call_id: &'static str,
    tool_name: &'static str,
    arguments: serde_json::Value,
) -> ResponseMock {
    let response_id = format!("resp-{call_id}");
    mount_sse_once_match(
        server,
        move |request: &wiremock::Request| body_contains(request, prompt),
        sse(vec![
            ev_response_created(&response_id),
            ev_function_call_with_namespace(
                call_id,
                MULTI_AGENT_V2_NAMESPACE,
                tool_name,
                &arguments.to_string(),
            ),
            ev_completed(&response_id),
        ]),
    )
    .await;

    let completion_id = format!("resp-{call_id}-complete");
    let mut answer = ev_assistant_message(&format!("msg-{call_id}"), "collaboration completed");
    answer["item"]["phase"] = json!("final_answer");
    mount_sse_once_match(
        server,
        move |request: &wiremock::Request| has_function_call_output(request, call_id),
        sse(vec![
            ev_response_created(&completion_id),
            answer,
            ev_completed(&completion_id),
        ]),
    )
    .await
}

async fn mount_completed_worker(
    server: &wiremock::MockServer,
    task: &'static str,
    parent_call_id: &'static str,
) -> ResponseMock {
    let response_id = format!("resp-worker-{parent_call_id}");
    let mut answer =
        ev_assistant_message(&format!("msg-worker-{parent_call_id}"), "worker completed");
    answer["item"]["phase"] = json!("final_answer");
    mount_sse_once_match(
        server,
        move |request: &wiremock::Request| {
            body_contains(request, task) && !has_function_call_output(request, parent_call_id)
        },
        sse(vec![
            ev_response_created(&response_id),
            answer,
            ev_completed(&response_id),
        ]),
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v2_nested_spawn_checks_shared_active_execution_capacity() -> Result<()> {
    let server = start_mock_server().await;
    let first_args = serde_json::to_string(&json!({
        "message": FIRST_TASK,
        "task_name": "first",
    }))?;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, FIRST_PROMPT),
        sse(vec![
            ev_response_created("first-response"),
            ev_function_call_with_namespace(
                "first-call",
                MULTI_AGENT_V2_NAMESPACE,
                "spawn_agent",
                &first_args,
            ),
            ev_completed("first-response"),
        ]),
    )
    .await;
    let second_args = serde_json::to_string(&json!({
        "message": SECOND_TASK,
        "task_name": "second",
    }))?;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, FIRST_TASK)
                && body_contains(request, r#"\"agent_name\":\"/root/first\""#)
        },
        sse(vec![
            ev_response_created("first-worker-response"),
            ev_function_call_with_namespace(
                "second-call",
                MULTI_AGENT_V2_NAMESPACE,
                "spawn_agent",
                &second_args,
            ),
            ev_completed("first-worker-response"),
        ]),
    )
    .await;
    let second_followup = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| has_function_call_output(request, "second-call"),
        sse(vec![
            ev_response_created("second-followup-response"),
            ev_assistant_message("second-followup-message", "blocked"),
            ev_completed("second-followup-response"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            has_function_call_output(request, "first-call")
                && body_contains(request, r#"\"agent_name\":\"/root\""#)
        },
        sse(vec![
            ev_response_created("first-followup-response"),
            ev_assistant_message("first-followup-message", "spawned"),
            ev_completed("first-followup-response"),
        ]),
    )
    .await;

    let mut builder = test_codex()
        .with_model("gpt-5.6-sol")
        .with_config(|config| {
            config
                .features
                .enable(Feature::Collab)
                .expect("test config should allow feature update");
            config
                .features
                .enable(Feature::MultiAgentV2)
                .expect("test config should allow feature update");
            config.multi_agent_v2.max_concurrent_threads_per_session = 2;
        });
    let test = builder.build(&server).await?;
    test.submit_turn(FIRST_PROMPT).await?;

    let second_output = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(output) = second_followup.function_call_output_text("second-call") {
                return output;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    assert_eq!(
        second_output,
        "collab spawn failed: agent thread limit reached"
    );
    assert_eq!(test.thread_manager.list_thread_ids().await.len(), 2);

    Ok(())
}

#[tokio::test]
#[tracing_test::traced_test]
async fn child_turn_start_preserves_root_attribution() -> Result<()> {
    let server = start_mock_server().await;
    mount_root_collaboration_call(
        &server,
        FIRST_PROMPT,
        "first-call",
        "spawn_agent",
        json!({
            "message": FIRST_TASK, "task_name": "first", "fork_turns": "none",
        }),
    )
    .await;
    let worker = mount_completed_worker(&server, FIRST_TASK, "first-call").await;
    let test = test_codex()
        .with_model("gpt-5.6-sol")
        .with_config(|config| {
            config.otel.log_agent_responses = true;
            config.otel.exporter = codex_config::types::OtelExporterKind::OtlpGrpc {
                endpoint: "http://127.0.0.1:1".into(),
                headers: Default::default(),
                tls: None,
            };
            config.features.enable(Feature::Collab).unwrap();
            config.features.enable(Feature::MultiAgentV2).unwrap();
        })
        .build_with_auto_env(&server)
        .await?;
    test.submit_turn(FIRST_PROMPT).await?;
    let thread_ids = test.thread_manager.list_thread_ids().await;
    assert_eq!(thread_ids.len(), 2);
    let mut starts = Vec::new();
    for thread_id in thread_ids {
        let thread = test.thread_manager.get_thread(thread_id).await?;
        if thread_id != test.session_configured.thread_id {
            wait_for_event(&thread, |event| matches!(event, EventMsg::TurnComplete(_))).await;
        }
        thread.flush_rollout().await?;
        let history = test
            .thread_store
            .load_latest_model_context(codex_thread_store::LoadThreadHistoryParams {
                thread_id,
                include_archived: false,
            })
            .await?;
        for item in history.items {
            if let codex_history::RolloutItem::EventMsg(EventMsg::TurnStarted(event)) = item {
                starts.push((thread_id, event));
            }
        }
    }
    worker.single_request();
    assert_eq!(starts.len(), 2);
    assert_ne!(starts[0].1.turn_id, starts[1].1.turn_id);
    let root_turn_id = &starts
        .iter()
        .find(|(id, _)| *id == test.session_configured.thread_id)
        .expect("root turn")
        .1
        .turn_id;
    assert!(
        starts
            .iter()
            .all(|(_, event)| { event.root_turn_id.as_ref() == Some(root_turn_id) })
    );
    logs_assert(|lines: &[&str]| {
        let logs: Vec<_> = lines
            .iter()
            .filter(|line| line.contains("codex.agent_response"))
            .collect();
        assert_eq!(logs.len(), 2);
        for (id, turn) in &starts {
            let line = logs
                .iter()
                .find(|line| line.contains(&format!(" conversation.id={id}")))
                .expect("response log");
            let (agent, item, text) = if *id == test.session_configured.thread_id {
                ("main", "msg-first-call", "collaboration completed")
            } else {
                assert!(line.contains(&format!(
                    " parent.conversation.id=\"{}\"",
                    test.session_configured.thread_id
                )));
                assert!(line.contains(&format!(" parent.turn.id={root_turn_id:?}")));
                assert!(line.contains(" initiating.agent.path=\"/root\""));
                ("subagent", "msg-worker-first-call", "worker completed")
            };
            for field in [
                format!(" agent.type={agent:?}"),
                format!(" turn.id={:?}", turn.turn_id),
                format!(" root.turn.id={root_turn_id:?}"),
                format!(" item.id={item:?}"),
                format!(" response={text:?}"),
            ] {
                assert!(line.contains(&field), "missing {field}: {line}");
            }
        }
        Ok(())
    });
    Ok(())
}

#[path = "agent_eviction_tests.rs"]
mod eviction_tests;

/// Tree shutdown waits for all admitted members, even after registry removal, without crossing
/// into another root's tree.
#[tokio::test]
async fn tree_shutdown_waits_for_children_without_affecting_other_roots() -> Result<()> {
    use codex_core::StartThreadOptions;
    use codex_protocol::error::CodexErrorDetails;
    use codex_protocol::protocol::InternalSessionSource;
    use codex_protocol::protocol::SessionSource;

    let server = start_mock_server().await;
    mount_root_collaboration_call(
        &server,
        FIRST_PROMPT,
        "first-call",
        "spawn_agent",
        json!({"message": FIRST_TASK, "task_name": "first", "fork_turns": "none"}),
    )
    .await;
    mount_completed_worker(&server, FIRST_TASK, "first-call").await;
    let test = test_codex()
        .with_model("gpt-5.6-sol")
        .with_config(|config| {
            config.features.enable(Feature::Collab).unwrap();
            config.features.enable(Feature::MultiAgentV2).unwrap();
        })
        .build_with_auto_env(&server)
        .await?;
    test.submit_turn(FIRST_PROMPT).await?;
    let root_id = test.session_configured.thread_id;
    let worker_id = test
        .thread_manager
        .list_thread_ids()
        .await
        .into_iter()
        .find(|id| *id != root_id)
        .expect("spawned worker");
    let worker = test.thread_manager.get_thread(worker_id).await?;
    wait_for_event(&worker, |event| matches!(event, EventMsg::TurnComplete(_))).await;
    let internal = test
        .thread_manager
        .spawn_internal_session(
            root_id,
            StartThreadOptions {
                session_source: Some(SessionSource::Internal(InternalSessionSource::Guardian)),
                ..StartThreadOptions::new(test.config.clone())
            },
        )
        .await?;
    let other = test
        .thread_manager
        .start_thread(StartThreadOptions::new(test.config.clone()))
        .await?;

    assert!(test.codex.wait_until_terminated().now_or_never().is_none());
    let shutdown = test
        .thread_manager
        .request_agent_tree_shutdown(worker_id)
        .await?;
    // A live child can leave the registry before its owner finishes teardown.
    test.thread_manager.remove_thread(&worker_id).await;
    let error = match test
        .thread_manager
        .spawn_internal_session(
            root_id,
            StartThreadOptions {
                session_source: Some(SessionSource::Internal(InternalSessionSource::Guardian)),
                ..StartThreadOptions::new(test.config.clone())
            },
        )
        .await
    {
        Ok(_) => panic!("agent-tree shutdown should reject a new internal session"),
        Err(error) => error,
    };
    assert!(matches!(
        error.details(),
        CodexErrorDetails::InvalidRequest(_)
    ));
    tokio::time::timeout(Duration::from_secs(/*secs*/ 10), shutdown.wait_detailed())
        .await?
        .expect("tree shutdown should succeed");
    // Legacy waiters observe the same successful completion after a detailed wait.
    shutdown.wait().await?;
    assert!(test.codex.wait_until_terminated().now_or_never().is_some());
    assert!(worker.wait_until_terminated().now_or_never().is_some());
    assert!(
        internal
            .thread
            .wait_until_terminated()
            .now_or_never()
            .is_some()
    );
    assert!(
        other
            .thread
            .wait_until_terminated()
            .now_or_never()
            .is_none()
    );
    // A root that has never used collaboration can also receive the signal.
    let other_shutdown = test
        .thread_manager
        .request_agent_tree_shutdown(other.thread_id)
        .await?;
    tokio::time::timeout(Duration::from_secs(/*secs*/ 10), other_shutdown.wait()).await??;
    assert!(
        other
            .thread
            .wait_until_terminated()
            .now_or_never()
            .is_some()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tree_shutdown_cancels_admitted_session_startup() -> Result<()> {
    use codex_core::StartThreadOptions;
    use codex_protocol::error::CodexErrorDetails;
    use codex_protocol::protocol::InternalSessionSource;
    use codex_protocol::protocol::SessionSource;

    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = test_codex().build_with_auto_env(&server).await?;
    let root_id = test.session_configured.thread_id;
    let mcp_server = start_mock_server().await;
    let (http_server, control) = AppsTestServer::mount_with_startup_control(&mcp_server).await?;
    let release = control.hold_next_successful_initialize();
    let mut config = test.config.clone();
    let mut servers = config.mcp_servers.get().clone();
    servers.insert(
        "stalled".to_owned(),
        serde_json::from_value(json!({
            "url": format!("{}/api/codex/ps/mcp", http_server.chatgpt_base_url),
            "http_headers": { "Authorization": "Bearer synthetic-test-token" },
            "required": true,
            "startup_timeout_sec": 120,
        }))?,
    );
    config.mcp_servers.set(servers)?;
    let mut start = Box::pin(test.thread_manager.spawn_internal_session(
        root_id,
        StartThreadOptions {
            session_source: Some(SessionSource::Internal(InternalSessionSource::Guardian)),
            ..StartThreadOptions::new(config)
        },
    ));

    tokio::select! {
        _ = &mut start => panic!("startup must wait for the required MCP server"),
        result = tokio::time::timeout(Duration::from_secs(10), async {
            while control.initialize_attempts() == 0 {
                tokio::task::yield_now().await;
            }
        }) => result.context("required MCP initialization did not begin")?,
    }
    let shutdown = test
        .thread_manager
        .request_agent_tree_shutdown(root_id)
        .await?;
    let (start_result, shutdown_result) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(&mut start, shutdown.wait())
    })
    .await
    .context("tree shutdown must cancel admitted startup")?;
    shutdown_result?;
    let error = match start_result {
        Ok(_) => panic!("cancelled internal session startup must not succeed"),
        Err(error) => error,
    };
    assert!(matches!(error.details(), CodexErrorDetails::TurnAborted));
    drop(release);
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test]
async fn tree_shutdown_reports_persistence_writer_failure() -> Result<()> {
    let server = start_mock_server().await;
    let test = test_codex().build_with_auto_env(&server).await?;
    let root_id = test.session_configured.thread_id;

    // Close the real local writer first so Core's shutdown observes a writer failure.
    test.thread_store.shutdown_thread(root_id).await?;
    let shutdown = test
        .thread_manager
        .request_agent_tree_shutdown(root_id)
        .await?;
    let report = tokio::time::timeout(Duration::from_secs(/*secs*/ 10), shutdown.wait_detailed())
        .await?
        .expect_err("shutdown should report the failed persistence writer");

    assert_eq!(
        report.failures,
        vec![AgentTreeShutdownFailure {
            operation: "session_shutdown",
            thread_id: Some(root_id),
            reason: AgentTreeShutdownFailureReason::OperationFailed {
                phase: "close_persistence",
                error_kind: "thread_store_not_found",
            },
        }]
    );
    assert_eq!(report.omitted_failures, 0);
    assert_eq!(
        shutdown
            .wait_detailed()
            .await
            .expect_err("failure persists"),
        report
    );
    shutdown
        .wait()
        .await
        .expect_err("legacy wait must still report the failure");
    Ok(())
}

#[path = "agent_mailbox.rs"]
mod agent_mailbox;
