use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use crate::config::Config;
use crate::mcp_tool_exposure::SharedMcpDefinitions;
use codex_config::DEFAULT_MCP_SERVER_ENVIRONMENT_ID;
use codex_config::McpServerConfig;
use codex_exec_server::ExecutorCapabilityDiscoverySnapshot;
use codex_exec_server::ExecutorCapabilityDiscoveryV2Snapshot;
use codex_extension_api::ExtensionData;
use codex_extension_api::ExtensionDataInit;
use codex_extension_api::ExtensionRegistry;
use codex_extension_api::McpServerContribution;
use codex_extension_api::McpServerContributionContext;
use codex_mcp::EffectiveMcpServer;
use codex_mcp::McpConfig;
use codex_mcp::McpEnvironmentAuthority;
use codex_mcp::McpServerRegistration;
use codex_mcp::McpToolCatalogCache;
use codex_mcp::ToolInfo;
use codex_mcp::configured_mcp_servers;
use codex_mcp::effective_mcp_servers;
use codex_protocol::capabilities::SelectedCapabilityRoot;
use codex_protocol::protocol::EnvironmentConfigState;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::TurnEnvironmentSelection;

/// Thread-local policy that excludes all MCP servers, including runtime contributions.
/// Insert into `StartThreadOptions::thread_extension_init` before starting the thread.
pub struct McpServersDisabled;

/// MCP configuration and capability availability derived from the same inputs.
#[derive(Clone)]
pub(crate) struct McpRuntimeProjection {
    pub(crate) config: McpConfig,
}

pub(crate) enum McpEnvironmentScope<'a> {
    /// Controller-level operations without an associated thread.
    HostOnly,
    /// Every thread selection captured for this operation, including unavailable configurations.
    Selected(&'a [TurnEnvironmentSelection]),
}

impl<'a> McpEnvironmentScope<'a> {
    pub(crate) fn authority_for(&self, environment_id: &str) -> McpEnvironmentAuthority<'a> {
        let Self::Selected(selections) = *self else {
            return McpEnvironmentAuthority::Unrestricted;
        };
        let Some(selection) = selections
            .iter()
            .find(|selection| selection.environment_id == environment_id)
        else {
            return if environment_id == DEFAULT_MCP_SERVER_ENVIRONMENT_ID {
                McpEnvironmentAuthority::Unrestricted
            } else {
                McpEnvironmentAuthority::Unavailable
            };
        };
        match &selection.config {
            EnvironmentConfigState::FromThread => McpEnvironmentAuthority::Unrestricted,
            EnvironmentConfigState::Pending | EnvironmentConfigState::Failed(_) => {
                McpEnvironmentAuthority::Unavailable
            }
            EnvironmentConfigState::Ready(config) => config
                .mcp_policy
                .as_ref()
                .map_or(McpEnvironmentAuthority::Unrestricted, |policy| {
                    McpEnvironmentAuthority::Restricted(policy)
                }),
        }
    }
}

pub(crate) struct McpThreadIdentity<'a> {
    pub(crate) auth_changed: bool,
    pub(crate) session_source: &'a SessionSource,
    pub(crate) originator: &'a str,
    pub(crate) environments: McpEnvironmentScope<'a>,
}

enum OrderedMcpOverlay {
    Set(Box<McpServerRegistration>),
    Remove {
        contributor_id: &'static str,
        contribution_order: usize,
        name: String,
    },
}

#[derive(Clone)]
pub struct McpManager {
    connection_pool: codex_mcp::McpConnectionPool,
    extensions: Arc<ExtensionRegistry<Config>>,
    tool_catalog_cache: McpToolCatalogCache,
    shared_definitions: Arc<SharedMcpDefinitions>,
}

impl McpManager {
    pub fn new() -> Self {
        Self::new_with_extensions(codex_extension_api::empty_extension_registry())
    }

    /// Creates a manager that resolves host-installed MCP contributions.
    pub fn new_with_extensions(extensions: Arc<ExtensionRegistry<Config>>) -> Self {
        Self {
            connection_pool: codex_mcp::McpConnectionPool::default(),
            extensions,
            tool_catalog_cache: McpToolCatalogCache::default(),
            shared_definitions: Arc::new(SharedMcpDefinitions::default()),
        }
    }

    pub fn tool_catalog_cache(&self) -> McpToolCatalogCache {
        self.tool_catalog_cache.clone()
    }

    pub(crate) fn connection_pool(&self) -> codex_mcp::McpConnectionPool {
        self.connection_pool.clone()
    }

    pub(crate) fn shared_definitions(&self) -> &SharedMcpDefinitions {
        &self.shared_definitions
    }

    /// Returns the MCP config after applying compatibility built-ins and
    /// runtime-only extension overlays.
    pub async fn runtime_config(&self, config: &Config) -> McpConfig {
        self.runtime_config_with_context(
            McpServerContributionContext::global(config),
            McpEnvironmentScope::HostOnly,
        )
        .await
        .config
    }

    #[allow(clippy::too_many_arguments)]
    #[tracing::instrument(name = "mcp.runtime_config.project_for_step", skip_all)]
    pub(crate) async fn runtime_config_for_step(
        &self,
        config: &Config,
        thread_init: &ExtensionDataInit,
        thread_store: &ExtensionData,
        identity: McpThreadIdentity<'_>,
        ready_selected_capability_roots: &[SelectedCapabilityRoot],
        executor_capability_discovery: Option<&ExecutorCapabilityDiscoverySnapshot>,
        executor_capability_discovery_v2: Option<&[ExecutorCapabilityDiscoveryV2Snapshot]>,
    ) -> McpRuntimeProjection {
        let mut context = McpServerContributionContext::for_step(
            config,
            thread_init,
            thread_store,
            identity.originator,
            ready_selected_capability_roots,
            executor_capability_discovery,
            executor_capability_discovery_v2,
        )
        .with_session_source(identity.session_source)
        .with_auth_changed(identity.auth_changed);
        if let McpEnvironmentScope::Selected(environments) = &identity.environments {
            context = context.with_selected_environments(environments);
        }
        self.runtime_config_with_context(context, identity.environments)
        .await
    }

    async fn runtime_config_with_context(
        &self,
        context: McpServerContributionContext<'_, Config>,
        environment_scope: McpEnvironmentScope<'_>,
    ) -> McpRuntimeProjection {
        let config = context.config();
        if context
            .thread_init()
            .is_some_and(|init| init.get::<McpServersDisabled>().is_some())
        {
            let mut mcp_config = config.to_mcp_config();
            mcp_config.mcp_server_catalog = Default::default();
            return McpRuntimeProjection { config: mcp_config };
        }
        let mut overlays = Vec::new();
        // A contributor can emit multiple ordered actions, so order each action globally rather
        // than enumerating contributors.
        let mut contribution_order = 0;
        for contributor in self.extensions.mcp_server_contributors() {
            for contribution in contributor.contribute(context).await {
                match contribution {
                    McpServerContribution::Set { name, config } => {
                        overlays.push(OrderedMcpOverlay::Set(Box::new(
                            McpServerRegistration::from_extension(
                                name,
                                contributor.id(),
                                contribution_order,
                                *config,
                            ),
                        )));
                    }
                    McpServerContribution::SetWithProtocolMode {
                        name,
                        config,
                        protocol_mode,
                    } => {
                        overlays.push(OrderedMcpOverlay::Set(Box::new(
                            McpServerRegistration::from_extension(
                                name,
                                contributor.id(),
                                contribution_order,
                                *config,
                            )
                            .with_protocol_mode(protocol_mode),
                        )));
                    }
                    McpServerContribution::Remove { name } => {
                        overlays.push(OrderedMcpOverlay::Remove {
                            contributor_id: contributor.id(),
                            contribution_order,
                            name,
                        });
                    }
                }
                contribution_order += 1;
            }
        }

        let mut mcp_config = config.to_mcp_config();
        let mut catalog = mcp_config.mcp_server_catalog.to_builder();

        for overlay in overlays {
            match overlay {
                OrderedMcpOverlay::Set(registration) => catalog.register(*registration),
                OrderedMcpOverlay::Remove {
                    contributor_id,
                    contribution_order,
                    name,
                } => catalog.remove_extension(name, contributor_id, contribution_order),
            }
        }
        let catalog = catalog.build_with_environment_authority(|environment_id| {
            environment_scope.authority_for(environment_id)
        });
        for conflict in catalog.conflicts() {
            tracing::warn!(
                server = conflict.name,
                outcome = ?conflict.outcome,
                contenders = ?conflict.contenders,
                "conflicting MCP server actions; using resolved catalog outcome"
            );
        }
        mcp_config.mcp_server_catalog = catalog;
        McpRuntimeProjection { config: mcp_config }
    }

    /// Returns config-backed servers without runtime contributions.
    pub async fn configured_servers(&self, config: &Config) -> HashMap<String, McpServerConfig> {
        let mcp_config = config.to_mcp_config();
        configured_mcp_servers(&mcp_config)
    }

    /// Returns configured and host-contributed servers before auth gating.
    pub async fn runtime_servers(&self, config: &Config) -> HashMap<String, McpServerConfig> {
        let mcp_config = self.runtime_config(config).await;
        configured_mcp_servers(&mcp_config)
    }

    /// Returns runtime servers after auth gating and compatibility built-ins.
    pub async fn effective_servers(&self, config: &Config) -> HashMap<String, EffectiveMcpServer> {
        let mcp_config = self.runtime_config(config).await;
        effective_mcp_servers(&mcp_config)
    }
}
