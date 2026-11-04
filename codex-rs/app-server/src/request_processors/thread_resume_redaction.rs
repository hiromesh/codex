use codex_app_server_protocol::McpToolCallResult;
use codex_app_server_protocol::ThreadItemPayload;
use codex_app_server_protocol::Turn;
use serde_json::Value as JsonValue;

// Temporary bandaid for remote clients: thread/resume can include large MCP
// payloads. Keep this response-only so persisted rollout history, model resume
// history, and other APIs stay unchanged.
const REDACTED_PAYLOAD: &str = "[redacted]";
const CHATGPT_REMOTE_CLIENT_NAMES: &[&str] =
    &["codex_chatgpt_android_remote", "codex_chatgpt_ios_remote"];

pub(super) fn should_redact_thread_resume_payloads(client_name: Option<&str>) -> bool {
    client_name.is_some_and(|client_name| CHATGPT_REMOTE_CLIENT_NAMES.contains(&client_name))
}

pub(super) fn redact_thread_resume_payloads(turns: &mut [Turn]) {
    for turn in turns {
        turn.items.retain_mut(|entry| match &mut entry.payload {
            ThreadItemPayload::McpToolCall {
                arguments,
                result,
                error,
                ..
            } => {
                *arguments = JsonValue::String(REDACTED_PAYLOAD.to_string());
                if result.is_some() {
                    *result = Some(Box::new(redacted_mcp_tool_call_result()));
                }
                if let Some(error) = error {
                    error.message = REDACTED_PAYLOAD.to_string();
                }
                true
            }
            _ => true,
        });
    }
}

fn redacted_mcp_tool_call_result() -> McpToolCallResult {
    McpToolCallResult {
        content: vec![serde_json::json!({
            "type": "text",
            "text": REDACTED_PAYLOAD,
        })],
        structured_content: None,
        meta: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_app_server_protocol::McpToolCallError;
    use codex_app_server_protocol::McpToolCallStatus;
    use codex_app_server_protocol::SessionSource;
    use codex_app_server_protocol::Thread;
    use codex_app_server_protocol::ThreadItem;
    use codex_app_server_protocol::ThreadStatus;
    use codex_app_server_protocol::TurnItemsView;
    use codex_app_server_protocol::TurnStatus;
    use codex_utils_absolute_path::test_support::PathBufExt;
    use codex_utils_absolute_path::test_support::test_path_buf;
    use pretty_assertions::assert_eq;

    #[test]
    fn redacts_mcp_success_result() {
        let mut thread = test_thread(vec![
            ThreadItemPayload::AgentMessage {
                id: "agent-1".to_string(),
                text: "kept".to_string(),
                phase: None,
                memory_citation: None,
                delivery: None,
                questions: None,
            },
            ThreadItemPayload::McpToolCall {
                id: "mcp-1".to_string(),
                server: "docs".to_string(),
                tool: "lookup".to_string(),
                status: McpToolCallStatus::Completed,
                arguments: serde_json::json!({"secret":"argument"}),
                read_only_hint: None,
                result: Some(Box::new(McpToolCallResult {
                    content: vec![serde_json::json!({
                        "type": "text",
                        "text": "secret result"
                    })],
                    structured_content: Some(serde_json::json!({"secret":"structured"})),
                    meta: Some(serde_json::json!({"secret":"meta"})),
                })),
                error: None,
                duration_ms: Some(8),
            },
        ]);
        thread.turns[0].items[1].started_at_ms = Some(123);

        redact_thread_resume_payloads(&mut thread.turns);

        assert_eq!(thread.turns[0].items.len(), 2);
        assert_eq!(thread.turns[0].items[1].started_at_ms, Some(123));
        assert_eq!(
            thread.turns[0].items[0].payload,
            ThreadItemPayload::AgentMessage {
                id: "agent-1".to_string(),
                text: "kept".to_string(),
                phase: None,
                memory_citation: None,
                delivery: None,
                questions: None,
            }
        );
        assert_eq!(
            thread.turns[0].items[1].payload,
            ThreadItemPayload::McpToolCall {
                id: "mcp-1".to_string(),
                server: "docs".to_string(),
                tool: "lookup".to_string(),
                status: McpToolCallStatus::Completed,
                arguments: JsonValue::String(REDACTED_PAYLOAD.to_string()),
                read_only_hint: None,
                result: Some(Box::new(redacted_mcp_tool_call_result())),
                error: None,
                duration_ms: Some(8),
            }
        );
    }

    #[test]
    fn redacts_mcp_error_message() {
        let mut thread = test_thread(vec![ThreadItemPayload::McpToolCall {
            id: "mcp-1".to_string(),
            server: "docs".to_string(),
            tool: "lookup".to_string(),
            status: McpToolCallStatus::Failed,
            arguments: serde_json::json!({"secret":"argument"}),
            read_only_hint: None,
            result: None,
            error: Some(McpToolCallError {
                message: "secret error".to_string(),
            }),
            duration_ms: Some(8),
        }]);

        redact_thread_resume_payloads(&mut thread.turns);

        assert_eq!(
            thread.turns[0].items[0].payload,
            ThreadItemPayload::McpToolCall {
                id: "mcp-1".to_string(),
                server: "docs".to_string(),
                tool: "lookup".to_string(),
                status: McpToolCallStatus::Failed,
                arguments: JsonValue::String(REDACTED_PAYLOAD.to_string()),
                read_only_hint: None,
                result: None,
                error: Some(McpToolCallError {
                    message: REDACTED_PAYLOAD.to_string(),
                }),
                duration_ms: Some(8),
            }
        );
    }

    fn test_thread(items: Vec<ThreadItemPayload>) -> Thread {
        Thread {
            originator: None,
            environments: None,
            id: "thread-1".to_string(),
            extra: None,
            session_id: "session-1".to_string(),
            forked_from_id: None,
            parent_thread_id: None,
            preview: "preview".to_string(),
            ephemeral: false,
            section: None,
            section_entered_at: None,
            project_id: None,
            daybreak_enabled: None,
            history_mode: Default::default(),
            model_provider: "mock_provider".to_string(),
            model: None,
            reasoning_effort: None,
            created_at: 0,
            updated_at: 0,
            recency_at: Some(0),
            status: ThreadStatus::Idle,
            path: None,
            cwd: test_path_buf("/tmp").abs(),
            cli_version: "0.0.0".to_string(),
            source: SessionSource::Cli,
            can_accept_direct_input: None,
            thread_source: None,
            agent_nickname: None,
            agent_role: None,
            git_info: None,
            name: None,
            turns: vec![Turn {
                id: "turn-1".to_string(),
                root_turn_id: None,
                items: items.into_iter().map(ThreadItem::untimed).collect(),
                items_view: TurnItemsView::Full,
                status: TurnStatus::Completed,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            }],
        }
    }
}
