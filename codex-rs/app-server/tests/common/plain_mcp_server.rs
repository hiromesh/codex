//! Minimal streamable-HTTP MCP server for app-server tests.
//!
//! This replaces the former Apps test fixture in tests that only need an ordinary
//! MCP server: `initialize`, `tools/list`, and `tools/call` are answered over
//! wiremock, with optional startup failure/stall control.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use serde_json::Value;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::Request;
use wiremock::Respond;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;

const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";

/// Controls startup behavior and tool inventory for a [`PlainMcpServer`].
#[derive(Clone, Default)]
pub struct PlainMcpControl {
    initialize_attempts: Arc<AtomicUsize>,
    remaining_initialize_failures: Arc<AtomicUsize>,
    successful_initialize_gate: Arc<Mutex<VecDeque<std::sync::mpsc::Receiver<()>>>>,
    tools: Arc<Mutex<Vec<Value>>>,
    call_result: Arc<Mutex<Option<Value>>>,
}

impl PlainMcpControl {
    pub fn initialize_attempts(&self) -> usize {
        self.initialize_attempts.load(Ordering::SeqCst)
    }

    /// Fails the next `attempts` `initialize` requests with a non-retryable error.
    pub fn fail_next_initialize_attempts(&self, attempts: usize) {
        self.remaining_initialize_failures
            .store(attempts, Ordering::SeqCst);
    }

    /// Blocks the next successful `initialize` until the returned sender fires.
    pub fn hold_next_successful_initialize(&self) -> std::sync::mpsc::Sender<()> {
        let (release, gate) = std::sync::mpsc::channel();
        self.successful_initialize_gate
            .lock()
            .expect("MCP initialization gate lock should not be poisoned")
            .push_back(gate);
        release
    }

    /// Replaces the tools returned by `tools/list`.
    pub fn set_tools(&self, tools: Vec<Value>) {
        *self
            .tools
            .lock()
            .expect("MCP tool list lock should not be poisoned") = tools;
    }

    /// Replaces the result returned by `tools/call`.
    pub fn set_call_result(&self, result: Value) {
        *self
            .call_result
            .lock()
            .expect("MCP call result lock should not be poisoned") = Some(result);
    }
}

/// A mounted ordinary MCP server and its startup controls.
pub struct PlainMcpServer {
    /// Base URL of the server (scheme, host, and configured MCP path).
    pub url: String,
    pub control: PlainMcpControl,
}

/// Mounts an ordinary streamable-HTTP MCP server on `server` at `mcp_path`.
pub async fn mount_plain_mcp_server(server: &MockServer, mcp_path: &str) -> PlainMcpServer {
    let control = PlainMcpControl::default();
    Mock::given(method("POST"))
        .and(path_regex(mcp_path.to_string()))
        .respond_with(PlainMcpResponder {
            control: control.clone(),
        })
        .mount(server)
        .await;
    PlainMcpServer {
        url: format!("{}{mcp_path}", server.uri()),
        control,
    }
}

struct PlainMcpResponder {
    control: PlainMcpControl,
}

impl Respond for PlainMcpResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = match serde_json::from_slice(&request.body) {
            Ok(body) => body,
            Err(error) => {
                return ResponseTemplate::new(400).set_body_json(json!({
                    "error": format!("invalid JSON-RPC body: {error}"),
                }));
            }
        };
        let Some(method) = body.get("method").and_then(Value::as_str) else {
            return ResponseTemplate::new(400).set_body_json(json!({
                "error": "missing method in JSON-RPC request",
            }));
        };

        match method {
            "initialize" => {
                self.control.initialize_attempts.fetch_add(1, Ordering::SeqCst);
                if self
                    .control
                    .remaining_initialize_failures
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                        remaining.checked_sub(1)
                    })
                    .is_ok()
                {
                    return ResponseTemplate::new(400).set_body_json(json!({
                        "error": "simulated non-retryable MCP startup failure",
                    }));
                }
                let gate = self
                    .control
                    .successful_initialize_gate
                    .lock()
                    .expect("MCP initialization gate lock should not be poisoned")
                    .pop_front();
                if let Some(gate) = gate
                    && gate.recv().is_err()
                {
                    return ResponseTemplate::new(500).set_body_json(json!({
                        "error": "MCP initialization gate was dropped before release",
                    }));
                }
                let id = body.get("id").cloned().unwrap_or(Value::Null);
                let protocol_version = body
                    .pointer("/params/protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or(DEFAULT_PROTOCOL_VERSION);
                ResponseTemplate::new(200).set_body_json(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": protocol_version,
                        "capabilities": {
                            "tools": {
                                "listChanged": true
                            }
                        },
                        "serverInfo": {
                            "name": "plain-mcp-test-server",
                            "version": "0.0.0"
                        }
                    }
                }))
            }
            "notifications/initialized" => ResponseTemplate::new(202),
            "tools/list" => {
                let id = body.get("id").cloned().unwrap_or(Value::Null);
                let tools = self
                    .control
                    .tools
                    .lock()
                    .expect("MCP tool list lock should not be poisoned")
                    .clone();
                ResponseTemplate::new(200).set_body_json(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "tools": tools },
                }))
            }
            "tools/call" => {
                let id = body.get("id").cloned().unwrap_or(Value::Null);
                let result = self
                    .control
                    .call_result
                    .lock()
                    .expect("MCP call result lock should not be poisoned")
                    .clone()
                    .unwrap_or_else(|| json!({
                        "content": [{"type": "text", "text": "ok"}],
                    }));
                ResponseTemplate::new(200).set_body_json(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result,
                }))
            }
            _ => ResponseTemplate::new(202),
        }
    }
}
