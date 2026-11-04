use std::collections::HashMap;
use std::io;
use std::sync::Arc;

use codex_config::AppToolApproval;
use codex_config::Constrained;
use codex_protocol::mcp::McpServerInfo;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_rmcp_client::InProcessTransportFactory;
use codex_rmcp_client::RmcpClient;
use codex_utils_absolute_path::AbsolutePathBuf;
use futures::FutureExt;
use pretty_assertions::assert_eq;
use rmcp::model::JsonObject;
use rmcp::model::Tool;
use tokio::io::DuplexStream;

use super::McpBinding;
use super::PreparedMcpCall;
use crate::binding_clients::McpBindingClients;
use crate::client_tool_catalog::ClientToolCatalog;
use crate::connection_manager::McpConnectionSet;
use crate::rmcp_client::ManagedClient;
use crate::server::McpServerMetadata;
use crate::server::McpServerOrigin;
use crate::tools::ToolInfo;

const SERVER_NAME: &str = "docs";
const TOOL_NAME: &str = "search";

struct TestInProcessTransportFactory;

impl InProcessTransportFactory for TestInProcessTransportFactory {
    fn open(&self) -> futures::future::BoxFuture<'static, io::Result<DuplexStream>> {
        async {
            let (client_stream, _server_stream) = tokio::io::duplex(1);
            Ok(client_stream)
        }
        .boxed()
    }
}

struct TestStep {
    step: Arc<McpBinding>,
    client: Arc<RmcpClient>,
}

async fn test_step(
    label: &str,
    approval_mode: AppToolApproval,
    supports_sandbox_state_meta: bool,
) -> TestStep {
    let tool = ToolInfo {
        server_name: SERVER_NAME.to_string(),
        supports_parallel_tool_calls: false,
        server_origin: None,
        callable_name: TOOL_NAME.to_string(),
        callable_namespace: SERVER_NAME.to_string(),
        namespace_description: None,
        tool: Tool::new(
            TOOL_NAME.to_string(),
            format!("{label} catalog"),
            Arc::new(JsonObject::default()),
        ),
        openai_file_input_optional_fields: Default::default(),
        connector_id: None,
        connector_name: None,
    };
    let client = Arc::new(
        RmcpClient::new_in_process_client(Arc::new(TestInProcessTransportFactory))
            .await
            .expect("create in-process MCP client"),
    );
    let tool_catalog = Arc::new(ClientToolCatalog::new(vec![tool.clone()], /*meta*/ None));
    let managed_client = Arc::new(ManagedClient {
        connection_sharing: Default::default(),
        shared_elicitation: None,
        _auth_change_notifications: None,
        client: Arc::clone(&client),
        server_info: McpServerInfo {
            name: label.to_string(),
            title: Some(format!("{label} server")),
            version: "1.0.0".to_string(),
            description: None,
            icons: None,
            website_url: None,
        },
        tool_catalog: Arc::clone(&tool_catalog),
        tool_timeout: None,
        server_instructions: None,
        server_supports_sandbox_state_meta_capability: supports_sandbox_state_meta,
        client_supports_network_policy_meta_capability: false,
    });
    let clients = Arc::new(McpBindingClients::new(HashMap::from([(
        SERVER_NAME.to_string(),
        Arc::clone(&managed_client),
    )])));
    let connections = Arc::new(McpConnectionSet::empty(/*prefix_mcp_tool_names*/ true));
    let mut config = crate::mcp::tests::test_mcp_config(std::env::temp_dir());
    if label == "old" {
        config.approval_policy = Constrained::allow_any(AskForApproval::Never);
        config.permission_profile = PermissionProfile::Disabled;
    }
    config
        .server_permission_profiles
        .insert(SERVER_NAME.to_string(), config.permission_profile.clone());
    let config = Arc::new(config);
    let prepared = PreparedMcpCall::new(
        Arc::clone(&connections),
        managed_client,
        Arc::clone(&config),
        tool_catalog.read(Arc::new).await,
        tool.clone(),
        McpServerMetadata {
            environment_id: format!("{label}-environment"),
            pollutes_memory: label == "old",
            origin: Some(McpServerOrigin::StreamableHttp(format!(
                "https://{label}.example"
            ))),
            supports_parallel_tool_calls: false,
            default_tools_approval_mode: Some(approval_mode),
            tool_approval_modes: HashMap::new(),
        },
    )
    .expect("test call should retain its thread-owned permission profile");
    let calls = HashMap::from([((SERVER_NAME.to_string(), TOOL_NAME.to_string()), prepared)]);

    TestStep {
        step: Arc::new(McpBinding::new(
            connections,
            clients,
            config,
            vec![tool],
            calls,
        )),
        client,
    }
}

#[tokio::test]
async fn prepared_call_keeps_captured_connection_and_authority_after_refresh() -> anyhow::Result<()>
{
    let old = test_step(
        "old",
        AppToolApproval::Prompt,
        /*supports_sandbox_state_meta*/ true,
    )
    .await;
    let old_call = old
        .step
        .prepare_call(SERVER_NAME, TOOL_NAME)
        .expect("old step should prepare the advertised tool");
    let old_connections = Arc::downgrade(&old.step.connections);

    let new = test_step(
        "new",
        AppToolApproval::Approve,
        /*supports_sandbox_state_meta*/ false,
    )
    .await;
    let new_call = new
        .step
        .prepare_call(SERVER_NAME, TOOL_NAME)
        .expect("new step should prepare the advertised tool");

    assert_eq!(
        (
            old.step.tools()[0].tool.description.as_deref(),
            old_call.tool_info().tool.description.as_deref(),
            old_call.server_origin(),
            old_call.server_environment_id(),
            old_call.server_pollutes_memory(),
            old_call.tool_approval_mode(),
            old_call
                .server_supports_sandbox_state_meta_capability()
                .await?,
        ),
        (
            Some("old catalog"),
            Some("old catalog"),
            Some("https://old.example"),
            "old-environment",
            true,
            AppToolApproval::Prompt,
            true,
        )
    );
    assert_eq!(
        (
            new.step.tools()[0].tool.description.as_deref(),
            new_call.tool_info().tool.description.as_deref(),
            new_call.server_environment_id(),
            new_call.tool_approval_mode(),
        ),
        (
            Some("new catalog"),
            Some("new catalog"),
            "new-environment",
            AppToolApproval::Approve,
        )
    );
    assert!(Arc::ptr_eq(&old_call.client.client, &old.client));
    assert!(!Arc::ptr_eq(&old.client, &new.client));
    assert_eq!(
        (
            old_call.config().approval_policy.value(),
            old_call.permission_profile(),
        ),
        (AskForApproval::Never, &PermissionProfile::Disabled)
    );
    assert_eq!(
        new_call.config().approval_policy.value(),
        AskForApproval::OnRequest,
    );

    drop(old.step);
    assert!(
        old_connections.upgrade().is_some(),
        "the prepared call should keep its captured connection set alive"
    );
    drop(old_call);
    assert!(
        old_connections.upgrade().is_none(),
        "the captured connection set should be released with the prepared call"
    );
    Ok(())
}

#[tokio::test]
async fn prepared_call_does_not_reroute_after_captured_connection_closes() {
    let old = test_step(
        "old",
        AppToolApproval::Prompt,
        /*supports_sandbox_state_meta*/ true,
    )
    .await;
    let old_call = old
        .step
        .prepare_call(SERVER_NAME, TOOL_NAME)
        .expect("old step should prepare the advertised tool");
    let new = test_step(
        "new",
        AppToolApproval::Approve,
        /*supports_sandbox_state_meta*/ false,
    )
    .await;
    assert!(!Arc::ptr_eq(&old.client, &new.client));

    old.client.shutdown().await;

    let error = old_call
        .call(
            Some(serde_json::json!({"query": "codex"})),
            /*meta*/ None,
            /*timeout*/ None,
        )
        .await
        .expect_err("a call bound to a closed connection must fail");
    assert!(
        format!("{error:#}").contains("MCP client is shut down"),
        "the prepared call should fail on its captured client: {error:#}"
    );
}

#[tokio::test]
async fn sandbox_executable_requires_local_stdio_and_a_full_cli() -> anyhow::Result<()> {
    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt;

    let step = test_step(
        "old",
        AppToolApproval::Approve,
        /*supports_sandbox_state_meta*/ true,
    )
    .await;
    let call = step
        .step
        .prepare_call(SERVER_NAME, TOOL_NAME)
        .expect("prepared call");
    // Use a probe-capable CLI so remote/HTTP exclusions cannot pass merely
    // because the supplied executable lacks the sandbox capability.
    let executable = codex_utils_cargo_bin::cargo_bin("codex")?;
    for (server, runtime_environment, path, expected) in [
        (
            serde_json::json!({"command": "mcp"}),
            "local",
            Some(executable.clone()),
            Some(AbsolutePathBuf::from_absolute_path_checked(&executable)?),
        ),
        // The Rust test harness accepts `sandbox --help`, but must not be
        // advertised as a sandbox launcher: it lacks `--sandbox-state-json`.
        (
            serde_json::json!({"command": "mcp"}),
            "local",
            Some(std::env::current_exe()?),
            None,
        ),
        (serde_json::json!({"command": "mcp"}), "local", None, None),
        #[cfg(unix)]
        (
            serde_json::json!({"command": "mcp"}),
            "local",
            Some(std::ffi::OsStr::from_bytes(b"/non-utf8-\xff/codex").into()),
            None,
        ),
        (
            serde_json::json!({"command": "mcp"}),
            "local",
            Some("codex".into()),
            None,
        ),
        (
            serde_json::json!({"command": "mcp", "environment_id": "remote"}),
            "remote",
            Some(executable.clone()),
            None,
        ),
        (
            serde_json::json!({"command": "mcp"}),
            "remote",
            Some(executable.clone()),
            None,
        ),
        (
            serde_json::json!({"url": "http://127.0.0.1:8080/mcp"}),
            "local",
            Some(executable),
            None,
        ),
    ] {
        let mut call = call.clone();
        let config = Arc::make_mut(&mut call.config);
        config.codex_self_exe = path;
        let mut catalog = crate::ResolvedMcpCatalog::builder();
        catalog.register(crate::McpServerRegistration::from_config(
            SERVER_NAME.to_owned(),
            serde_json::from_value(server)?,
        ));
        config.mcp_server_catalog = catalog.build();
        call.server_metadata.environment_id = runtime_environment.to_owned();
        assert_eq!(call.sandbox_codex_executable().await, expected);

        let state = crate::SandboxState {
            permission_profile: PermissionProfile::default(),
            codex_linux_sandbox_exe: None,
            sandbox_cwd: codex_utils_path_uri::PathUri::from_host_native_path(
                std::env::current_dir()?,
            )?,
            use_legacy_landlock: false,
        };
        let expected_state = serde_json::to_value(&state)?;
        let mut metadata = Some(serde_json::json!({
            "unrelated": "retained",
            "codex/sandbox-state": {"stale": true},
            "codex/sandbox-state-meta": {"different": true},
            "codex/sandbox-launcher": {"codexExecutable": "must-not-survive"},
        }));
        call.augment_sandbox_metadata(&mut metadata, state).await?;
        let mut expected_metadata = serde_json::json!({
            "unrelated": "retained",
            "codex/sandbox-state": expected_state,
            "codex/sandbox-state-meta": expected_state,
        });
        if let Some(executable) = expected {
            expected_metadata["codex/sandbox-launcher"] =
                serde_json::json!({"codexExecutable": executable});
        }
        assert_eq!(metadata, Some(expected_metadata));
    }
    Ok(())
}
