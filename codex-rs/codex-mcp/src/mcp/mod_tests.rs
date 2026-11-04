use super::*;
use crate::McpServerRegistration;
use crate::connection_manager::tests::create_ready_async_managed_client;
use crate::mcp::auth::McpAuthStatusEntry;
use crate::rmcp_client::StartupOutcomeError;
use crate::server::McpCredentialPolicy;
use codex_config::Constrained;
use codex_config::types::AppToolApproval;
use codex_config::types::AuthKeyringBackendKind;
use codex_protocol::models::ManagedFileSystemPermissions;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::GranularApprovalConfig;
use codex_rmcp_client::McpAuthState;
use futures::FutureExt;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::test]
async fn status_snapshot_only_downgrades_oauth_authentication_failures() {
    let auth_failure = StartupOutcomeError::Failed {
        error: "OAuth refresh token was rejected".to_string(),
        is_authentication_required: true,
        shutdown: None,
    };
    let mut manager = McpConnectionSet::empty(/*prefix_mcp_tool_names*/ true);
    let mut auth_status_entries = HashMap::new();
    for (name, auth_state, startup_error) in [
        (
            "oauth-failed",
            McpAuthState::OAuth,
            Some(auth_failure.clone()),
        ),
        ("oauth-ready", McpAuthState::OAuth, None),
        (
            "oauth-provider-error",
            McpAuthState::OAuth,
            Some(StartupOutcomeError::Failed {
                error: "provider temporarily unavailable".to_string(),
                is_authentication_required: false,
                shutdown: None,
            }),
        ),
        ("bearer", McpAuthState::BearerToken, Some(auth_failure)),
    ] {
        let mut client = create_ready_async_managed_client(Vec::new()).await;
        if let Some(error) = startup_error {
            client.client = futures::future::ready(Err(error)).boxed().shared();
        }
        manager.insert_test_client(name, client);
        auth_status_entries.insert(
            name.to_string(),
            McpAuthStatusEntry {
                config: None,
                auth_state,
            },
        );
    }

    let server_names = auth_status_entries.keys().cloned().collect();
    let snapshot = collect_mcp_server_status_snapshot_from_manager(
        &manager,
        auth_status_entries,
        server_names,
        McpSnapshotDetail::ToolsAndAuthOnly,
    )
    .await;

    assert_eq!(
        snapshot.auth_statuses,
        HashMap::from([
            ("oauth-failed".to_string(), McpAuthStatus::NotLoggedIn),
            ("oauth-ready".to_string(), McpAuthStatus::OAuth),
            ("oauth-provider-error".to_string(), McpAuthStatus::OAuth),
            ("bearer".to_string(), McpAuthStatus::BearerToken),
        ]),
    );
}

pub(crate) fn test_mcp_config(codex_home: PathBuf) -> McpConfig {
    McpConfig {
        chatgpt_base_url: "https://chatgpt.com".to_string(),
        requires_read_only_mcp_tools: false,
        codex_home,
        mcp_oauth_credentials_store_mode: OAuthCredentialsStoreMode::default(),
        oauth_refresh_mode: McpOAuthRefreshMode::Legacy,
        auth_keyring_backend_kind: AuthKeyringBackendKind::default(),
        mcp_oauth_callback_port: None,
        mcp_oauth_callback_url: None,
        optional_mcp_startup_grace: DEFAULT_OPTIONAL_MCP_STARTUP_GRACE,
        skill_mcp_dependency_install_enabled: true,
        approval_policy: Constrained::allow_any(AskForApproval::OnRequest),
        permission_profile: PermissionProfile::default(),
        environment_cwds: HashMap::new(),
        server_permission_profiles: HashMap::new(),
        codex_self_exe: None,
        codex_linux_sandbox_exe: None,
        use_legacy_landlock: false,
        prefix_mcp_tool_names: true,
        non_prefixed_mcp_tool_servers: Vec::new(),
        protocol_mode: McpProtocolMode::Legacy,
        discover_local_stdio: false,
        mcp_tool_list_changed: true,
        client_elicitation_capability: ElicitationCapability::default(),
        mcp_server_catalog: ResolvedMcpCatalog::default(),
    }
}

pub(crate) fn test_elicitation_config(
    server_name: &str,
    approval_policy: AskForApproval,
    permission_profile: PermissionProfile,
) -> Arc<McpConfig> {
    let mut config = test_mcp_config(PathBuf::new());
    config.approval_policy = Constrained::allow_any(approval_policy);
    config.permission_profile = permission_profile.clone();
    config
        .server_permission_profiles
        .insert(server_name.to_string(), permission_profile);
    Arc::new(config)
}

#[test]
fn ema_authenticated_servers_are_not_enabled() {
    let server: McpServerConfig = serde_json::from_value(serde_json::json!({
        "url": "https://resource.example/mcp", "auth": "ema_auth",
        "oauth": { "client_id": "resource-client", "authorization_server_issuer": "https://as.example" }
    }))
    .unwrap();
    let mut catalog = ResolvedMcpCatalog::builder();
    catalog.register(McpServerRegistration::from_config(
        "configured".into(),
        server,
    ));
    let mut config = test_mcp_config(PathBuf::new());
    config.mcp_server_catalog = catalog.build_with_environment_authority(|_| {
        crate::McpEnvironmentAuthority::Unrestricted
    });
    let servers = effective_mcp_servers(&config);
    assert!(!servers["configured"].enabled());
}

#[test]
fn qualified_mcp_tool_name_prefix_sanitizes_server_names_without_lowercasing() {
    assert_eq!(
        qualified_mcp_tool_name_prefix("Some-Server"),
        "mcp__Some_Server__".to_string()
    );
}

#[test]
fn mcp_server_permissions_handle_unattached_and_threadless_servers() {
    let mut config = test_mcp_config(PathBuf::new());
    config.permission_profile = PermissionProfile::Disabled;
    let missing_server: McpServerConfig = serde_json::from_value(serde_json::json!({
        "url": "https://example.com/mcp",
        "environment_id": "missing",
    }))
    .expect("valid MCP server config");
    let mut selected_server = missing_server.clone();
    selected_server.environment_id = "unattached".to_string();
    let mut catalog = ResolvedMcpCatalog::builder();
    catalog.register(McpServerRegistration::from_config(
        "missing".to_string(),
        missing_server,
    ));
    catalog.register(McpServerRegistration::from_executor_config(
        "selected".to_string(),
        selected_server,
    ));
    config.mcp_server_catalog = catalog.build();
    let servers = effective_mcp_servers(&config);
    assert_eq!(config.permission_profile_for_server("selected"), None);
    config.set_server_permission_profiles(&servers, std::iter::empty());

    assert_eq!(config.permission_profile_for_server("selected"), None);
    assert_eq!(config.permission_profile_for_server("missing"), None);

    let config = config.for_threadless_operations(&servers);
    assert_eq!(
        config.permission_profile_for_server("selected"),
        Some(&PermissionProfile::default())
    );
}

#[test]
fn mcp_prompt_auto_approval_honors_unrestricted_managed_profiles() {
    assert!(mcp_permission_prompt_is_auto_approved(
        AskForApproval::Never,
        &PermissionProfile::Managed {
            file_system: ManagedFileSystemPermissions::Unrestricted,
            network: NetworkSandboxPolicy::Enabled,
        },
        McpPermissionPromptAutoApproveContext::default(),
    ));
    assert!(mcp_permission_prompt_is_auto_approved(
        AskForApproval::Never,
        &PermissionProfile::Managed {
            file_system: ManagedFileSystemPermissions::Unrestricted,
            network: NetworkSandboxPolicy::Restricted,
        },
        McpPermissionPromptAutoApproveContext::default(),
    ));
    assert!(!mcp_permission_prompt_is_auto_approved(
        AskForApproval::Never,
        &PermissionProfile::read_only(),
        McpPermissionPromptAutoApproveContext::default(),
    ));
    assert!(!mcp_permission_prompt_is_auto_approved(
        AskForApproval::OnRequest,
        &PermissionProfile::Managed {
            file_system: ManagedFileSystemPermissions::Unrestricted,
            network: NetworkSandboxPolicy::Enabled,
        },
        McpPermissionPromptAutoApproveContext::default(),
    ));
}

#[test]
fn mcp_prompt_auto_approval_honors_approved_tools_in_all_permission_modes() {
    for approval_policy in [
        AskForApproval::UnlessTrusted,
        AskForApproval::OnRequest,
        AskForApproval::Granular(GranularApprovalConfig {
            sandbox_approval: true,
            rules: true,
            skill_approval: true,
            request_permissions: true,
            mcp_elicitations: true,
        }),
        AskForApproval::Never,
    ] {
        assert!(mcp_permission_prompt_is_auto_approved(
            approval_policy,
            &PermissionProfile::read_only(),
            McpPermissionPromptAutoApproveContext {
                tool_approval_mode: Some(AppToolApproval::Approve),
            },
        ));
    }

    assert!(!mcp_permission_prompt_is_auto_approved(
        AskForApproval::OnRequest,
        &PermissionProfile::read_only(),
        McpPermissionPromptAutoApproveContext {
            tool_approval_mode: Some(AppToolApproval::Auto),
        },
    ));
}

#[test]
fn effective_mcp_servers_preserve_executor_credential_policy() {
    let mut config = test_mcp_config(PathBuf::new());
    let server: McpServerConfig = serde_json::from_value(serde_json::json!({
        "url": "https://executor.example/mcp",
        "environment_id": "remote",
        "bearer_token_env_var": "MCP_EXECUTOR_CREDENTIAL_CANARY",
    }))
    .expect("valid MCP server config");
    let mut catalog = ResolvedMcpCatalog::builder();
    catalog.register(McpServerRegistration::from_executor_config(
        "executor".to_string(),
        server.clone(),
    ));
    catalog.register(McpServerRegistration::from_config(
        "host".to_string(),
        server,
    ));
    config.mcp_server_catalog = catalog.build();
    let effective = effective_mcp_servers(&config);

    assert_eq!(
        effective["executor"].credential_policy(),
        McpCredentialPolicy::ExecutorOnly,
    );
    assert_eq!(
        effective["host"].credential_policy(),
        McpCredentialPolicy::HostFallbackAllowed,
    );
}

#[test]
#[should_panic(expected = "materialized MCP server must have a catalog registration")]
fn effective_mcp_servers_require_catalog_registration() {
    let config = test_mcp_config(PathBuf::new());
    let server: McpServerConfig = serde_json::from_value(serde_json::json!({
        "url": "https://executor.example/mcp",
        "environment_id": "remote",
        "bearer_token_env_var": "MCP_EXECUTOR_CREDENTIAL_CANARY",
    }))
    .expect("valid MCP server config");

    effective_mcp_servers_from_configured(
        HashMap::from([("unregistered".to_string(), server)]),
        &config,
    );
}

#[test]
fn effective_mcp_servers_preserve_chatgpt_auth_for_trusted_origins() {
    for (base_url, server_url) in [
        ("https://chatgpt-staging.com", "https://chatgpt-staging.com"),
        (
            "https://preview.chatgpt-staging.com",
            "https://preview.chatgpt-staging.com",
        ),
        ("https://gov.chatgpt.com", "https://chatgpt.com"),
        (
            "https://chatgpt.com",
            "https://gov.chatgpt.com/backend-api/",
        ),
        ("https://chatgpt.com", "https://GOV.CHATGPT.COM:443"),
    ] {
        let mut config = test_mcp_config(PathBuf::new());
        config.chatgpt_base_url = base_url.to_string();
        let server: McpServerConfig =
            serde_json::from_value(serde_json::json!({"url": server_url, "auth": "chatgpt"}))
                .expect("valid MCP server config");
        let mut catalog = ResolvedMcpCatalog::builder();
        catalog.register(McpServerRegistration::from_config(
            "trusted".to_string(),
            server,
        ));
        config.mcp_server_catalog = catalog.build();

        let effective = effective_mcp_servers(&config);

        assert_eq!(
            effective["trusted"].config().auth,
            McpServerAuth::ChatGpt,
            "backend {base_url}, server {server_url}",
        );
    }
}

#[test]
fn effective_mcp_servers_reject_untrusted_origins_for_chatgpt_auth() {
    for url in [
        "http://gov.chatgpt.com",
        "https://gov.chatgpt.com:8443",
        "https://gov.chatgpt.com.evil.example",
        "https://sub.gov.chatgpt.com",
        "https://gov.chatgpt.com.",
        "https://gov.chatgpt.com@evil.example",
        "https://example.com",
    ] {
        let mut config = test_mcp_config(PathBuf::new());
        config.chatgpt_base_url = url.to_string();
        let server: McpServerConfig =
            serde_json::from_value(serde_json::json!({"url": url, "auth": "chatgpt"}))
                .expect("valid MCP server config");
        let mut catalog = ResolvedMcpCatalog::builder();
        catalog.register(McpServerRegistration::from_config(
            "untrusted".to_string(),
            server,
        ));
        config.mcp_server_catalog = catalog.build();

        let effective = effective_mcp_servers(&config);

        assert_eq!(
            effective["untrusted"].config().auth,
            McpServerAuth::OAuth,
            "server {url}",
        );
    }
}

#[tokio::test]
async fn effective_mcp_servers_preserve_runtime_servers() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let mut config = test_mcp_config(codex_home.path().to_path_buf());

    let mut catalog = ResolvedMcpCatalog::builder();
    catalog.register(McpServerRegistration::from_config(
        "sample".to_string(),
        McpServerConfig {
            auth: Default::default(),
            transport: McpServerTransportConfig::StreamableHttp {
                url: "https://user.example/mcp".to_string(),
                bearer_token_env_var: None,
                http_headers: None,
                env_http_headers: None,
                http_headers_helper: None,
            },
            environment_id: codex_config::DEFAULT_MCP_SERVER_ENVIRONMENT_ID.to_string(),
            enabled: true,
            required: false,
            startup_readiness: Default::default(),
            supports_parallel_tool_calls: false,
            tool_input_schema_max_bytes: None,
            omit_tools_from: None,
            disabled_reason: None,
            startup_timeout_sec: None,
            tool_timeout_sec: None,
            default_tools_approval_mode: None,
            enabled_tools: None,
            disabled_tools: None,
            scopes: None,
            oauth: None,
            oauth_resource: None,
            tools: HashMap::new(),
        },
    ));
    catalog.register(McpServerRegistration::from_config(
        "docs".to_string(),
        McpServerConfig {
            auth: Default::default(),
            transport: McpServerTransportConfig::StreamableHttp {
                url: "https://docs.example/mcp".to_string(),
                bearer_token_env_var: None,
                http_headers: None,
                env_http_headers: None,
                http_headers_helper: None,
            },
            environment_id: codex_config::DEFAULT_MCP_SERVER_ENVIRONMENT_ID.to_string(),
            enabled: true,
            required: false,
            startup_readiness: Default::default(),
            supports_parallel_tool_calls: false,
            tool_input_schema_max_bytes: None,
            omit_tools_from: None,
            disabled_reason: None,
            startup_timeout_sec: None,
            tool_timeout_sec: None,
            default_tools_approval_mode: None,
            enabled_tools: None,
            disabled_tools: None,
            scopes: None,
            oauth: None,
            oauth_resource: None,
            tools: HashMap::new(),
        },
    ));
    config.mcp_server_catalog = catalog.build();

    let effective = effective_mcp_servers(&config);

    let sample = effective.get("sample").expect("user server should exist");
    let docs = effective
        .get("docs")
        .expect("configured server should exist");

    let sample = sample.config();
    let docs = docs.config();

    match &sample.transport {
        McpServerTransportConfig::StreamableHttp { url, .. } => {
            assert_eq!(url, "https://user.example/mcp");
        }
        other => panic!("expected streamable http transport, got {other:?}"),
    }
    match &docs.transport {
        McpServerTransportConfig::StreamableHttp { url, .. } => {
            assert_eq!(url, "https://docs.example/mcp");
        }
        other => panic!("expected streamable http transport, got {other:?}"),
    }
}
