use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::write_models_cache;
use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::routing::post;
use codex_app_server_protocol::CapabilityRootLocation;
use codex_app_server_protocol::ListMcpServerStatusParams;
use codex_app_server_protocol::ListMcpServerStatusResponse;
use codex_app_server_protocol::McpServerOauthLoginCompletedNotification;
use codex_app_server_protocol::McpServerOauthLoginResponse;
use codex_app_server_protocol::McpServerStatus;
use codex_app_server_protocol::McpServerToolCallParams;
use codex_app_server_protocol::McpServerToolCallResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SelectedCapabilityRoot;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput;
use codex_features::Feature;
use codex_http_client::HttpClientBuilder;
use codex_utils_path_uri::PathUri;
use core_test_support::responses;
use core_test_support::stdio_server_bin;
use futures::SinkExt;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use rmcp::handler::server::ServerHandler;
use rmcp::model::CallToolRequestParams;
use rmcp::model::CallToolResult;
use rmcp::model::JsonObject;
use rmcp::model::ListToolsResult;
use rmcp::model::ServerCapabilities;
use rmcp::model::ServerConfig;
use rmcp::model::Tool;
use rmcp::model::ToolAnnotations;
use rmcp::service::RequestContext;
use rmcp::service::RoleServer;
use rmcp::transport::StreamableHttpServerConfig;
use rmcp::transport::StreamableHttpService;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use serde_json::Value;
use serde_json::json;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(20);
const EXECUTOR_HTTP_MCP_URL: &str = "http://executor-only.invalid/mcp";
const HTTP_MCP_SERVER_NAME: &str = "executor_http";
const MCP_SERVER_NAME: &str = "executor_demo";
const OAUTH_MCP_SERVER_NAME: &str = "executor_oauth";
const PRE_REGISTERED_OAUTH_MCP_SERVER_NAME: &str = "executor_oauth_preregistered";
const EXECUTOR_OAUTH_MCP_URL: &str = "http://oauth-only.invalid/oauth-mcp";
const HOST_OAUTH_ACCESS_TOKEN: &str = "host-access-token";
const EXECUTOR_OAUTH_ACCESS_TOKEN: &str = "executor-access-token";
const EXECUTOR_ENV_NAME: &str = "MCP_EXECUTOR_MARKER";
const EXECUTOR_ENV_VALUE: &str = "executor-only";
const EXECUTOR_HTTP_AUTH_ENV_NAME: &str = "NODE_REPL_AUTH_TOKEN";
const EXECUTOR_HTTP_AUTH_ENV_VALUE: &str = "executor-only-http-token";
const EXECUTOR_ID: &str = "executor-1";
const PROJECT_MCP_SERVER_NAME: &str = "node_repl";
const PROJECT_MCP_BEARER_TOKEN: &str = "executor-browser-token";
const PROJECT_MCP_BEARER_ENV_NAME: &str = "NODE_REPL_AUTH_TOKEN";
const REFRESH_PROBE_SERVER_NAME: &str = "refresh_probe";
const TOOL_CALL_ID: &str = "executor-mcp-call";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selected_executor_discovers_browser_mcp_with_executor_only_bearer_token() -> Result<()> {
    assert!(std::env::var_os(PROJECT_MCP_BEARER_ENV_NAME).is_none());
    let responses_server = responses::start_mock_server().await;
    let http_listener = TcpListener::bind("127.0.0.1:0").await?;
    let http_addr = http_listener.local_addr()?;
    let http_mcp_service = StreamableHttpService::new(
        || Ok(ExecutorHttpMcpServer),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default().with_allowed_hosts(["executor-only.invalid"]),
    );
    let browser_starting = Arc::new(AtomicBool::new(true));
    let http_router =
        Router::new()
            .nest_service("/mcp", http_mcp_service)
            .layer(axum::middleware::from_fn(
                move |request: axum::extract::Request, next: axum::middleware::Next| {
                    let browser_starting = Arc::clone(&browser_starting);
                    async move {
                        if browser_starting.swap(false, Ordering::Relaxed) {
                            tokio::time::sleep(Duration::from_secs(2)).await;
                        }
                        let authorized = request
                            .headers()
                            .get(axum::http::header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .is_some_and(|value| value == "Bearer executor-browser-token");
                        if !authorized {
                            return axum::http::StatusCode::UNAUTHORIZED.into_response();
                        }
                        next.run(request).await
                    }
                },
            ));
    let http_server_handle = tokio::spawn(async move {
        let _ = axum::serve(http_listener, http_router).await;
    });
    let codex_home = TempDir::new()?;
    let executor_home = TempDir::new()?;
    MockResponsesConfig::new(&responses_server.uri())
        .with_sandbox_mode("danger-full-access")
        .write(codex_home.path())?;
    std::fs::write(
        codex_home.path().join("requirements.toml"),
        format!(
            "[mcp_servers.{PROJECT_MCP_SERVER_NAME}.identity]\nurl = \"{EXECUTOR_HTTP_MCP_URL}\"\n"
        ),
    )?;
    std::fs::write(
        executor_home.path().join("config.toml"),
        format!(
            "[mcp_servers.{PROJECT_MCP_SERVER_NAME}]\nurl = \"{EXECUTOR_HTTP_MCP_URL}\"\nbearer_token_env_var = \"{PROJECT_MCP_BEARER_ENV_NAME}\"\nrequired = true\nstartup_timeout_sec = 10\n\n[mcp_servers.ignored_stdio]\ncommand = \"executor-local-command\"\nenv_vars = [\"EXECUTOR_ONLY_TOKEN\"]\ncwd = \"./server\"\n\n[mcp_servers.policy_unlisted]\nurl = \"{EXECUTOR_HTTP_MCP_URL}\"\n"
        ),
    )?;
    // Browser auth tokens are not inherited by spawned executors, so use the production
    // WebSocket connection and put the token directly in the executor's environment.
    let mut executor = Command::new(codex_utils_cargo_bin::cargo_bin("codex")?)
        .args(["exec-server", "--listen", "ws://127.0.0.1:0"])
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .env("CODEX_HOME", executor_home.path())
        .env(PROJECT_MCP_BEARER_ENV_NAME, PROJECT_MCP_BEARER_TOKEN)
        .env("HTTP_PROXY", format!("http://{http_addr}"))
        .spawn()?;
    let stdout = executor.stdout.take().expect("executor stdout is piped");
    let mut lines = BufReader::new(stdout).lines();
    let executor_url = timeout(DEFAULT_READ_TIMEOUT, lines.next_line())
        .await??
        .expect("executor emits its websocket URL");
    std::fs::write(
        codex_home.path().join("environments.toml"),
        format!(
            "default = \"{EXECUTOR_ID}\"\ninclude_local = false\n\n[[environments]]\nid = \"{EXECUTOR_ID}\"\nurl = \"{executor_url}\"\n"
        ),
    )?;

    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let thread_id = start_thread(&mut app_server, /*selected_capability_roots*/ None).await?;

    let namespace = format!("mcp__{PROJECT_MCP_SERVER_NAME}");
    let response_mock = responses::mount_sse_sequence(
        &responses_server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("resp-browser-mcp-call"),
                responses::ev_function_call_with_namespace(
                    "browser-mcp-call",
                    &namespace,
                    "echo",
                    &json!({"message": "browser use works"}).to_string(),
                ),
                responses::ev_completed("resp-browser-mcp-call"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("resp-browser-mcp-done"),
                responses::ev_assistant_message("msg-browser-mcp-done", "Done"),
                responses::ev_completed("resp-browser-mcp-done"),
            ]),
        ],
    )
    .await;
    let request_id = app_server
        .send_turn_start_request(TurnStartParams {
            thread_id: thread_id.clone(),
            input: vec![UserInput::Text {
                text: "Use the executor browser".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let _: TurnStartResponse =
        timeout(DEFAULT_READ_TIMEOUT, app_server.read_response(request_id)).await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    let requests = response_mock.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].tool_by_name(&namespace, "echo").is_some());
    let output = requests[1].function_call_output("browser-mcp-call");
    assert!(
        output
            .get("output")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|output| output.contains("ECHOING: browser use works"))
    );

    let servers = mcp_server_statuses(&mut app_server, thread_id).await?;
    assert!(
        servers
            .iter()
            .any(|server| server.name == PROJECT_MCP_SERVER_NAME)
    );
    assert!(servers.iter().all(|server| server.name != "ignored_stdio"));
    assert!(
        servers
            .iter()
            .any(|server| server.name == "policy_unlisted" && server.tools.is_empty())
    );

    http_server_handle.abort();
    let _ = http_server_handle.await;

    Ok(())
}

/// A cached executor declaration must retain its credential boundary after reconnecting.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cached_executor_mcp_cannot_read_host_token_after_capability_downgrade() -> Result<()> {
    const TOKEN_ENV: &str = "MCP_CREDENTIAL_BOUNDARY_TEST_TOKEN";
    const HOST_TOKEN: &str = "host-only-credential-boundary-canary";
    const EXECUTOR_TOKEN: &str = "executor-only-credential-boundary-canary";
    const PARENT_PROMPT: &str = "spawn the credential boundary worker";
    const CHILD_PROMPT: &str = "check cached credential boundary";
    const DISCONNECT_MARKER: &str = "credential-boundary-disconnect";
    const CALL_ID: &str = "cached-credential-call";

    let responses_server = responses::start_mock_server().await;
    let codex_home = TempDir::new()?;
    let executor_home = TempDir::new()?;
    MockResponsesConfig::new(&responses_server.uri())
        .with_model("gpt-5.4")
        .with_sandbox_mode("danger-full-access")
        .with_approval_policy("never")
        .with_extra_config("[features.multi_agent_v2]\nenabled = true\n")
        .write(codex_home.path())?;
    write_models_cache(codex_home.path()).await?;

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let mcp_url = format!("http://{}/mcp", listener.local_addr()?);
    let initializations = Arc::new(AtomicUsize::new(0));
    let service = StreamableHttpService::new(
        || Ok(ExecutorHttpMcpServer),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let requests_seen = Arc::clone(&initializations);
    let router = Router::new()
        .nest_service("/mcp", service)
        .layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let requests_seen = Arc::clone(&requests_seen);
                async move {
                    let expected_authorization = format!("Bearer {EXECUTOR_TOKEN}");
                    assert_eq!(
                        request
                            .headers()
                            .get(axum::http::header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok()),
                        Some(expected_authorization.as_str())
                    );
                    if request.method() == axum::http::Method::POST {
                        let (parts, body) = request.into_parts();
                        let body = axum::body::to_bytes(body, 1024 * 1024).await.unwrap();
                        let value: Value = serde_json::from_slice(&body).unwrap();
                        if value["method"] == "initialize" {
                            requests_seen.fetch_add(1, Ordering::SeqCst);
                        }
                        return next
                            .run(axum::extract::Request::from_parts(parts, body.into()))
                            .await;
                    }
                    next.run(request).await
                }
            },
        ));
    let http_server = tokio::spawn(async move { axum::serve(listener, router).await });
    std::fs::write(
        executor_home.path().join("config.toml"),
        format!(
            "[mcp_servers.cached_executor]\nurl = \"{mcp_url}\"\nbearer_token_env_var = \"{TOKEN_ENV}\"\nrequired = true\nstartup_timeout_sec = 3\n"
        ),
    )?;
    let mut executor = Command::new(codex_utils_cargo_bin::cargo_bin("codex")?)
        .args(["exec-server", "--listen", "ws://127.0.0.1:0"])
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .env("CODEX_HOME", executor_home.path())
        .env(TOKEN_ENV, EXECUTOR_TOKEN)
        .spawn()?;
    let mut lines = BufReader::new(executor.stdout.take().expect("executor stdout")).lines();
    let executor_url = timeout(DEFAULT_READ_TIMEOUT, lines.next_line())
        .await??
        .expect("executor websocket URL");
    let proxy_listener = TcpListener::bind("127.0.0.1:0").await?;
    let proxy_url = format!("ws://{}", proxy_listener.local_addr()?);
    let downgraded = Arc::new(AtomicBool::new(false));
    let downgrade_seen = Arc::clone(&downgraded);
    let fresh_connection = Arc::new(AtomicBool::new(false));
    let fresh_connection_seen = Arc::clone(&fresh_connection);
    let (http_requests_tx, mut http_requests_rx) = mpsc::unbounded_channel();
    let proxy = tokio::spawn(async move {
        while let Ok((stream, _)) = proxy_listener.accept().await {
            let mut downstream = accept_async(stream).await?;
            let initialize = downstream.next().await.expect("executor initialize")?;
            let Message::Text(text) = &initialize else {
                anyhow::bail!("expected text initialize request");
            };
            let request: Value = serde_json::from_str(text)?;
            assert_eq!(request["method"], "initialize");
            let legacy = downgrade_seen.load(Ordering::SeqCst);
            if legacy && !request["params"]["resumeSessionId"].is_null() {
                // Successful resume preserves cached metadata; retire the old logical session.
                let response = json!({
                    "id": request["id"],
                    "error": {"code": -32602, "message": "test executor session retired"}
                });
                downstream
                    .send(Message::Text(response.to_string().into()))
                    .await?;
                downstream.close(/*msg*/ None).await?;
                continue;
            }
            fresh_connection_seen.fetch_or(legacy, Ordering::SeqCst);
            let (mut upstream, _) = connect_async(&executor_url).await?;
            upstream.send(initialize).await?;
            let (mut downstream_tx, mut downstream_rx) = downstream.split();
            let (mut upstream_tx, mut upstream_rx) = upstream.split();
            let requests = async {
                while let Some(message) = downstream_rx.next().await {
                    let message = message?;
                    if let Message::Text(text) = &message {
                        let request: Value = serde_json::from_str(text)?;
                        if request["method"] == "http/request" {
                            http_requests_tx.send(request.clone())?;
                        }
                        if request["method"] == "process/start"
                            && request["params"]["argv"]
                                .to_string()
                                .contains(DISCONNECT_MARKER)
                        {
                            // The child's first tool forces reconnect before its cached MCP starts.
                            assert_eq!(initializations.load(Ordering::SeqCst), 1);
                            downgrade_seen.store(true, Ordering::SeqCst);
                            break;
                        }
                    }
                    upstream_tx.send(message).await?;
                }
                Ok::<_, anyhow::Error>(())
            };
            let responses = async {
                while let Some(message) = upstream_rx.next().await {
                    let mut message = message?;
                    if legacy && let Message::Text(text) = &message {
                        let mut response: Value = serde_json::from_str(text)?;
                        if let Some(result) =
                            response.get_mut("result").and_then(Value::as_object_mut)
                        {
                            result.remove("environmentInfo");
                        }
                        if let Some(capabilities) = response
                            .pointer_mut("/result/capabilities")
                            .and_then(Value::as_object_mut)
                        {
                            capabilities.remove("httpHeaderEnvVars");
                        }
                        message = Message::Text(response.to_string().into());
                    }
                    downstream_tx.send(message).await?;
                }
                Ok::<_, anyhow::Error>(())
            };
            tokio::select! {
                result = requests => result?,
                result = responses => result?,
            }
        }
        Ok::<_, anyhow::Error>(())
    });
    std::fs::write(
        codex_home.path().join("environments.toml"),
        format!(
            "default = \"{EXECUTOR_ID}\"\ninclude_local = false\n\n[[environments]]\nid = \"{EXECUTOR_ID}\"\nurl = \"{proxy_url}\"\n"
        ),
    )?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[(TOKEN_ENV, Some(HOST_TOKEN))])
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let request_id = app_server
        .send_thread_start_request(ThreadStartParams {
            model: Some("gpt-5.4".to_string()),
            ..Default::default()
        })
        .await?;
    let ThreadStartResponse { thread, .. } =
        timeout(DEFAULT_READ_TIMEOUT, app_server.read_response(request_id)).await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_matching_notification("executor MCP ready", |notification| {
            notification.method == "mcpServer/startupStatus/updated"
                && notification.params.as_ref().is_some_and(|params| {
                    params["name"] == "cached_executor" && params["status"] == "ready"
                })
        }),
    )
    .await??;

    for (marker, events) in [
        (
            PARENT_PROMPT,
            vec![responses::ev_function_call_with_namespace(
                "spawn-credential-worker",
                "collaboration",
                "spawn_agent",
                &json!({"task_name": "worker", "message": CHILD_PROMPT, "fork_turns": "none"})
                    .to_string(),
            )],
        ),
        (
            CHILD_PROMPT,
            vec![responses::ev_exec_command_call(
                "disconnect-call",
                &format!("echo {DISCONNECT_MARKER}"),
            )],
        ),
        (
            "disconnect-call",
            // A process RPC waits for terminal recovery before the MCP credential lookup.
            vec![responses::ev_exec_command_call(
                "reconnect-barrier",
                "echo credential-boundary-ready",
            )],
        ),
        (
            "reconnect-barrier",
            vec![responses::ev_function_call_with_namespace(
                CALL_ID,
                "mcp__cached_executor",
                "echo",
                r#"{"message":"test credential boundary"}"#,
            )],
        ),
        (
            "spawn-credential-worker",
            vec![responses::ev_assistant_message("parent-done", "Done")],
        ),
    ] {
        responses::mount_sse_once_match(
            &responses_server,
            move |request: &wiremock::Request| {
                let body = String::from_utf8_lossy(&request.body);
                body.contains(marker)
                    && (marker != CHILD_PROMPT || !body.contains("spawn-credential-worker"))
            },
            responses::sse(
                events
                    .into_iter()
                    .chain([responses::ev_completed(marker)])
                    .collect(),
            ),
        )
        .await;
    }
    let child_done = responses::mount_sse_once_match(
        &responses_server,
        |request: &wiremock::Request| String::from_utf8_lossy(&request.body).contains(CALL_ID),
        responses::sse(vec![
            responses::ev_assistant_message("child-done", "Done"),
            responses::ev_completed("child-done"),
        ]),
    )
    .await;
    let request_id = app_server
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id,
            input: vec![UserInput::Text {
                text: PARENT_PROMPT.to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let _: TurnStartResponse =
        timeout(DEFAULT_READ_TIMEOUT, app_server.read_response(request_id)).await??;
    let request = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            if let Some(request) = child_done.requests().into_iter().next() {
                break request;
            }
            tokio::time::sleep(Duration::from_millis(/*millis*/ 10)).await;
        }
    })
    .await?;
    assert!(downgraded.load(Ordering::SeqCst));
    assert!(fresh_connection.load(Ordering::SeqCst));
    let (barrier_output, barrier_success) = request
        .function_call_output_content_and_success("reconnect-barrier")
        .expect("reconnect barrier should have an output");
    let barrier_output = barrier_output.expect("reconnect barrier output should contain text");
    assert_ne!(barrier_success, Some(false));
    assert!(
        barrier_output.contains("Process exited with code 0")
            && barrier_output.contains("credential-boundary-ready"),
        "reconnect barrier did not complete successfully: {barrier_output}"
    );
    let mut http_requests = Vec::new();
    while let Ok(request) = http_requests_rx.try_recv() {
        http_requests.push(request);
    }
    assert!(
        http_requests
            .iter()
            .any(|request| request["params"]["headers"].to_string().contains(TOKEN_ENV))
    );
    assert!(
        http_requests
            .iter()
            .all(|request| !request.to_string().contains(HOST_TOKEN)),
        "worker credentials reached executor HTTP requests"
    );
    let output = request.function_call_output(CALL_ID);
    assert!(
        output["output"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| {
                item["type"] == "input_text"
                    && item["text"]
                        == "MCP tool `cached_executor/echo` is not available to the model"
            })),
        "expected the failed MCP server to be unavailable: {output}"
    );
    proxy.abort();
    http_server.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guardian_review_does_not_discover_executor_mcp() -> Result<()> {
    let responses_server = responses::start_mock_server().await;
    let codex_home = TempDir::new()?;
    let executor_home = TempDir::new()?;
    MockResponsesConfig::new(&responses_server.uri())
        .with_approval_policy("on-request")
        .with_root_config("approvals_reviewer = \"auto_review\"")
        .enable_feature(Feature::GuardianApproval)
        .write(codex_home.path())?;
    std::fs::write(executor_home.path().join("config.toml"), "")?;
    let codex_bin = toml::Value::String(
        codex_utils_cargo_bin::cargo_bin("codex")?
            .to_string_lossy()
            .into_owned(),
    );
    let executor_home_value =
        toml::Value::String(executor_home.path().to_string_lossy().into_owned());
    std::fs::write(
        codex_home.path().join("environments.toml"),
        format!(
            "default = \"{EXECUTOR_ID}\"\ninclude_local = false\n\n[[environments]]\nid = \"{EXECUTOR_ID}\"\nprogram = {codex_bin}\nargs = [\"exec-server\", \"--listen\", \"stdio\"]\n[environments.env]\nCODEX_HOME = {executor_home_value}\n"
        ),
    )?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let thread_id = start_thread(&mut app_server, /*selected_capability_roots*/ None).await?;

    std::fs::write(
        executor_home.path().join("config.toml"),
        "[mcp_servers.unavailable_browser]\nurl = \"http://127.0.0.1:9/mcp\"\nrequired = true\nstartup_timeout_sec = 1\n",
    )?;
    let response_mock = responses::mount_sse_sequence(
        &responses_server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("resp-guardian-command"),
                responses::ev_function_call(
                    "guardian-shell-call",
                    "exec_command",
                    &json!({
                        "cmd": "echo guardian",
                        "sandbox_permissions": "require_escalated",
                        "justification": "Test Guardian isolation.",
                    })
                    .to_string(),
                ),
                responses::ev_completed("resp-guardian-command"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("resp-guardian-review"),
                responses::ev_assistant_message(
                    "guardian-review",
                    &json!({
                        "outcome": "deny",
                        "rationale": "Guardian can review without the browser.",
                    })
                    .to_string(),
                ),
                responses::ev_completed("resp-guardian-review"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("resp-guardian-done"),
                responses::ev_assistant_message("guardian-done", "Done"),
                responses::ev_completed("resp-guardian-done"),
            ]),
        ],
    )
    .await;
    let request_id = app_server
        .send_turn_start_request(TurnStartParams {
            thread_id,
            input: vec![UserInput::Text {
                text: "Review a command without starting the browser".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let _: TurnStartResponse =
        timeout(DEFAULT_READ_TIMEOUT, app_server.read_response(request_id)).await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[1].body_json()["client_metadata"]["x-openai-subagent"],
        "guardian"
    );
    let output = requests[2].function_call_output("guardian-shell-call");
    assert!(
        output
            .get("output")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|output| output.contains("Guardian can review without the browser."))
    );

    Ok(())
}

#[derive(Clone, Copy)]
struct ExecutorHttpMcpServer;

impl ServerHandler for ExecutorHttpMcpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        let input_schema: JsonObject = serde_json::from_value(json!({
            "type": "object",
            "properties": {"message": {"type": "string"}},
            "required": ["message"],
            "additionalProperties": false
        }))
        .map_err(|err| rmcp::ErrorData::internal_error(err.to_string(), None))?;
        let mut tool = Tool::new(
            Cow::Borrowed("echo"),
            Cow::Borrowed("Echo a message."),
            Arc::new(input_schema),
        );
        tool.annotations = Some(ToolAnnotations::new().read_only(true));

        Ok(ListToolsResult::with_all_items(vec![tool]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
        let message = request
            .arguments
            .as_ref()
            .and_then(|arguments| arguments.get("message"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        Ok(CallToolResult::structured(json!({
            "echo": format!("ECHOING: {message}")
        }))
        .into())
    }
}

async fn mcp_server_statuses(
    app_server: &mut TestAppServer,
    thread_id: String,
) -> Result<Vec<McpServerStatus>> {
    let request_id = app_server
        .send_list_mcp_server_status_request(ListMcpServerStatusParams {
            server_name: None,
            cursor: None,
            limit: None,
            detail: None,
            thread_id: Some(thread_id),
        })
        .await?;
    let response: ListMcpServerStatusResponse =
        timeout(DEFAULT_READ_TIMEOUT, app_server.read_response(request_id)).await??;
    Ok(response.data)
}

async fn start_thread(
    app_server: &mut TestAppServer,
    selected_capability_roots: Option<Vec<SelectedCapabilityRoot>>,
) -> Result<String> {
    let request_id = app_server
        .send_thread_start_request(ThreadStartParams {
            model: Some("mock-model".to_string()),
            selected_capability_roots,
            ..Default::default()
        })
        .await?;
    let ThreadStartResponse { thread, .. } =
        timeout(DEFAULT_READ_TIMEOUT, app_server.read_response(request_id)).await??;
    Ok(thread.id)
}
