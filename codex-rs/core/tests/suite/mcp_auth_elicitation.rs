//! Verify MCP prompts and elicitation flow through actual turns.

use anyhow::Result;
use codex_core::CodexThread;
use codex_core::StartThreadOptions;
use codex_core::TurnInputRequest;
use codex_core::config::Config;
use codex_core::config::Constrained;
use codex_features::Feature;
const CODEX_APPS_MCP_SERVER_NAME: &str = "codex_apps";
use codex_protocol::approvals::ElicitationRequest;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::items::McpToolCallStatus;
use codex_protocol::items::TurnItem;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::ElicitationAction;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::McpStartupStatus;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::user_input::UserInput;
use core_test_support::apps_test_server::AppsTestServer;
use core_test_support::apps_test_server::CALENDAR_CREATE_EVENT_TOOL_NAME;
use core_test_support::apps_test_server::CODEX_APPS_TOOL_NAMESPACE;
use codex_config::types::AppToolApproval;
use core_test_support::apps_test_server::recorded_apps_tool_calls;
use core_test_support::apps_test_server::search_capable_apps_builder;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::wait_for_event;
use std::time::Duration;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use test_case::test_case;

fn set_apps_approval_mode(config: &mut Config, approval_mode: AppToolApproval) {
    let mut servers = config.mcp_servers.get().clone();
    if let Some(server) = servers.get_mut(CODEX_APPS_MCP_SERVER_NAME) {
        server.default_tools_approval_mode = Some(approval_mode);
    }
    config
        .mcp_servers
        .set(servers)
        .expect("apps server approval mode should be accepted");
}

/// Waits until the Apps server actually reports readiness.
///
/// A child session can publish an empty startup summary before its extension
/// contributors install the Apps server, and deferred servers report readiness
/// through a startup update instead of the summary; neither may be mistaken for
/// a completed startup of the server under test.
async fn wait_for_apps_mcp_ready(thread: &CodexThread) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let event = thread
                .next_event()
                .await
                .expect("event stream should stay open");
            match event.msg {
                EventMsg::McpStartupUpdate(update)
                    if update.server == CODEX_APPS_MCP_SERVER_NAME =>
                {
                    match update.status {
                        McpStartupStatus::Ready => return Ok(()),
                        McpStartupStatus::Failed { error, .. } => {
                            return Err(anyhow::anyhow!(
                                "MCP server {CODEX_APPS_MCP_SERVER_NAME} failed to start: {error}"
                            ));
                        }
                        McpStartupStatus::Cancelled => {
                            return Err(anyhow::anyhow!(
                                "MCP server {CODEX_APPS_MCP_SERVER_NAME} startup was cancelled"
                            ));
                        }
                        McpStartupStatus::Starting => {}
                    }
                }
                EventMsg::McpStartupComplete(summary) => {
                    if summary
                        .ready
                        .iter()
                        .any(|server| server == CODEX_APPS_MCP_SERVER_NAME)
                    {
                        return Ok(());
                    }
                    if let Some(failure) = summary
                        .failed
                        .iter()
                        .find(|failure| failure.server == CODEX_APPS_MCP_SERVER_NAME)
                    {
                        return Err(anyhow::anyhow!(
                            "MCP server {CODEX_APPS_MCP_SERVER_NAME} failed to start: {}",
                            failure.error
                        ));
                    }
                    if summary
                        .cancelled
                        .iter()
                        .any(|server| server == CODEX_APPS_MCP_SERVER_NAME)
                    {
                        return Err(anyhow::anyhow!(
                            "MCP server {CODEX_APPS_MCP_SERVER_NAME} startup was cancelled"
                        ));
                    }
                }
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("timed out waiting for {CODEX_APPS_MCP_SERVER_NAME} startup"))?
}

#[derive(Clone, Copy)]
enum SubagentRequestKind {
    ToolApproval,
    LegacyToolApproval,
}

#[test_case(SubagentRequestKind::ToolApproval, ElicitationAction::Accept; "child_tool_approval_waits_for_user_before_execution")]
#[test_case(SubagentRequestKind::LegacyToolApproval, ElicitationAction::Accept; "child_legacy_tool_approval_is_rejected_before_execution")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn core_generated_mcp_elicitations_support_subagents(
    kind: SubagentRequestKind,
    decision: ElicitationAction,
) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let apps = AppsTestServer::mount_searchable(&server).await?;
    let test = search_capable_apps_builder(apps.chatgpt_base_url)
        .with_config(move |config| {
            config.permissions.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);
            config.approvals_reviewer = ApprovalsReviewer::User;
            config
                .features
                .enable(Feature::AuthElicitation)
                .expect("enable auth elicitation");
            config
                .features
                .set_enabled(
                    Feature::ToolCallMcpElicitation,
                    !matches!(kind, SubagentRequestKind::LegacyToolApproval),
                )
                .expect("configure MCP approval transport");
            set_apps_approval_mode(config, AppToolApproval::Prompt);
        })
        .build_with_auto_env(&server)
        .await?;
    let child = test
        .thread_manager
        .start_thread(StartThreadOptions {
            session_source: Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: test.session_configured.thread_id,
                depth: 1,
                agent_path: None,
                agent_nickname: None,
                agent_role: None,
            })),
            environments: Some(vec![test.executor_environment().request()]),
            ..StartThreadOptions::new(test.config.clone())
        })
        .await?
        .thread;
    wait_for_apps_mcp_ready(&child).await?;
    responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_function_call_with_namespace(
                "calendar-call",
                CODEX_APPS_TOOL_NAMESPACE,
                CALENDAR_CREATE_EVENT_TOOL_NAME,
                &json!({"title": "Lunch", "starts_at": "2026-06-18T12:00:00Z"}).to_string(),
            ),
            responses::ev_completed("tool-response"),
        ]),
    )
    .await;
    let follow_up = responses::mount_sse_once(&server, responses::sse_completed("done")).await;
    child
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: "Use [$calendar](app://calendar) to create a calendar event.".into(),
                text_elements: Vec::new(),
            }])
            .with_thread_settings(ThreadSettingsOverrides {
                approval_policy: Some(AskForApproval::OnRequest),
                permission_profile: Some(PermissionProfile::Disabled),
                ..Default::default()
            }),
        )
        .await?;
    let mut completed_status = None;
    let mut prompts = 0;
    loop {
        let event = wait_for_event(&child, |event| {
            assert!(
                !matches!(event, EventMsg::RequestUserInput(_)),
                "subagents must not fall back to request_user_input"
            );
            if let EventMsg::ItemCompleted(event) = event
                && let TurnItem::McpToolCall(item) = &event.item
                && item.id == "calendar-call"
            {
                completed_status = Some(item.status);
            }
            matches!(
                event,
                EventMsg::ElicitationRequest(_) | EventMsg::TurnComplete(_)
            )
        })
        .await;
        let EventMsg::ElicitationRequest(request) = event else {
            break;
        };
        assert_eq!(prompts, 0, "one tool call must not prompt twice");
        prompts += 1;
        assert_eq!(request.server_name, CODEX_APPS_MCP_SERVER_NAME);
        assert!(
            follow_up.requests().is_empty(),
            "the tool must wait for input"
        );
        match kind {
            SubagentRequestKind::ToolApproval => {
                assert!(matches!(request.request, ElicitationRequest::Form { .. }));
                assert!(recorded_apps_tool_calls(&server).await.is_empty());
            }
            SubagentRequestKind::LegacyToolApproval => {
                panic!("legacy subagent approval must be rejected before prompting");
            }
        }
        child
            .submit(Op::ResolveElicitation {
                server_name: request.server_name,
                request_id: request.id,
                decision,
                content: None,
                meta: None,
            })
            .await?;
    }
    let expects_prompt = !matches!(kind, SubagentRequestKind::LegacyToolApproval);
    assert_eq!(prompts, usize::from(expects_prompt));
    assert_eq!(
        completed_status,
        Some(if matches!(kind, SubagentRequestKind::ToolApproval) {
            McpToolCallStatus::Completed
        } else {
            McpToolCallStatus::Failed
        })
    );
    assert_eq!(
        recorded_apps_tool_calls(&server).await.len(),
        usize::from(expects_prompt)
    );
    let output = follow_up
        .single_request()
        .function_call_output("calendar-call");
    let output_text = match &output["output"] {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        other => panic!("unexpected MCP output: {other}"),
    };
    if matches!(kind, SubagentRequestKind::LegacyToolApproval) {
        assert!(output_text.contains("parent agent"));
    }
    child.shutdown_and_wait().await?;
    test.codex.shutdown_and_wait().await?;
    Ok(())
}
