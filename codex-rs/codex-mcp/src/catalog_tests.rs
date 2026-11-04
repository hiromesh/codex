use std::collections::BTreeMap;
use std::collections::HashMap;
use std::time::Duration;

use codex_config::AppToolApproval;
use codex_config::DEFAULT_MCP_SERVER_ENVIRONMENT_ID;
use codex_config::McpServerConfig;
use codex_config::McpServerToolConfig;
use codex_config::McpServerTransportConfig;
use codex_protocol::mcp_policy::EnvironmentMcpPolicy;
use pretty_assertions::assert_eq;

use crate::McpProtocolMode;
use crate::server::McpCredentialPolicy;

use super::McpEnvironmentAuthority;
use super::McpServerConflict;
use super::McpServerConflictAction;
use super::McpServerRegistration;
use super::McpServerSource;
use super::ResolvedMcpCatalog;
use super::ResolvedMcpServer;

fn server(url: &str) -> McpServerConfig {
    McpServerConfig {
        auth: Default::default(),
        transport: McpServerTransportConfig::StreamableHttp {
            url: url.to_string(),
            bearer_token_env_var: None,
            http_headers: None,
            env_http_headers: None,
            http_headers_helper: None,
        },
        environment_id: DEFAULT_MCP_SERVER_ENVIRONMENT_ID.to_string(),
        enabled: true,
        required: true,
        startup_readiness: Default::default(),
        supports_parallel_tool_calls: true,
        tool_input_schema_max_bytes: None,
        omit_tools_from: None,
        disabled_reason: None,
        startup_timeout_sec: Some(Duration::from_secs(7)),
        tool_timeout_sec: Some(Duration::from_secs(11)),
        default_tools_approval_mode: Some(AppToolApproval::Prompt),
        enabled_tools: Some(vec!["read".to_string()]),
        disabled_tools: Some(vec!["write".to_string()]),
        scopes: None,
        oauth: None,
        oauth_resource: None,
        tools: HashMap::from([(
            "read".to_string(),
            McpServerToolConfig {
                approval_mode: Some(AppToolApproval::Approve),
                ..Default::default()
            },
        )]),
    }
}

fn compatibility_source(id: &str) -> McpServerSource {
    McpServerSource::Compatibility { id: id.to_string() }
}

fn extension_source(id: &str) -> McpServerSource {
    McpServerSource::Extension { id: id.to_string() }
}

fn register(source: McpServerSource) -> McpServerConflictAction {
    McpServerConflictAction::Register(source)
}

fn remove(source: McpServerSource) -> McpServerConflictAction {
    McpServerConflictAction::Remove(source)
}

#[test]
fn executor_credential_policy_survives_catalog_rebuilds_and_materialization() {
    let mut config = server("https://executor.example/mcp");
    config.environment_id = "remote".to_string();
    let mut executor = ResolvedMcpCatalog::builder();
    executor.register(McpServerRegistration::from_executor_config(
        "docs".to_string(),
        config.clone(),
    ));
    let executor = executor.build();
    let mut host = ResolvedMcpCatalog::builder();
    host.register(McpServerRegistration::from_config(
        "docs".to_string(),
        config,
    ));
    let host = host.build();

    for catalog in [
        executor.to_builder().build(),
        executor.with_materialized_servers(executor.configured_servers()),
    ] {
        assert_eq!(
            catalog.server("docs").unwrap(),
            executor.server("docs").unwrap(),
        );
        assert_eq!(
            catalog.server("docs").unwrap().credential_policy(),
            McpCredentialPolicy::ExecutorOnly,
        );
        assert!(!catalog.has_same_servers(&host));
    }
    assert_eq!(
        host.server("docs").unwrap().credential_policy(),
        McpCredentialPolicy::HostFallbackAllowed,
    );
}

#[test]
#[should_panic(expected = "materialized MCP server must have a catalog registration")]
fn materialized_server_requires_catalog_registration() {
    let catalog = ResolvedMcpCatalog::default();
    let server: McpServerConfig = serde_json::from_value(serde_json::json!({
        "url": "https://executor.example/mcp",
        "environment_id": "remote",
        "bearer_token_env_var": "MCP_EXECUTOR_CREDENTIAL_CANARY",
    }))
    .expect("valid MCP server config");

    catalog.with_materialized_servers(HashMap::from([("unregistered".to_string(), server)]));
}

#[test]
fn source_precedence_preserves_the_winning_registration() {
    let extension = server("https://extension.example/mcp");
    let mut builder = ResolvedMcpCatalog::builder();
    builder.register(McpServerRegistration::from_extension(
        "docs".to_string(),
        "hosted",
        /*contribution_order*/ 0,
        extension.clone(),
    ));
    builder.register(McpServerRegistration::from_compatibility(
        "docs".to_string(),
        "legacy",
        server("https://compatibility.example/mcp"),
    ));
    builder.register(McpServerRegistration::from_config(
        "docs".to_string(),
        server("https://config.example/mcp"),
    ));

    let catalog = builder.build();
    let resolved = catalog.server("docs").expect("resolved server");

    assert_eq!(
        resolved.source(),
        &McpServerSource::Extension {
            id: "hosted".to_string(),
        }
    );
    assert_eq!(resolved.config(), &extension);
    assert!(catalog.conflicts().is_empty());
}

#[test]
fn disabled_veto_only_disables_the_winning_registration() {
    let extension = server("https://extension.example/mcp");
    let mut expected = extension.clone();
    expected.enabled = false;
    let mut builder = ResolvedMcpCatalog::builder();
    builder.register(McpServerRegistration::from_extension(
        "docs".to_string(),
        "hosted",
        /*contribution_order*/ 0,
        extension,
    ));
    builder.disable("docs".to_string());

    let actual = builder
        .build()
        .server("docs")
        .expect("resolved server")
        .config()
        .clone();

    assert_eq!(actual, expected);
}

#[test]
fn disabled_winner_remains_a_veto_when_the_catalog_is_extended() {
    let mut disabled = server("https://config.example/mcp");
    disabled.enabled = false;
    let mut expected = server("https://extension.example/mcp");
    expected.enabled = false;
    let mut builder = ResolvedMcpCatalog::builder();
    builder.register(McpServerRegistration::from_config(
        "docs".to_string(),
        disabled,
    ));
    let mut builder = builder.build().to_builder();
    builder.register(McpServerRegistration::from_extension(
        "docs".to_string(),
        "hosted",
        /*contribution_order*/ 0,
        server("https://extension.example/mcp"),
    ));

    let resolved = builder.build();

    assert_eq!(
        resolved.server("docs"),
        Some(&super::ResolvedMcpServer {
            source: extension_source("hosted"),
            config: expected,
            credential_policy: McpCredentialPolicy::HostFallbackAllowed,
            protocol_mode: None,
        })
    );
}

#[test]
fn equal_precedence_uses_insertion_order_not_source_identity() {
    let mut builder = ResolvedMcpCatalog::builder();
    builder.register(McpServerRegistration::from_compatibility(
        "docs".to_string(),
        "z-first",
        server("https://first.example/mcp"),
    ));
    builder.register(McpServerRegistration::from_compatibility(
        "docs".to_string(),
        "a-second",
        server("https://second.example/mcp"),
    ));

    let catalog = builder.build();

    assert_eq!(
        catalog.server("docs"),
        Some(&super::ResolvedMcpServer {
            source: compatibility_source("a-second"),
            config: server("https://second.example/mcp"),
            credential_policy: McpCredentialPolicy::HostFallbackAllowed,
            protocol_mode: None,
        })
    );
    let mut builder = catalog.to_builder();
    builder.remove_compatibility("docs".to_string(), "remove-last");

    let catalog = builder.build();

    assert_eq!(catalog.server("docs"), None);
    assert_eq!(
        catalog.conflicts(),
        &[McpServerConflict {
            name: "docs".to_string(),
            outcome: remove(compatibility_source("remove-last")),
            contenders: vec![
                register(compatibility_source("z-first")),
                register(compatibility_source("a-second")),
                remove(compatibility_source("remove-last")),
            ],
        }]
    );
}

#[test]
fn extension_protocol_mode_follows_the_winner_through_materialization() {
    let config = server("https://apps.example/mcp");
    let mut builder = ResolvedMcpCatalog::builder();
    builder.register(
        McpServerRegistration::from_extension(
            "apps".to_string(),
            "loser",
            /*contribution_order*/ 0,
            config.clone(),
        )
        .with_protocol_mode(McpProtocolMode::V20260728),
    );
    builder.register(McpServerRegistration::from_extension(
        "apps".to_string(),
        "winner",
        /*contribution_order*/ 1,
        config.clone(),
    ));
    let without_override = builder.build();
    assert_eq!(
        without_override
            .server("apps")
            .and_then(ResolvedMcpServer::protocol_mode),
        None
    );

    let mut builder = ResolvedMcpCatalog::builder();
    builder.register(
        McpServerRegistration::from_extension(
            "apps".to_string(),
            "winner",
            /*contribution_order*/ 1,
            config,
        )
        .with_protocol_mode(McpProtocolMode::Legacy),
    );
    let with_override = builder.build();
    assert!(!without_override.has_same_servers(&with_override));

    let refreshed = server("https://refreshed.example/mcp");
    let materialized = with_override
        .with_materialized_servers(HashMap::from([("apps".to_string(), refreshed.clone())]));
    assert_eq!(
        materialized.server("apps"),
        Some(&ResolvedMcpServer {
            source: extension_source("winner"),
            config: refreshed,
            credential_policy: McpCredentialPolicy::HostFallbackAllowed,
            protocol_mode: Some(McpProtocolMode::Legacy),
        })
    );
}

#[test]
fn environment_policy_denies_config_servers_with_an_empty_allowlist() {
    let mut builder = ResolvedMcpCatalog::builder();
    builder.register(McpServerRegistration::from_config(
        "configured".to_string(),
        server("https://config.example/mcp"),
    ));
    let unrestricted_policy = EnvironmentMcpPolicy { servers: None };
    let deny_all_policy = EnvironmentMcpPolicy {
        servers: Some(BTreeMap::new()),
    };

    for (policy, expected) in [(&unrestricted_policy, true), (&deny_all_policy, false)] {
        let resolved = builder
            .clone()
            .build_with_environment_authority(|_| McpEnvironmentAuthority::Restricted(policy));
        assert_eq!(
            resolved
                .server("configured")
                .expect("configured server")
                .config()
                .enabled,
            expected
        );
    }
}
