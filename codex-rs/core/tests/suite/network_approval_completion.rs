//! Exercises remote network launch policy, including owner hooks and MXC's local-binding default.

use super::accept_initialized_exec_server;
use super::read_exec_server_json;
use super::send_exec_server_json;
use anyhow::Context;
use anyhow::Result;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use codex_config::test_support::CloudConfigBundleFixture;
use codex_core::TurnInputRequest;
use codex_core::config::NetworkProxySpec;
use codex_exec_server::ExecParams;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::items::CommandExecutionStatus;
use codex_protocol::items::TurnItem;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::sandbox::SandboxType;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::mount_response_once_match;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::sse;
use core_test_support::responses::sse_response;
use core_test_support::responses::start_mock_server;
use core_test_support::startup::STARTUP_TIMEOUT;
use core_test_support::test_codex::environment_config_for_selection;
use core_test_support::test_codex::test_codex;
use core_test_support::test_codex::turn_permission_fields;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::fs;
use std::time::Duration;
use test_case::test_case;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::WebSocketStream;

async fn accept_process_start(websocket: &mut WebSocketStream<TcpStream>) -> ExecParams {
    loop {
        let mut request = read_exec_server_json(websocket, STARTUP_TIMEOUT).await;
        if request["method"] == "process/start" {
            let start: ExecParams =
                serde_json::from_value(request["params"].take()).expect("process start");
            send_exec_server_json(
                websocket,
                json!({"id": request["id"], "result": {"processId": start.process_id}}),
            )
            .await;
            return start;
        }
        respond_to_setup_or_hook_request(websocket, &request).await;
    }
}

fn review_body(request: &wiremock::Request) -> Value {
    let bytes = if request
        .headers
        .get("content-encoding")
        .is_some_and(|value| value == "zstd")
    {
        zstd::stream::decode_all(std::io::Cursor::new(&request.body)).expect("decode request")
    } else {
        request.body.clone()
    };
    serde_json::from_slice(&bytes).expect("request JSON")
}

async fn respond_to_setup_or_hook_request(
    websocket: &mut WebSocketStream<TcpStream>,
    request: &Value,
) {
    let result = match request["method"].as_str() {
        Some("environment/info") => json!({
            "shell": {"name": "powershell", "path": "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"},
            "platformOs": "windows",
            "capabilities": {"networkProxyLaunch": true}
        }),
        Some("fs/getMetadata" | "fs/readFile" | "fs/readDirectory") => {
            send_exec_server_json(
                websocket,
                json!({
                    "id": request["id"],
                    "error": {"code": -32004, "message": "not found"}
                }),
            )
            .await;
            return;
        }
        Some("fs/canonicalize") => json!({"path": request["params"]["path"]}),
        Some("fs/walk") => json!({"entries": [], "errors": [], "truncated": false}),
        Some("process/write") => json!({"status": "accepted"}),
        Some("process/read") => {
            json!({"chunks": [], "nextSeq": 1, "exited": true, "exitCode": 0, "closed": true})
        }
        _ => panic!("unexpected setup or hook request: {request}"),
    };
    send_exec_server_json(websocket, json!({"id": request["id"], "result": result})).await;
}
