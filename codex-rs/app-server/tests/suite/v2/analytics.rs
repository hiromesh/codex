use anyhow::Result;
use app_test_support::DEFAULT_CLIENT_NAME;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::create_final_assistant_message_sse_response;
use app_test_support::create_mock_responses_server_sequence;
use app_test_support::to_response;
use app_test_support::write_api_key_auth;
use app_test_support::write_mock_responses_config_toml_with_chatgpt_base_url;
use codex_app_server_protocol::ApprovalsReviewer;
use codex_app_server_protocol::AskForApproval;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SandboxPolicy;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::ThreadItemPayload;
use codex_app_server_protocol::ThreadReadParams;
use codex_app_server_protocol::ThreadReadResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnSettingsUpdateParams;
use codex_app_server_protocol::TurnSettingsUpdateResponse;
use codex_app_server_protocol::TurnSettingsUpdateStatus;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput;
use codex_config::types::OtelExporterKind;
use codex_config::types::OtelHttpProtocol;
use codex_core::config::ConfigBuilder;
use codex_features::Feature;
use codex_models_manager::bundled_models_response;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::openai_models::ReasoningEffort;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_remote;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;
use wiremock::matchers::path_regex;

const SERVICE_VERSION: &str = "0.0.0-test";

#[tokio::test]
async fn usage_limit_window_reaches_turn_analytics() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": {
                "type": "usage_limit_reached",
                "message": "usage limit reached",
                "plan_type": "pro",
                "limit_window_minutes": 300
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .with_root_config(&format!("chatgpt_base_url = \"{}\"", server.uri()))
        .with_provider_config("supports_websockets = false")
        .write(codex_home.path())?;
    mount_analytics_capture(&server, codex_home.path()).await?;

    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_managed_config()
        .build_initialized()
        .await?;
    let thread = app_server
        .start_thread(ThreadStartParams::default())
        .await?
        .thread;
    let completed = app_server
        .start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: thread.id,
            input: vec![UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;

    let event = wait_for_matching_analytics_event(&server, Duration::from_secs(30), |event| {
        event["event_type"] == "codex_turn_event"
            && event["event_params"]["turn_id"] == completed.turn.id
    })
    .await?;
    let params = &event["event_params"];
    assert_eq!(
        json!([
            params["status"],
            params["turn_error"],
            params["codex_error_kind"],
            params["usage_limit_window_minutes"],
        ]),
        json!(["failed", "usageLimitExceeded", "usage_limit_reached", 300])
    );

    Ok(())
}

#[tokio::test]
async fn guardian_review_turns_and_tools_reach_analytics() -> Result<()> {
    skip_if_no_network!(Ok(()));
    const PRIVATE: &str = "guardian-private-content";
    const READ_TIMEOUT: Duration = Duration::from_secs(60);
    let server = responses::start_mock_server().await;
    let codex_home = TempDir::new()?;
    mount_analytics_capture(&server, codex_home.path()).await?;
    let catalog_path = codex_home.path().join("guardian-models.json");
    std::fs::write(
        &catalog_path,
        serde_json::to_vec(&bundled_models_response()?)?,
    )?;
    MockResponsesConfig::new(&server.uri())
        .with_root_config(&format!(
            "chatgpt_base_url = \"{}\"\nmodel_catalog_json = {}",
            server.uri(),
            serde_json::to_string(&catalog_path)?
        ))
        .with_provider_config("supports_websockets = false")
        .enable_feature(Feature::GuardianApproval)
        .write(codex_home.path())?;
    let parent_tool = |id: &str| {
        responses::sse_response(responses::sse(vec![
            responses::ev_response_created(id),
            responses::ev_function_call(
                id,
                "exec_command",
                &json!({
                    "cmd": format!("echo {id} > {id}.txt"),
                    "sandbox_permissions": "require_escalated",
                    "justification": "Exercise Guardian analytics",
                })
                .to_string(),
            ),
            responses::ev_completed(id),
        ]))
    };
    let requests = responses::mount_response_sequence(
        &server,
        vec![
            parent_tool("parent-first"),
            responses::sse_response(responses::sse(vec![
                responses::ev_response_created("guardian-first"),
                responses::ev_web_search_call_added_partial("guardian-search", "in_progress"),
                responses::ev_web_search_call_done("guardian-search", "completed", PRIVATE),
                responses::ev_reasoning_item("guardian-reasoning", &[PRIVATE], &[PRIVATE]),
                responses::ev_assistant_message(
                    "guardian-answer",
                    &json!({"outcome": "deny", "rationale": PRIVATE}).to_string(),
                ),
                responses::ev_completed("guardian-first"),
            ])),
            parent_tool("parent-second"),
            responses::sse_response(responses::sse_failed(
                "guardian-failed",
                "invalid_prompt",
                PRIVATE,
            )),
            parent_tool("parent-third"),
            responses::sse_response(responses::sse(vec![responses::ev_response_created(
                "guardian-pending",
            )]))
            .set_delay(READ_TIMEOUT),
        ],
    )
    .await;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_managed_config()
        // Basic Guardian command approvals require host-native paths.
        .without_auto_env()
        .build_initialized_with_timeout(READ_TIMEOUT)
        .await?;
    let ThreadStartResponse { thread, .. } = app_server
        .request(|request_id| ClientRequest::ThreadStart {
            request_id,
            params: ThreadStartParams {
                approval_policy: Some(AskForApproval::OnRequest),
                approvals_reviewer: Some(ApprovalsReviewer::AutoReview),
                ..Default::default()
            },
        })
        .await?;
    let request_id = app_server
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            input: vec![UserInput::Text {
                text: "Review three commands".to_string(),
                text_elements: Vec::new(),
            }],
            effort: Some(ReasoningEffort::High),
            ..Default::default()
        })
        .await?;
    let TurnStartResponse { turn } =
        timeout(READ_TIMEOUT, app_server.read_response(request_id)).await??;
    let reviews = timeout(READ_TIMEOUT, async {
        loop {
            let reviews = requests
                .requests()
                .into_iter()
                .map(|request| request.body_json()["client_metadata"].clone())
                .filter(|metadata| metadata["x-openai-subagent"] == "guardian")
                .collect::<Vec<_>>();
            if reviews.len() == 3 {
                break reviews;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await?;
    app_server
        .interrupt_turn_and_wait_for_aborted(thread.id.clone(), turn.id, READ_TIMEOUT)
        .await?;
    wait_for_matching_analytics_event(&server, READ_TIMEOUT, |event| {
        event["event_type"] == "codex_turn_event"
            && event["event_params"]["turn_id"] == reviews[2]["turn_id"]
    })
    .await?;
    timeout(READ_TIMEOUT, app_server.shutdown_gracefully()).await??;
    let events = captured_analytics_events(&server).await;
    assert!(!serde_json::to_string(&events)?.contains(PRIVATE));
    let parent_command = events
        .iter()
        .find(|event| {
            event["event_type"] == "codex_command_execution_event"
                && event["event_params"]["thread_id"] == thread.id
                && event["event_params"]["item_id"] == "parent-first"
        })
        .expect("denied parent command analytics");
    let params = &parent_command["event_params"];
    assert_eq!(
        json!([params["model_slug"], params["reasoning_effort"]]),
        json!(["mock-model", "high"])
    );
    let children = events
        .iter()
        .filter(|event| event["event_params"]["subagent_source"] == "guardian")
        .collect::<Vec<_>>();
    for event in &children {
        let params = &event["event_params"];
        assert_eq!(
            json!([
                params["thread_id"],
                params["session_id"],
                params["parent_thread_id"],
                params["thread_source"]
            ]),
            json!([
                reviews[0]["thread_id"],
                thread.session_id,
                thread.id,
                "guardian_review"
            ])
        );
    }
    let turns = children
        .iter()
        .filter(|event| event["event_type"] == "codex_turn_event")
        .map(|event| {
            let params = &event["event_params"];
            let started_at = params["started_at"].as_u64().expect("turn start time");
            let completed_at = params["completed_at"]
                .as_u64()
                .expect("turn completion time");
            assert!(started_at > 0 && completed_at >= started_at);
            assert!(params["duration_ms"].is_u64());
            assert_eq!(params["guardian_v2_enabled"], false);
            json!([
                params["turn_id"],
                params["status"],
                params["turn_error"],
                params["total_tool_call_count"],
                params["web_search_count"]
            ])
        })
        .collect::<Vec<_>>();
    assert_eq!(
        turns,
        vec![
            json!([reviews[0]["turn_id"], "completed", null, 1, 1]),
            json!([reviews[1]["turn_id"], "failed", "other", 0, 0]),
            json!([reviews[2]["turn_id"], "interrupted", null, 0, 0]),
        ]
    );
    let tools = children
        .iter()
        .filter(|event| event["event_type"] == "codex_web_search_event")
        .collect::<Vec<_>>();
    let [tool] = tools.as_slice() else {
        anyhow::bail!(
            "expected one Guardian web search event, got {}",
            tools.len()
        );
    };
    let params = &tool["event_params"];
    let started_at = params["started_at_ms"].as_u64().expect("tool start time");
    let completed_at = params["completed_at_ms"]
        .as_u64()
        .expect("tool completion time");
    assert!(started_at > 0 && completed_at >= started_at);
    assert_eq!(params["duration_ms"], completed_at - started_at);
    assert_eq!(
        json!([
            params["turn_id"],
            params["item_id"],
            params["terminal_status"]
        ]),
        json!([reviews[0]["turn_id"], "guardian-search", "completed"])
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_agent_v2_tools_emit_collaborator_analytics() -> Result<()> {
    const PARENT_PROMPT: &str = "exercise collaborator analytics";
    const READ_TIMEOUT: Duration = Duration::from_secs(30);
    let calls = [
        (
            "spawn_agent",
            json!({"task_name": "worker", "message": "PRIVATE_CHILD", "fork_turns": "none"}),
        ),
        (
            "send_message",
            json!({"target": "worker", "message": "PRIVATE_MESSAGE"}),
        ),
        (
            "followup_task",
            json!({"target": "worker", "message": "PRIVATE_FOLLOWUP"}),
        ),
        ("interrupt_agent", json!({"target": "worker"})),
        ("list_agents", json!({})),
        (
            "send_message",
            json!({"target": "missing", "message": "PRIVATE_MESSAGE"}),
        ),
        (
            "followup_task",
            json!({"target": "/root", "message": "PRIVATE_FOLLOWUP"}),
        ),
        ("interrupt_agent", json!({"target": "/root"})),
        ("list_agents", json!({"unexpected": true})),
        ("send_message", json!({})),
    ];
    let server = responses::start_mock_server().await;
    for (index, (tool, args)) in calls.iter().enumerate() {
        let response_id = format!("parent-response-{index}");
        responses::mount_sse_once_match(
            &server,
            |request: &wiremock::Request| {
                String::from_utf8_lossy(&request.body).contains(PARENT_PROMPT)
            },
            responses::sse(vec![
                responses::ev_response_created(&response_id),
                responses::ev_function_call_with_namespace(
                    &format!("call-{index}"),
                    "collaboration",
                    tool,
                    &args.to_string(),
                ),
                responses::ev_completed(&response_id),
            ]),
        )
        .await;
    }
    // Child turns and the final parent request only need an ordinary completion.
    Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(responses::sse(vec![responses::ev_completed(
                    "response-done",
                )])),
        )
        .with_priority(10)
        .mount(&server)
        .await;

    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .enable_feature(Feature::MultiAgentV2)
        .with_root_config(&format!("chatgpt_base_url = \"{}\"", server.uri()))
        .write(codex_home.path())?;
    mount_analytics_capture(&server, codex_home.path()).await?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_managed_config()
        .build_initialized()
        .await?;
    let thread = app_server
        .start_thread(ThreadStartParams::default())
        .await?
        .thread;
    let completed = app_server
        .start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: thread.id.clone(),
            input: vec![UserInput::Text {
                text: PARENT_PROMPT.to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let read: ThreadReadResponse = app_server
        .request(|request_id| ClientRequest::ThreadRead {
            request_id,
            params: ThreadReadParams {
                thread_id: thread.id.clone(),
                include_turns: true,
            },
        })
        .await?;
    let items = &read
        .thread
        .turns
        .iter()
        .find(|turn| turn.id == completed.turn.id)
        .expect("completed parent turn should be readable")
        .items;
    let child_thread_id = items
        .iter()
        .find_map(|item| match &item.payload {
            ThreadItemPayload::SubAgentActivity {
                id,
                agent_thread_id,
                ..
            } if id == "call-0" => Some(agent_thread_id.clone()),
            _ => None,
        })
        .expect("spawn should retain its public activity item");
    assert!(
        !items
            .iter()
            .any(|item| matches!(&item.payload, ThreadItemPayload::CollabAgentToolCall { .. }))
    );
    let activity_ids = items
        .iter()
        .filter_map(|item| match &item.payload {
            ThreadItemPayload::SubAgentActivity { id, .. } if id.starts_with("call-") => {
                Some(id.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(activity_ids, vec!["call-0", "call-1", "call-2", "call-3"]);

    for (index, (tool, _)) in calls.iter().enumerate() {
        let event = wait_for_matching_analytics_event(&server, READ_TIMEOUT, |event| {
            event["event_type"] == "codex_collab_agent_tool_call_event"
                && event["event_params"]["item_id"] == format!("call-{index}")
        })
        .await?;
        let params = &event["event_params"];
        let receivers = match index {
            0..=3 => vec![child_thread_id.clone()],
            6 | 7 => vec![thread.id.clone()],
            _ => Vec::new(),
        };
        assert_eq!(
            json!({
                "tool": params["tool_name"],
                "sender": params["sender_thread_id"],
                "receivers": params["receiver_thread_ids"],
                "receiverCount": params["receiver_thread_count"],
                "turn": params["turn_id"],
                "status": params["terminal_status"],
                "origin": params["originating_response_id"],
                "failure": params["failure_kind"],
                "hasDuration": params["execution_duration_ms"].is_u64(),
            }),
            json!({
                "tool": tool,
                "sender": thread.id,
                "receivers": receivers,
                "receiverCount": receivers.len(),
                "turn": completed.turn.id,
                "status": if index < 5 { "completed" } else { "failed" },
                "origin": format!("parent-response-{index}"),
                "failure": if index < 5 { None } else { Some("tool_error") },
                "hasDuration": true,
            })
        );
        assert_eq!(params["duration_ms"], params["execution_duration_ms"]);
        assert!(!event.to_string().contains("PRIVATE_"));
    }
    let turn_event = wait_for_matching_analytics_event(&server, READ_TIMEOUT, |event| {
        event["event_type"] == "codex_turn_event"
            && event["event_params"]["turn_id"] == completed.turn.id
    })
    .await?;
    assert_eq!(
        json!({
            "total": turn_event["event_params"]["total_tool_call_count"],
            "subagent": turn_event["event_params"]["subagent_tool_call_count"],
            "dynamic": turn_event["event_params"]["dynamic_tool_call_count"],
        }),
        json!({"total": calls.len(), "subagent": calls.len(), "dynamic": 0})
    );

    Ok(())
}

#[tokio::test]
async fn metrics_exporters_respect_analytics_preferences() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await?;
    for exporter in [
        OtelExporterKind::None,
        OtelExporterKind::Statsig,
        OtelExporterKind::OtlpHttp {
            endpoint: "http://localhost:4318".to_string(),
            headers: HashMap::new(),
            protocol: OtelHttpProtocol::Json,
            tls: None,
        },
        OtelExporterKind::OtlpGrpc {
            endpoint: "http://localhost:4317".to_string(),
            headers: HashMap::new(),
            tls: None,
        },
    ] {
        config.otel.metrics_exporter = exporter;
        for analytics_enabled in [None, Some(false), Some(true)] {
            config.analytics_enabled = analytics_enabled;
            for default_analytics_enabled in [false, true] {
                let expected_metrics = match config.otel.metrics_exporter {
                    OtelExporterKind::None => false,
                    // The built-in exporter is disabled in debug builds even with consent.
                    OtelExporterKind::Statsig => {
                        !cfg!(debug_assertions)
                            && analytics_enabled.unwrap_or(default_analytics_enabled)
                    }
                    OtelExporterKind::OtlpHttp { .. } | OtelExporterKind::OtlpGrpc { .. } => true,
                };
                let provider = codex_core::otel_init::build_provider(
                    &config,
                    SERVICE_VERSION,
                    Some("codex-app-server"),
                    default_analytics_enabled,
                )
                .map_err(|err| anyhow::anyhow!(err.to_string()))?;
                let metrics = provider.as_ref().and_then(|otel| otel.metrics());
                let metrics_enabled = metrics.is_some();
                // Exercise the nonempty export path that concurrent tests can
                // trigger through process-global metrics.
                let record_result = metrics
                    .map(|metrics| {
                        metrics.counter("codex.test.analytics_preferences", /*inc*/ 1, &[])
                    })
                    .transpose();
                if let Some(provider) = provider {
                    // Keep Tokio running while the gRPC exporter flushes; a
                    // synchronous drop can deadlock this current-thread runtime.
                    provider
                        .shutdown_with_timeout(Duration::from_secs(/*secs*/ 5))
                        .await?;
                }
                record_result?;
                assert_eq!(
                    metrics_enabled, expected_metrics,
                    "exporter={:?}, analytics={analytics_enabled:?}, default={default_analytics_enabled}",
                    config.otel.metrics_exporter,
                );
            }
        }
    }

    Ok(())
}

pub(crate) async fn mount_analytics_capture(server: &MockServer, codex_home: &Path) -> Result<()> {
    Mock::given(method("POST"))
        .and(path("/codex/analytics-events/events"))
        .respond_with(ResponseTemplate::new(200))
        .mount(server)
        .await;

    write_api_key_auth(codex_home, "sk-test-key")?;

    app_test_support::mount_workspace_routing(server).await;
    Ok(())
}

pub(crate) async fn captured_analytics_events(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|request| request.url.path() == "/codex/analytics-events/events")
        .flat_map(|request| {
            let payload: Value = serde_json::from_slice(&request.body).expect("analytics payload");
            payload["events"]
                .as_array()
                .expect("analytics events")
                .clone()
        })
        .collect()
}

pub(crate) async fn wait_for_analytics_payload(
    server: &MockServer,
    read_timeout: Duration,
) -> Result<Value> {
    let body = timeout(read_timeout, async {
        loop {
            let Some(requests) = server.received_requests().await else {
                tokio::time::sleep(Duration::from_millis(25)).await;
                continue;
            };
            if let Some(request) = requests.iter().find(|request| {
                request.method == "POST" && request.url.path() == "/codex/analytics-events/events"
            }) {
                break request.body.clone();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await?;
    serde_json::from_slice(&body).map_err(|err| anyhow::anyhow!("invalid analytics payload: {err}"))
}

pub(crate) async fn wait_for_analytics_event(
    server: &MockServer,
    read_timeout: Duration,
    event_type: &str,
) -> Result<Value> {
    wait_for_matching_analytics_event(server, read_timeout, |event| {
        event["event_type"] == event_type
    })
    .await
}

pub(crate) async fn wait_for_goal_event(
    server: &MockServer,
    read_timeout: Duration,
    event_kind: &str,
    goal_status: &str,
) -> Result<Value> {
    wait_for_matching_analytics_event(server, read_timeout, |event| {
        event["event_type"] == "codex_goal_event"
            && event["event_params"]["event_kind"] == event_kind
            && event["event_params"]["goal_status"] == goal_status
    })
    .await
}

pub(crate) async fn wait_for_matching_analytics_event(
    server: &MockServer,
    read_timeout: Duration,
    matches: impl Fn(&Value) -> bool,
) -> Result<Value> {
    timeout(read_timeout, async {
        loop {
            let Some(requests) = server.received_requests().await else {
                tokio::time::sleep(Duration::from_millis(25)).await;
                continue;
            };
            for request in &requests {
                if request.method != "POST"
                    || !request
                        .url
                        .path()
                        .ends_with("/codex/analytics-events/events")
                {
                    continue;
                }
                let payload: Value = serde_json::from_slice(&request.body)
                    .map_err(|err| anyhow::anyhow!("invalid analytics payload: {err}"))?;
                let Some(events) = payload["events"].as_array() else {
                    continue;
                };
                if let Some(event) = events.iter().find(|event| matches(event)) {
                    return Ok::<Value, anyhow::Error>(event.clone());
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await?
}

pub(crate) fn thread_initialized_event(payload: &Value) -> Result<&Value> {
    let events = payload["events"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("analytics payload missing events array"))?;
    events
        .iter()
        .find(|event| event["event_type"] == "codex_thread_initialized")
        .ok_or_else(|| anyhow::anyhow!("codex_thread_initialized event should be present"))
}

pub(crate) fn assert_basic_thread_initialized_event(
    event: &Value,
    thread_id: &str,
    session_id: &str,
    expected_product_client_id: &str,
    expected_model: &str,
    initialization_mode: &str,
    expected_thread_source: &str,
) {
    assert_eq!(event["event_params"]["thread_id"], thread_id);
    assert_eq!(event["event_params"]["session_id"], session_id);
    assert_eq!(
        event["event_params"]["app_server_client"]["product_client_id"],
        expected_product_client_id
    );
    assert_eq!(
        event["event_params"]["app_server_client"]["client_name"],
        DEFAULT_CLIENT_NAME
    );
    assert_eq!(
        event["event_params"]["app_server_client"]["rpc_transport"],
        "stdio"
    );
    assert_eq!(event["event_params"]["model"], expected_model);
    assert_eq!(event["event_params"]["ephemeral"], false);
    assert_eq!(
        event["event_params"]["thread_source"],
        expected_thread_source
    );
    assert_eq!(
        event["event_params"]["subagent_source"],
        serde_json::Value::Null
    );
    assert_eq!(
        event["event_params"]["parent_thread_id"],
        serde_json::Value::Null
    );
    assert_eq!(
        event["event_params"]["initialization_mode"],
        initialization_mode
    );
    assert!(event["event_params"]["created_at"].as_u64().is_some());
}
