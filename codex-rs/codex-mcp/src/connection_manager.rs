//! Aggregates MCP server connections for Codex.
//!
//! [`McpConnectionSet`] is the private connection set behind
//! [`crate::McpRuntime`] and [`crate::McpBinding`]. It coordinates startup status
//! events, keeps server metadata, and aggregates tools and resources across
//! running RMCP clients.

#[path = "connection_manager/catalog_telemetry.rs"]
mod catalog_telemetry;
#[path = "connection_manager/network_policy.rs"]
mod network_policy;
#[path = "connection_manager/required.rs"]
mod required;
pub use network_policy::NetworkPolicyMetadata;
pub use network_policy::NetworkPolicyMetadataProvider;
#[path = "connection_manager/resources.rs"]
mod resources;
#[path = "connection_manager/startup.rs"]
mod startup;
#[path = "connection_manager/status.rs"]
mod status;
#[path = "connection_manager/tool_catalog.rs"]
mod tool_catalog;

use startup::chatgpt_auth_provider_for_server;
use startup::emit_update;
use startup::mcp_init_error_display;
use startup::mcp_startup_failure_reason;
pub(crate) use tool_catalog::BindingCatalogRevision;
pub use tool_catalog::tool_is_model_visible;

use std::cell::LazyCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::McpProtocolMode;
use crate::binding::call_tool_result_from_rmcp;
use crate::connection_pool::ConnectionKey;
use crate::connection_pool::ConnectionSharing;
use crate::elicitation::ElicitationRequestManager;
use crate::elicitation::ElicitationRequestRouter;
use crate::event_stream::EventStreamConnectionSettings;
use crate::pagination::MAX_MCP_CATALOG_ITEMS;
use crate::rmcp_client::AsyncManagedClient;
use crate::rmcp_client::DEFAULT_STARTUP_TIMEOUT;
use crate::rmcp_client::DEFAULT_TOOL_TIMEOUT;
use crate::rmcp_client::ManagedClient;
use crate::rmcp_client::StartupOutcomeError;
use crate::runtime::McpPublicationGate;
use crate::runtime::McpRuntimeInput;
use crate::runtime::McpStartupPolicy;
use crate::server::McpServerConnectionIdentity;
use crate::server::McpServerMetadata;
use crate::tools::ToolFilter;
use crate::tools::ToolInfo;
use crate::tools::filter_tools;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;
use codex_config::McpServerTransportConfig;
use codex_config::McpStartupReadiness;
use codex_diagnostics::Gauge;
use codex_diagnostics::GaugeGuard;
use codex_otel::auth_storage::AuthStorageOriginator;
use codex_protocol::mcp::CallToolResult;
use codex_protocol::mcp::McpServerInfo;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::McpStartupCompleteEvent;
use codex_protocol::protocol::McpStartupFailure;
use codex_protocol::protocol::McpStartupFailureReason;
use codex_protocol::protocol::McpStartupStatus;
use codex_protocol::protocol::McpStartupUpdateEvent;
use codex_rmcp_client::determine_streamable_http_auth_status_from_credentials;
use futures::FutureExt;
use futures::future::join_all;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tracing::warn;

static LIVE_CONNECTIONS: Gauge = Gauge::new("mcp.connections.live");

pub(crate) struct McpServerConnection {
    identity: Option<McpServerConnectionIdentity>,
    client: AsyncManagedClient,
    legacy_fallback: Option<Arc<OnceLock<Arc<Self>>>>,
    // Startup-only budget; changing it must not replace a ready connection.
    startup_timeout: Duration,
    startup_trigger: Option<watch::Sender<bool>>,
    _diagnostics_guard: GaugeGuard,
}

impl McpServerConnection {
    fn retained_legacy(&self) -> Option<&Arc<Self>> {
        self.legacy_fallback.as_deref().and_then(OnceLock::get)
    }

    fn may_use_legacy_fallback(&self) -> bool {
        self.legacy_fallback
            .as_ref()
            .is_some_and(|fallback| fallback.get().is_some() || self.client.client.peek().is_none())
    }

    /// Keep the working private connection if a singleton cannot start a second process.
    fn retain_on_failed_discovery(
        self: &Arc<Self>,
        replacement: &mut AsyncManagedClient,
    ) -> Arc<OnceLock<Arc<Self>>> {
        let previous = Arc::clone(self.retained_legacy().unwrap_or(self));
        let fallback = Arc::new(OnceLock::new());
        let selected = Arc::clone(&fallback);
        let cancellation = replacement.cancel_token.clone();
        replacement.client = replacement
            .client
            .clone()
            .then(move |outcome| async move {
                let Err(error @ StartupOutcomeError::Failed { .. }) = &outcome else {
                    return outcome;
                };
                if error.is_authentication_required() {
                    return outcome;
                }
                error.wait_for_shutdown().await;
                let Some(client) = previous.client.ready_client() else {
                    return outcome;
                };
                if client.client.is_closed().await {
                    return outcome;
                }
                if cancellation.is_cancelled() || previous.client.cancel_token.is_cancelled() {
                    return Err(StartupOutcomeError::Cancelled);
                }
                // The connection owns the startup token and the capabilities for this transport.
                selected.get_or_init(|| previous);
                Ok(client)
            })
            .boxed()
            .shared();
        fallback
    }

    async fn reusable_client(
        &self,
        desired: &McpServerConnectionIdentity,
    ) -> Option<ManagedClient> {
        let current = self.identity.as_ref()?;
        if !current.has_same_connection_config(desired) {
            return None;
        }
        if !self.client.startup_complete.load(Ordering::Acquire) {
            return None;
        }
        let client = self.client.client().await.ok()?;
        if client.client.is_closed().await {
            return None;
        }
        if current == desired {
            if matches!(desired.oauth_credentials(), Ok(None))
                && tokio::time::timeout(Duration::ZERO, client.client.managed_oauth_credentials())
                    .await
                    .is_ok_and(|credentials| matches!(credentials, Some(Some(_))))
            {
                return None;
            }
            return Some(client);
        }
        let Ok(desired_credentials) = desired.oauth_credentials() else {
            return Some(client);
        };
        let reusable = match client.client.managed_oauth_credentials().await {
            Some(live_credentials) => live_credentials.as_ref() == desired_credentials,
            None => current
                .oauth_credentials()
                .is_ok_and(|startup_credentials| startup_credentials == desired_credentials),
        };
        if reusable { Some(client) } else { None }
    }

    pub(crate) async fn client(&self) -> Result<ManagedClient, StartupOutcomeError> {
        if let Some(startup_trigger) = &self.startup_trigger {
            startup_trigger.send_replace(true);
        }
        self.client.client().await
    }

    async fn client_for_call(&self) -> Result<ManagedClient, StartupOutcomeError> {
        self.client().await
    }

    async fn shutdown(&self) {
        self.client.shutdown().await;
    }

    fn cancel_startup(&self) {
        if !self.startup_is_dormant() && !self.client.startup_complete.load(Ordering::Acquire) {
            self.client.cancel_token.cancel();
        }
    }

    fn startup_is_dormant(&self) -> bool {
        self.startup_trigger
            .as_ref()
            .is_some_and(|startup_trigger| !*startup_trigger.borrow())
    }
}

impl Drop for McpServerConnection {
    fn drop(&mut self) {
        self.client.cancel_token.cancel();
    }
}

#[derive(Clone)]
struct McpServerView {
    connection: Arc<McpServerConnection>,
    protocol_mode: McpProtocolMode,
    startup_readiness: McpStartupReadiness,
    metadata: McpServerMetadata,
    tool_filter: ToolFilter,
    tool_timeout: Option<Duration>,
    catalog_item_limit: usize,
    refresh_tool_list: bool,
    refresh_timeout: Duration,
}

impl McpServerView {
    fn allows_cached_startup(&self) -> bool {
        self.startup_readiness == McpStartupReadiness::Catalog
            || self.connection.startup_is_dormant()
    }

    fn cached_startup_tools(&self, fallback: Option<Vec<ToolInfo>>) -> Option<Vec<ToolInfo>> {
        self.connection
            .client
            .cached_tools_or(fallback)
            .filter(|tools| self.accepts_cached_tools(tools))
    }

    fn accepts_cached_tools(&self, tools: &[ToolInfo]) -> bool {
        match self.startup_readiness {
            McpStartupReadiness::Connection => !tools.is_empty(),
            McpStartupReadiness::Catalog => tools.iter().any(|tool| {
                self.tool_filter.allows(&tool.tool.name)
                    && (self.connection.startup_is_dormant() || tool_is_model_visible(tool))
            }),
        }
    }

    async fn refresh_notified_catalog(&self, server_name: &str, client: &ManagedClient) {
        if !self.refresh_tool_list {
            return;
        }
        let timeout = self.refresh_timeout;
        let result = client
            .tool_catalog
            .refresh_after_notifications(&client.client.tool_list_revision(), timeout, || async {
                let relist = crate::rmcp_client::list_tools_for_client_uncached(
                    server_name,
                    &client.client,
                    Some(timeout),
                    self.catalog_item_limit,
                    client.server_instructions.as_deref(),
                );
                client.with_active_shared_lease(relist.boxed()).await?
            })
            .await;
        if result.is_err() {
            tracing::warn!(
                server_name,
                "MCP tool catalog refresh failed; retaining the previous catalog"
            );
        }
    }

    async fn listed_catalog(
        &self,
        server_name: &str,
    ) -> Result<(Vec<ToolInfo>, Option<Arc<serde_json::Value>>), StartupOutcomeError> {
        let (tools, meta) = if let Some(client) = self.connection.client.ready_client() {
            self.refresh_notified_catalog(server_name, &client).await;
            client.listed_catalog().await
        } else {
            self.connection.client.listed_catalog().await?
        };
        let tools = filter_tools(tools, &self.tool_filter);
        Ok((tools, meta))
    }
}

pub(crate) struct PreviousMcpConnections<'a> {
    pub(crate) connections: &'a McpConnectionSet,
    pub(crate) same_owner: bool,
}

/// A published view over a set of running MCP server connections.
pub(crate) struct McpConnectionSet {
    config: Option<Arc<crate::McpConfig>>,
    network_policy_metadata: Option<Arc<dyn NetworkPolicyMetadataProvider>>,
    servers: HashMap<String, McpServerView>,
    event_stream_connections: HashMap<String, Arc<EventStreamConnectionSettings>>,
    disabled_servers: Vec<String>,
    required_servers: Vec<String>,
    optional_startup_deadline: OnceLock<tokio::time::Instant>,
    prefix_mcp_tool_names: bool,
    non_prefixed_mcp_tool_servers: Vec<String>,
    elicitation_requests: ElicitationRequestManager,
}

impl McpConnectionSet {
    /// Creates an MCP connection manager. Threadless callers can pass no `tx_event`; startup
    /// notifications are then skipped and interactive elicitations are declined.
    pub async fn new(
        previous: Option<PreviousMcpConnections<'_>>,
        publication_gate: McpPublicationGate,
        input: McpRuntimeInput,
        elicitation_router: ElicitationRequestRouter,
    ) -> Self {
        let same_owner = previous.as_ref().is_some_and(|state| state.same_owner);
        let previous = previous.map(|state| state.connections);
        let McpRuntimeInput {
            startup_policy,
            config,
            network_policy_metadata,
            ready_selected_capability_roots: _,
            mcp_servers,
            submit_id,
            tx_event,
            startup_cancellation_token,
            runtime_context,
            tool_catalog_cache,
            client_mcp_extensions,
            auth,
            auth_manager,
            elicitation_reviewer,
            elicitation_lifecycle,
        } = input;
        let store_mode = config.mcp_oauth_credentials_store_mode;
        let keyring_backend_kind = config.auth_keyring_backend_kind;
        let oauth_refresh_mode = config.oauth_refresh_mode;
        let prefix_mcp_tool_names = config.prefix_mcp_tool_names;
        let non_prefixed_mcp_tool_servers = config.non_prefixed_mcp_tool_servers.clone();
        let default_protocol_mode = config.protocol_mode;
        let client_elicitation_capability = config.client_elicitation_capability.clone();
        let auth = auth.as_ref();
        let mut servers = HashMap::new();
        let mut event_stream_connections = HashMap::new();
        let disabled_servers = mcp_servers
            .iter()
            .filter(|(_, server)| !server.enabled())
            .map(|(name, _)| name.clone())
            .collect();
        let mut required_servers = mcp_servers
            .iter()
            .filter(|(_, server)| server.enabled() && server.required())
            .map(|(server_name, _)| server_name.clone())
            .collect::<Vec<_>>();
        required_servers.sort();
        let mut reused_ready = Vec::new();
        let mut join_set = JoinSet::new();
        // Explicit reconnects have no previous set and must replace their clients eagerly.
        let allow_deferred_startup =
            startup_policy == McpStartupPolicy::LazyWhenCached && previous.is_some();
        // Retained clients keep callbacks attached to this manager even when
        // an explicit reconnect replaces the ordinary clients.
        let elicitation_previous = previous
            .filter(|previous| {
                !previous.servers.is_empty()
                    && previous.elicitation_requests.update(
                        Arc::clone(&config),
                        elicitation_reviewer.clone(),
                        elicitation_lifecycle.clone(),
                    )
            });
        let reusable_previous = previous.filter(|_| elicitation_previous.is_some());
        let elicitation_requests = if let Some(previous) = elicitation_previous {
            previous.elicitation_requests.clone()
        } else {
            ElicitationRequestManager::new(
                Arc::clone(&config),
                elicitation_reviewer,
                elicitation_lifecycle,
                elicitation_router,
            )
        };
        let startup_submit_id = submit_id;
        let static_chatgpt_auth_provider =
            auth.map(codex_model_provider::auth_provider_from_auth);
        for (server_name, server) in mcp_servers
            .into_iter()
            .filter(|(_, server)| server.enabled())
        {
            let server = server.with_read_only_mcp_tools(config.requires_read_only_mcp_tools);
            let registration = config.mcp_server_catalog.server(&server_name);
            let client_mcp_extensions = crate::client_capabilities::server_mcp_extensions(
                &client_mcp_extensions,
                registration,
                network_policy_metadata.is_some(),
                &config.chatgpt_base_url,
            );
            let catalog_item_limit = MAX_MCP_CATALOG_ITEMS;
            let metadata = McpServerMetadata::from(&server);
            let configured_config = server.config().clone();
            let protocol_mode = if matches!(
                &configured_config.transport,
                McpServerTransportConfig::StreamableHttp { .. }
            ) {
                registration
                    .and_then(crate::ResolvedMcpServer::protocol_mode)
                    .unwrap_or(default_protocol_mode)
            } else {
                default_protocol_mode
            };
            let configured_tool_filter = ToolFilter::from_config(&configured_config);
            let startup_timeout = configured_config
                .startup_timeout_sec
                .unwrap_or(DEFAULT_STARTUP_TIMEOUT);
            let configured_tool_timeout = Some(
                configured_config
                    .tool_timeout_sec
                    .unwrap_or(DEFAULT_TOOL_TIMEOUT),
            );
            let resolved_environment =
                runtime_context.resolve_server_environment(&server_name, &configured_config);
            let runtime_auth_provider =
                chatgpt_auth_provider_for_server(&server, static_chatgpt_auth_provider.clone());
            let discover_local_stdio = config.discover_local_stdio
                && server.can_discover_local_stdio(runtime_auth_provider.as_ref());
            let protocol_mode = if discover_local_stdio {
                McpProtocolMode::DiscoverLocalStdio
            } else {
                protocol_mode
            };
            let connection_identity = McpServerConnectionIdentity::new(
                &server_name,
                &server,
                store_mode,
                keyring_backend_kind,
                oauth_refresh_mode,
                &resolved_environment,
                &runtime_context,
                runtime_auth_provider.as_ref(),
                auth,
                client_elicitation_capability.clone(),
                client_mcp_extensions.clone(),
                previous
                    .and_then(|previous| previous.servers.get(&server_name))
                    .and_then(|view| view.connection.identity.as_ref()),
            );
            let expected_protocol_mode = match &configured_config.transport {
                McpServerTransportConfig::StreamableHttp { .. } => Some(protocol_mode),
                McpServerTransportConfig::Stdio { .. } => server.stdio_protocol_mode(protocol_mode),
            };
            let mut tool_catalog_cache_context = LazyCell::new(|| {
                if server.requires_read_only_mcp_tools() {
                    return None;
                }
                let environment = resolved_environment.as_ref().ok()?;
                let cache_mode = match expected_protocol_mode? {
                    McpProtocolMode::DiscoverLocalStdio | McpProtocolMode::Legacy => {
                        McpProtocolMode::Legacy
                    }
                    McpProtocolMode::V20260728 => McpProtocolMode::V20260728,
                };
                let mut context = tool_catalog_cache.context(
                    &server_name,
                    &configured_config,
                    &runtime_context,
                    environment.as_ref(),
                    (&client_elicitation_capability, &client_mcp_extensions),
                    Some((&connection_identity, cache_mode)),
                )?;
                context.track_tool_list_changes = config.mcp_tool_list_changed;
                Some(context)
            });
            let defer_startup = allow_deferred_startup
                && tool_catalog_cache_context
                    .as_ref()
                    .and_then(|context| {
                        context.current_revision_if(|tools| {
                            tools
                                .iter()
                                .any(|tool| configured_tool_filter.allows(&tool.tool.name))
                        })
                    })
                    .is_some();
            // A subagent using legacy tool definitions must launch with the same handshake.
            let expected_protocol_mode = if defer_startup
                && expected_protocol_mode == Some(McpProtocolMode::DiscoverLocalStdio)
            {
                Some(McpProtocolMode::Legacy)
            } else {
                expected_protocol_mode
            };
            let protocol_mode = expected_protocol_mode.unwrap_or(protocol_mode);
            event_stream_connections.insert(
                server_name.clone(),
                Arc::new(EventStreamConnectionSettings {
                    server_name: server_name.clone(),
                    server: server.clone(),
                    store_mode,
                    keyring_backend_kind,
                    oauth_refresh_mode,
                    runtime_context: runtime_context.clone(),
                    resolved_environment: resolved_environment.clone(),
                    auth_provider: runtime_auth_provider.clone(),
                    auth_manager: auth_manager.clone(),
                    auth: auth.cloned(),
                    protocol_mode,
                    client_mcp_extensions: client_mcp_extensions.clone(),
                }),
            );
            let connection_pool = runtime_context.connection_pool.as_ref().filter(|_| {
                // Sharing a signed-in helper requires tracking account changes.
                discover_local_stdio
                    && protocol_mode != McpProtocolMode::Legacy
                    && (auth.is_none() || auth_manager.is_some())
            });
            let mut legacy_to_upgrade = None;
            if let Some(previous_view) =
                reusable_previous.and_then(|previous| previous.servers.get(&server_name))
                && previous_view.refresh_tool_list == config.mcp_tool_list_changed
                && (same_owner || !previous_view.connection.may_use_legacy_fallback())
            {
                let connection = Arc::clone(&previous_view.connection);
                // A failed pool acquisition can leave this chat on its private helper.
                let retained_legacy = connection.retained_legacy().is_some();
                let can_reuse = retained_legacy
                    || connection.client.connection_slot.is_some() == connection_pool.is_some();
                let reusable_pending_startup = connection.identity.as_ref()
                    == Some(&connection_identity)
                    && !connection.client.startup_complete.load(Ordering::Acquire)
                    && connection.startup_timeout == startup_timeout
                    && !connection.startup_is_dormant()
                    && !connection.client.cancel_token.is_cancelled()
                    && previous_view.catalog_item_limit == catalog_item_limit
                    && expected_protocol_mode.is_some()
                    && previous_view.protocol_mode == protocol_mode;
                let unchanged_auth_failure = if connection.identity.as_ref()
                    == Some(&connection_identity)
                    && connection_identity.oauth_store_was_contended
                    && previous_view.protocol_mode == protocol_mode
                    && connection.client.startup_complete.load(Ordering::Acquire)
                {
                    connection
                        .client()
                        .await
                        .err()
                        .filter(StartupOutcomeError::is_authentication_required)
                } else {
                    None
                };
                let ready = connection
                    .reusable_client(&connection_identity)
                    .await
                    .filter(|_| previous_view.catalog_item_limit == catalog_item_limit);
                if can_reuse
                    && (reusable_pending_startup
                        || unchanged_auth_failure.is_some()
                        || ready.as_ref().is_some_and(|client| {
                            expected_protocol_mode.is_some_and(|expected| {
                                client.client.protocol_mode() == expected
                                    || (previous_view.protocol_mode == expected && retained_legacy)
                            })
                        }))
                {
                    let pending_client =
                        reusable_pending_startup.then(|| connection.client.clone());
                    servers.insert(
                        server_name.clone(),
                        McpServerView {
                            connection,
                            protocol_mode,
                            startup_readiness: configured_config.startup_readiness,
                            refresh_tool_list: config.mcp_tool_list_changed,
                            refresh_timeout: startup_timeout,
                            metadata,
                            tool_filter: configured_tool_filter,
                            tool_timeout: configured_tool_timeout,
                            catalog_item_limit,
                        },
                    );
                    if let Some(error) = unchanged_auth_failure {
                        let reason = connection_identity
                            .oauth_credentials()
                            .ok()
                            .flatten()
                            .map(|_| McpStartupFailureReason::ReauthenticationRequired);
                        let status = McpStartupStatus::Failed {
                            error: mcp_init_error_display(
                                &server_name,
                                Some(&configured_config),
                                &error,
                                reason,
                            ),
                            reason,
                        };
                        let tx_event = tx_event.clone();
                        let submit_id = startup_submit_id.clone();
                        let publication_gate = publication_gate.clone();
                        join_set.spawn(async move {
                            if !publication_gate.wait().await {
                                return (server_name, Err(StartupOutcomeError::Cancelled));
                            }
                            if let Some(tx_event) = tx_event.as_ref() {
                                for status in [McpStartupStatus::Starting, status] {
                                    let _ = emit_update(
                                        submit_id.as_str(),
                                        tx_event,
                                        McpStartupUpdateEvent {
                                            server: server_name.clone(),
                                            status,
                                        },
                                    )
                                    .await;
                                }
                            }
                            (server_name, Err(error))
                        });
                    } else if let Some(client) = pending_client {
                        let publication_gate = publication_gate.clone();
                        join_set.spawn(async move {
                            if !publication_gate.wait().await {
                                return (server_name, Err(StartupOutcomeError::Cancelled));
                            }
                            (server_name, client.client().await)
                        });
                    } else {
                        reused_ready.push(server_name);
                    }
                    continue;
                }
                if same_owner
                    && expected_protocol_mode == Some(McpProtocolMode::DiscoverLocalStdio)
                    && previous_view.protocol_mode == McpProtocolMode::Legacy
                    && connection.identity.as_ref() == Some(&connection_identity)
                    && !connection.client.cancel_token.is_cancelled()
                    && ready.is_some_and(|client| {
                        client.client.protocol_mode() == McpProtocolMode::Legacy
                            && matches!(client.connection_sharing, ConnectionSharing::Private)
                    })
                {
                    legacy_to_upgrade = Some(connection);
                }
            }
            let cancel_token = startup_cancellation_token.child_token();
            let tool_catalog_cache_context = tool_catalog_cache_context.take();
            let has_runtime_auth = runtime_auth_provider.is_some();
            let connection_pool = connection_pool.map(|pool| {
                (
                    pool.clone(),
                    ConnectionKey {
                        server_name: server_name.clone(),
                        identity: connection_identity.clone(),
                        auth_state: auth_manager.as_ref().map(|manager| {
                            (
                                Arc::clone(manager),
                                *manager.auth_change_state_receiver().borrow(),
                            )
                        }),
                        startup_timeout,
                        catalog_item_limit,
                    },
                )
            });
            let mut async_managed_client = AsyncManagedClient::new(
                server_name.clone(),
                server,
                store_mode,
                keyring_backend_kind,
                oauth_refresh_mode,
                cancel_token.clone(),
                tx_event.clone(),
                elicitation_requests.clone(),
                tool_catalog_cache_context,
                runtime_context.clone(),
                resolved_environment,
                runtime_auth_provider,
                client_elicitation_capability.clone(),
                client_mcp_extensions.clone(),
                auth_manager
                    .as_ref()
                    .filter(|_| {
                        matches!(
                            &configured_config.transport,
                            McpServerTransportConfig::Stdio { .. }
                        )
                    })
                    .map(|manager| manager.auth_change_state_receiver()),
                protocol_mode,
                catalog_item_limit,
                connection_pool,
            );
            let legacy_fallback = legacy_to_upgrade
                .map(|connection| connection.retain_on_failed_discovery(&mut async_managed_client));
            let (startup_trigger, startup_receiver) = if defer_startup {
                let (trigger, receiver) = watch::channel(false);
                (Some(trigger), Some(receiver))
            } else {
                (None, None)
            };
            servers.insert(
                server_name.clone(),
                McpServerView {
                    connection: Arc::new(McpServerConnection {
                        identity: Some(connection_identity),
                        client: async_managed_client.clone(),
                        legacy_fallback,
                        startup_timeout,
                        startup_trigger,
                        _diagnostics_guard: LIVE_CONNECTIONS.track(),
                    }),
                    protocol_mode,
                    startup_readiness: configured_config.startup_readiness,
                    refresh_tool_list: config.mcp_tool_list_changed,
                    refresh_timeout: startup_timeout,
                    metadata,
                    tool_filter: configured_tool_filter,
                    tool_timeout: configured_tool_timeout,
                    catalog_item_limit,
                },
            );
            let tx_event = tx_event.clone();
            let submit_id = startup_submit_id.clone();
            let publication_gate = publication_gate.clone();
            let startup = async move {
                if let Some(mut startup_receiver) = startup_receiver
                    && tokio::select! {
                        started = startup_receiver.wait_for(|started| *started) => started.is_err(),
                        () = cancel_token.cancelled() => true,
                    }
                {
                    return (server_name, Err(StartupOutcomeError::Cancelled));
                }
                if !publication_gate.wait().await {
                    return (server_name, Err(StartupOutcomeError::Cancelled));
                }
                if let Some(tx_event) = tx_event.as_ref() {
                    let _ = emit_update(
                        submit_id.as_str(),
                        tx_event,
                        McpStartupUpdateEvent {
                            server: server_name.clone(),
                            status: McpStartupStatus::Starting,
                        },
                    )
                    .await;
                }
                let mut outcome = async_managed_client.client().await;
                if cancel_token.is_cancelled() {
                    outcome = Err(StartupOutcomeError::Cancelled);
                }
                if let Some(tx_event) = tx_event.as_ref() {
                    let auth_state = match &outcome {
                        Err(error)
                            if error.is_authentication_required() && !has_runtime_auth =>
                        {
                            match &configured_config.transport {
                                McpServerTransportConfig::StreamableHttp {
                                    url,
                                    bearer_token_env_var,
                                    http_headers,
                                    env_http_headers,
                                    ..
                                } => {
                                    match determine_streamable_http_auth_status_from_credentials(
                                        configured_config
                                            .oauth_credential_name(&server_name)
                                            .as_ref(),
                                        url,
                                        bearer_token_env_var.as_deref(),
                                        http_headers.clone(),
                                        env_http_headers.clone(),
                                        store_mode,
                                        keyring_backend_kind,
                                    ) {
                                        Ok(auth_state) => auth_state,
                                        Err(error) => {
                                            warn!(
                                                "failed to read stored auth status for MCP server `{server_name}`: {error:?}"
                                            );
                                            None
                                        }
                                    }
                                }
                                McpServerTransportConfig::Stdio { .. } => None,
                            }
                        }
                        Ok(_) | Err(_) => None,
                    };
                    if cancel_token.is_cancelled() {
                        outcome = Err(StartupOutcomeError::Cancelled);
                    }
                    let status = match &outcome {
                        Ok(_) => McpStartupStatus::Ready,
                        Err(StartupOutcomeError::Cancelled) => McpStartupStatus::Cancelled,
                        Err(error) => {
                            let reason = mcp_startup_failure_reason(auth_state, error);
                            let error_str = mcp_init_error_display(
                                server_name.as_str(),
                                Some(&configured_config),
                                error,
                                reason,
                            );
                            McpStartupStatus::Failed {
                                error: error_str,
                                reason,
                            }
                        }
                    };

                    let _ = emit_update(
                        submit_id.as_str(),
                        tx_event,
                        McpStartupUpdateEvent {
                            server: server_name.clone(),
                            status,
                        },
                    )
                    .await;
                }
                if cancel_token.is_cancelled() {
                    outcome = Err(StartupOutcomeError::Cancelled);
                }

                (server_name, outcome)
            };
            let startup = AuthStorageOriginator::current().scope(startup);
            if defer_startup {
                // Dormant servers must not hold the initial startup summary open.
                tokio::spawn(startup);
            } else {
                join_set.spawn(startup);
            }
        }
        let manager = Self {
            config: Some(config),
            network_policy_metadata,
            servers,
            event_stream_connections,
            disabled_servers,
            required_servers,
            optional_startup_deadline: OnceLock::new(),
            prefix_mcp_tool_names,
            non_prefixed_mcp_tool_servers,
            elicitation_requests: elicitation_requests.clone(),
        };
        let summary_publication_gate = publication_gate;
        tokio::spawn(async move {
            let outcomes = join_set.join_all().await;
            if let Some(tx_event) = tx_event {
                if !summary_publication_gate.wait().await {
                    return;
                }
                let mut summary = McpStartupCompleteEvent {
                    ready: reused_ready,
                    ..Default::default()
                };
                for server_name in &summary.ready {
                    let _ = emit_update(
                        startup_submit_id.as_str(),
                        &tx_event,
                        McpStartupUpdateEvent {
                            server: server_name.clone(),
                            status: McpStartupStatus::Ready,
                        },
                    )
                    .await;
                }
                for (server_name, outcome) in outcomes {
                    match outcome {
                        Ok(_) => summary.ready.push(server_name),
                        Err(StartupOutcomeError::Cancelled) => summary.cancelled.push(server_name),
                        Err(StartupOutcomeError::Failed { error, .. }) => {
                            summary.failed.push(McpStartupFailure {
                                server: server_name,
                                error,
                            })
                        }
                    }
                }
                let _ = tx_event
                    .send(Event {
                        id: startup_submit_id,
                        msg: EventMsg::McpStartupComplete(summary),
                    })
                    .await;
            }
        });
        manager
    }

    pub fn empty(prefix_mcp_tool_names: bool) -> Self {
        Self {
            config: None,
            network_policy_metadata: None,
            servers: HashMap::new(),
            event_stream_connections: HashMap::new(),
            disabled_servers: Vec::new(),
            required_servers: Vec::new(),
            optional_startup_deadline: OnceLock::new(),
            prefix_mcp_tool_names,
            non_prefixed_mcp_tool_servers: Vec::new(),
            elicitation_requests: ElicitationRequestManager::default(),
        }
    }

    pub fn has_servers(&self) -> bool {
        !self.servers.is_empty()
    }

    pub(crate) fn contains_server(&self, server_name: &str) -> bool {
        self.servers.contains_key(server_name)
    }

    pub(crate) fn event_stream_connection(
        &self,
        server_name: &str,
    ) -> Option<&Arc<EventStreamConnectionSettings>> {
        self.event_stream_connections.get(server_name)
    }

    pub(crate) async fn authentication_failed_servers(&self) -> Vec<String> {
        let mut failed_servers = Vec::new();
        for (server_name, view) in &self.servers {
            if view
                .connection
                .client
                .startup_complete
                .load(Ordering::Acquire)
                && let Err(error) = view.connection.client().await
                && error.is_authentication_required()
            {
                failed_servers.push(server_name.clone());
            }
        }
        failed_servers
    }

    pub(crate) async fn updated_oauth_credentials_after_auth_failure(
        &self,
        config: &crate::McpConfig,
    ) -> Vec<String> {
        let mut candidates = Vec::new();
        for server_name in self.authentication_failed_servers().await {
            if let Some(view) = self.servers.get(&server_name)
                && let Some(identity) = view.connection.identity.as_ref()
                && let Some(server) = config.mcp_server_catalog.server(&server_name)
            {
                candidates.push((server_name, identity.clone(), server.config().clone()));
            }
        }
        if candidates.is_empty() {
            return Vec::new();
        }

        let originator = AuthStorageOriginator::current();
        match tokio::task::spawn_blocking(move || {
            originator.sync_scope(|| {
                candidates
                    .into_iter()
                    .filter_map(|(server_name, identity, config)| {
                        identity
                            .oauth_credentials_changed(&server_name, &config)
                            .then_some(server_name)
                    })
                    .collect()
            })
        })
        .await
        {
            Ok(recovered_servers) => recovered_servers,
            Err(error) => {
                warn!(%error, "failed to inspect stored MCP OAuth credentials");
                Vec::new()
            }
        }
    }

    pub(crate) async fn wait_for_server_startup(&self, server_name: &str) -> bool {
        let Some(view) = self.servers.get(server_name) else {
            return false;
        };
        view.connection.client.ready_transport().is_some()
            || view.connection.client_for_call().await.is_ok()
    }

    /// Release this manager's clients, terminating shared stdio servers after their final release.
    pub async fn shutdown(&self) {
        let connections = self
            .servers
            .values()
            .map(|view| Arc::clone(&view.connection))
            .collect::<Vec<_>>();
        Self::shutdown_connections(connections).await;
    }

    /// Releases pool leases retained by older bindings.
    pub(crate) async fn shutdown_shared_connections(&self) {
        let connections = self
            .servers
            .values()
            .filter(|view| view.connection.client.connection_slot.is_some())
            .map(|view| Arc::clone(&view.connection))
            .collect();
        Self::shutdown_connections(connections).await;
    }

    /// Keeps cleanup alive if an interrupt cancels the refresh that requested it.
    async fn shutdown_connections(connections: Vec<Arc<McpServerConnection>>) {
        let shutdown_task = tokio::spawn(async move {
            join_all(connections.iter().map(|connection| connection.shutdown())).await;
        });
        if let Err(error) = shutdown_task.await {
            warn!("MCP client shutdown task failed: {error}");
        }
    }

    pub(crate) fn cancel_startup(&self) {
        for view in self.servers.values() {
            view.connection.cancel_startup();
        }
    }

    pub(crate) fn retire_shared_connections(&self) {
        for view in self.servers.values() {
            if let Some(slot) = &view.connection.client.connection_slot {
                slot.retire();
            }
        }
    }

    pub async fn wait_for_server_ready(&self, server_name: &str, timeout: Duration) -> bool {
        let Some(view) = self.servers.get(server_name) else {
            return false;
        };

        match tokio::time::timeout(timeout, view.connection.client()).await {
            Ok(Ok(_)) => true,
            Ok(Err(_)) | Err(_) => false,
        }
    }

    pub(crate) fn validate_tool_call(
        &self,
        server: &str,
        tool: &str,
        environment_id: Option<&str>,
    ) -> Result<()> {
        let view = self
            .servers
            .get(server)
            .ok_or_else(|| anyhow!("unknown MCP server '{server}'"))?;
        if let Some(environment_id) = environment_id
            && view.metadata.environment_id != environment_id
        {
            bail!(
                "MCP server `{server}` is running in environment `{}`, expected `{environment_id}`",
                view.metadata.environment_id
            );
        }
        if !view.tool_filter.allows(tool) {
            return Err(anyhow!(
                "tool '{tool}' is disabled for MCP server '{server}'"
            ));
        }
        Ok(())
    }

    /// Invoke the tool indicated by the (server, tool) pair.
    #[allow(clippy::too_many_arguments)]
    pub async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        environment_id: Option<&str>,
        arguments: Option<serde_json::Value>,
        meta: Option<serde_json::Value>,
        requested_timeout: Option<Duration>,
        wait_for_server: bool,
    ) -> Result<CallToolResult> {
        self.validate_tool_call(server, tool, environment_id)?;
        let view = &self.servers[server];
        let client = if wait_for_server {
            view.connection
                .client()
                .await
                .context("failed to get client")?
        } else {
            let client = view
                .connection
                .client
                .ready_client()
                .ok_or_else(|| anyhow!("MCP server '{server}' is not connected"))?;
            if client.client.is_closed().await {
                bail!("MCP server '{server}' is not connected");
            }
            client
        };

        let effective_timeout = match (view.tool_timeout, requested_timeout) {
            (Some(server_timeout), Some(requested_timeout)) => {
                Some(server_timeout.min(requested_timeout))
            }
            (server_timeout, requested_timeout) => server_timeout.or(requested_timeout),
        };
        let result: rmcp::model::CallToolResult = client
            .call_tool(
                tool.to_string(),
                arguments,
                self.network_policy_meta_for_call(
                    &client,
                    self.config
                        .as_ref()
                        .and_then(|config| config.permission_profile_for_server(server)),
                    &view.metadata.environment_id,
                    meta,
                ),
                effective_timeout,
            )
            .await
            .with_context(|| format!("tool call failed for `{server}/{tool}`"))?;

        Ok(call_tool_result_from_rmcp(result))
    }

    /// Capabilities belong to the initialized connection, never a shared tool cache.
    pub(crate) fn list_available_server_capabilities(
        &self,
        include_server: impl Fn(&str) -> bool,
    ) -> HashMap<String, serde_json::Value> {
        self.servers
            .iter()
            .filter(|(name, _)| include_server(name))
            .filter_map(|(name, view)| {
                view.connection
                    .retained_legacy()
                    .unwrap_or(&view.connection)
                    .client
                    .server_capabilities
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
                    .map(|capabilities| (name.clone(), capabilities))
            })
            .collect()
    }

    /// Returns presentation metadata from the current connection.
    pub(crate) async fn list_available_server_infos(
        &self,
        include_server: impl Fn(&str) -> bool,
    ) -> HashMap<String, McpServerInfo> {
        let mut server_infos = HashMap::new();
        for (server_name, view) in self.servers.iter().filter(|(name, _)| include_server(name)) {
            match view.connection.client().await {
                Ok(managed_client) => {
                    server_infos.insert(server_name.clone(), managed_client.server_info);
                }
                Err(_) => {}
            }
        }
        server_infos
    }
}

#[cfg(test)]
#[path = "connection_manager_tests.rs"]
pub(crate) mod tests;
