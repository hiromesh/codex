use super::mcp_refresh::McpRefreshInvalidationGuard;
use super::*;
use crate::environment_selection::TurnEnvironmentSnapshot;
use crate::environment_selection::combine_selected_capability_roots;
use crate::mcp_tool_call::McpToolApprovalContext;
use codex_exec_server::DiscoverV2CapabilitiesRequest;
use codex_exec_server::ExecutorCapabilityDiscoveryCache;
use codex_exec_server::ExecutorCapabilityDiscoverySnapshot;
use codex_exec_server::ExecutorCapabilityDiscoveryV2Cache;
use codex_exec_server::ExecutorCapabilityDiscoveryV2Snapshot;
use codex_exec_server::MAX_SELECTED_CAPABILITY_ROOTS;
use codex_exec_server::ResolvedSelectedCapabilityRoot;
use codex_exec_server::supports_capability_discovery_v2;
use codex_mcp::ElicitationReviewRequest;
use codex_mcp::ElicitationReviewer;
use codex_mcp::ElicitationReviewerHandle;
use codex_otel::auth_storage::AuthStorageOriginator;
use codex_protocol::capabilities::CapabilityRootLocation;
use codex_protocol::capabilities::SelectedCapabilityRoot;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::mcp::is_node_repl_backed_connector;
use codex_protocol::mcp::is_node_repl_backed_server;
use codex_protocol::mcp_approval_meta::APPROVAL_KIND_KEY as MCP_ELICITATION_APPROVAL_KIND_KEY;
use codex_protocol::mcp_approval_meta::APPROVAL_KIND_MCP_TOOL_CALL as MCP_ELICITATION_APPROVAL_KIND_MCP_TOOL_CALL;
use codex_protocol::mcp_approval_meta::PERSIST_KEY as MCP_ELICITATION_PERSIST_KEY;
use codex_protocol::mcp_approval_meta::SENSITIVE_ACTION_KEY as MCP_ELICITATION_SENSITIVE_ACTION_KEY;
use codex_protocol::mcp_approval_meta::STRICT_AUTO_REVIEW_KEY as MCP_ELICITATION_STRICT_AUTO_REVIEW_KEY;
use codex_protocol::mcp_approval_meta::TOOL_NAME_KEY as MCP_ELICITATION_TOOL_NAME_KEY;
use codex_protocol::openai_models::GuardianScope;
use rmcp::model::ElicitationAction;


pub(crate) struct McpServerElicitationOutcome {
    pub(crate) response: Option<ElicitationResponse>,
    pub(crate) sent: bool,
}

impl Session {
    pub(crate) async fn runtime_mcp_config(&self, config: &Config) -> McpConfig {
        self.runtime_mcp_config_and_context(config).await.0
    }

    pub(crate) async fn runtime_mcp_config_and_context(
        &self,
        config: &Config,
    ) -> (McpConfig, McpRuntimeContext) {
        let originator = self.originator().await;
        let (session_source, host_fallback_cwd) = {
            let state = self.state.lock().await;
            (
                state.session_configuration.session_source.clone(),
                state.session_configuration.cwd().clone(),
            )
        };
        let environments = self.services.turn_environments.snapshot().await;
        let environment_selections = environments.all_selections();
        let selected_capability_roots = self
            .resolve_selected_capability_roots_for_step(&environments)
            .await;
        let ready_selected_capability_roots =
            Self::ready_selected_capability_roots(&selected_capability_roots);
        let (executor_capability_discovery_v2, inventory_changed) = self
            .executor_capability_discovery_for_step_v2(config, &environments)
            .await;
        let (executor_capability_discovery, discovery_recovered) = self
            .executor_capability_discovery_for_step(
                config,
                &selected_capability_roots,
                &environments,
                executor_capability_discovery_v2.as_deref(),
            )
            .await;
        if inventory_changed || discovery_recovered {
            self.mark_mcp_runtime_dirty();
        }
        let mcp_projection =
            self.services
                .mcp_manager
                .runtime_config_for_step(
                    config,
                    &self.services.mcp_thread_init,
                    &self.services.thread_extension_data,
                    McpThreadIdentity {
                        auth_changed: !self.services.mcp_runtime.current_auth_matches(
                            self.services.auth_manager.auth_cached().as_ref(),
                        ),
                        session_source: &session_source,
                        originator: &originator,
                        environments: McpEnvironmentScope::Selected(&environment_selections),
                    },
                    &ready_selected_capability_roots,
                    executor_capability_discovery.as_deref(),
                    executor_capability_discovery_v2.as_deref(),
                )
                .await;
        let mcp_config = self
            .project_selected_environment_mcp_servers(config, &environments, mcp_projection)
            .await
            .config;
        let runtime_context = self.mcp_runtime_context(&environments, &host_fallback_cwd);
        (mcp_config, runtime_context)
    }

    pub(crate) async fn current_mcp_runtime_context(&self) -> McpRuntimeContext {
        let host_fallback_cwd = self.state.lock().await.session_configuration.cwd().clone();
        let environments = self.services.turn_environments.snapshot().await;
        self.mcp_runtime_context(&environments, &host_fallback_cwd)
    }

    pub(super) fn mcp_runtime_context(
        &self,
        environments: &TurnEnvironmentSnapshot,
        host_fallback_cwd: &std::path::Path,
    ) -> McpRuntimeContext {
        let local_process_cwd = environments
            .local_environment_cwd()
            .map(|cwd| cwd.to_path_buf())
            .unwrap_or_else(|| host_fallback_cwd.to_path_buf());
        McpRuntimeContext::new(
            self.services.turn_environments.environment_manager(),
            local_process_cwd,
        )
        .with_connection_pool(self.services.mcp_manager.connection_pool())
        .with_selected_environments(
            environments.all_selections().into(),
            environments.ready_environment_handles(),
        )
    }

    pub(crate) async fn runtime_mcp_servers(
        &self,
        config: &Config,
    ) -> HashMap<String, McpServerConfig> {
        codex_mcp::configured_mcp_servers(&self.runtime_mcp_config(config).await)
    }

    /// Publishes changed MCP state, waiting for any refresh already in progress.
    #[tracing::instrument(name = "mcp.runtime.refresh_if_dirty", skip_all)]
    pub(crate) async fn refresh_mcp_if_dirty(self: &Arc<Self>) {
        let Ok(_refresh) = self.mcp_refresh.acquire().await else {
            error!("MCP runtime refresh semaphore closed");
            return;
        };
        loop {
            // Compare and rebuild from current environments, not choices saved for a future turn.
            let environments = self.services.turn_environments.snapshot().await;
            let environment_selections = environments.all_selections();
            if !self.services.mcp_runtime.current_environments_match(
                &environment_selections,
                &environments.ready_environment_handles(),
            ) {
                self.mark_mcp_runtime_dirty();
            }
            let auth = self.services.auth_manager.auth_cached();
            if !self
                .services
                .mcp_runtime
                .current_auth_matches(auth.as_ref())
            {
                self.mark_mcp_runtime_dirty();
            }

            if !self.mcp_refresh.claim() {
                return;
            }
            let mut refresh_invalidation = McpRefreshInvalidationGuard {
                refresh: &self.mcp_refresh,
                published: false,
            };
            let auth = self.services.auth_manager.auth().await;
            let desired = self.latest_mcp_desired_state(auth, environments).await;
            let selected_capability_roots = self
                .resolve_selected_capability_roots_for_step(&desired.environments)
                .await;
            let ready_selected_capability_roots =
                Self::ready_selected_capability_roots(&selected_capability_roots);
            let (executor_capability_discovery_v2, _) = self
                .executor_capability_discovery_for_step_v2(&desired.config, &desired.environments)
                .await;
            let (executor_capability_discovery, _) = self
                .executor_capability_discovery_for_step(
                    &desired.config,
                    &selected_capability_roots,
                    &desired.environments,
                    executor_capability_discovery_v2.as_deref(),
                )
                .await;
            let mcp_projection = self
                .services
                .mcp_manager
                .runtime_config_for_step(
                    &desired.config,
                    &self.services.mcp_thread_init,
                    &self.services.thread_extension_data,
                    McpThreadIdentity {
                        auth_changed: !self
                            .services
                            .mcp_runtime
                            .current_auth_matches(desired.auth.as_ref()),
                        session_source: &desired.session_source,
                        originator: &desired.originator,
                        environments: McpEnvironmentScope::Selected(&environment_selections),
                    },
                    &ready_selected_capability_roots,
                    executor_capability_discovery.as_deref(),
                    executor_capability_discovery_v2.as_deref(),
                )
                .await;
            self.publish_mcp_runtime(
                &desired,
                mcp_projection,
                &ready_selected_capability_roots,
                None,
            )
            .await;
            refresh_invalidation.published = true;
        }
    }

    pub(super) fn mark_mcp_runtime_dirty(&self) {
        self.mcp_refresh.invalidate();
    }

    #[tracing::instrument(
        name = "mcp.runtime.resolve_for_step",
        skip_all,
        fields(turn.id = %turn_context.sub_id)
    )]
    pub(crate) async fn mcp_runtime_for_step(
        self: &Arc<Self>,
        turn_context: &TurnContext,
        selected_capability_roots: &[ResolvedSelectedCapabilityRoot],
        required_servers: &[String],
    ) -> Arc<codex_mcp::McpBinding> {
        let ready_selected_capability_roots =
            Self::ready_selected_capability_roots(selected_capability_roots);
        if self
            .services
            .mcp_runtime
            .current_ready_selected_capability_roots()
            != ready_selected_capability_roots
        {
            self.mark_mcp_runtime_dirty();
        }

        let recovered_oauth_servers =
            AuthStorageOriginator::from_client_name(&turn_context.originator)
                .scope(
                    self.services
                        .mcp_runtime
                        .updated_oauth_credentials_after_auth_failure(),
                )
                .await;
        if !recovered_oauth_servers.is_empty()
            && let Ok(_refresh) = self.mcp_refresh.acquire().await
            && self
                .services
                .mcp_runtime
                .has_authentication_failed_servers(&recovered_oauth_servers)
                .await
        {
            self.mark_mcp_runtime_dirty();
        }
        self.refresh_mcp_if_dirty().await;
        // Prewarm and other context-only captures must not start cloud discovery.
        let is_active_regular_turn = {
            let active_turn = self.active_turn.lock().await;
            active_turn
                .as_ref()
                .and_then(|turn| turn.task.as_ref())
                .is_some_and(|task| {
                    task.kind == TaskKind::Regular
                        && task.turn_context.sub_id == turn_context.sub_id
                })
        };
        if is_active_regular_turn {
            // Refresh cloud catalogs without holding the MCP permit or active-turn lock.
            for contributor in self.services.extensions.turn_lifecycle_contributors() {
                if contributor
                    .refresh_catalog_for_step(codex_extension_api::CatalogRefreshInput {
                        turn_id: &turn_context.sub_id,
                        session_store: &self.services.session_extension_data,
                        thread_store: &self.services.thread_extension_data,
                        turn_store: turn_context.extension_data.as_ref(),
                    })
                    .await
                {
                    // Preserve MCP updates if a later refresh is cancelled.
                    self.mark_mcp_runtime_dirty();
                }
            }
            self.refresh_mcp_if_dirty().await;
        }
        let required_servers = required_servers
            .iter()
            .chain(&recovered_oauth_servers)
            .cloned()
            .collect::<Vec<_>>();
        if let Some(binding) = self
            .services
            .mcp_runtime
            .current_binding_with_requirements(&required_servers)
            .await
        {
            return binding;
        }
        let config = Arc::new(self.runtime_mcp_config(&turn_context.config).await);
        Arc::new(codex_mcp::McpBinding::empty(config))
    }

    #[tracing::instrument(
        name = "capability_roots.snapshot_for_step",
        skip_all,
        fields(root_count = resolved_roots.len())
    )]
    pub(crate) async fn executor_capability_discovery_for_step(
        &self,
        config: &Config,
        resolved_roots: &[ResolvedSelectedCapabilityRoot],
        environments: &TurnEnvironmentSnapshot,
        v2_snapshots: Option<&[ExecutorCapabilityDiscoveryV2Snapshot]>,
    ) -> (Option<Arc<ExecutorCapabilityDiscoverySnapshot>>, bool) {
        if v2_snapshots.is_some() {
            return (None, false);
        }
        let restricted_file_system = environments.primary().map_or_else(
            || {
                !config
                    .permissions
                    .file_system_sandbox_policy()
                    .has_full_disk_read_access()
            },
            |_| {
                environments.turn_environments().any(|environment| {
                    !environment
                        .permission_profile()
                        .file_system_sandbox_policy()
                        .has_full_disk_read_access()
                })
            },
        );
        if !restricted_file_system
            && !config
                .features
                .enabled(Feature::ExecutorCapabilityDiscovery)
        {
            return (None, false);
        }
        let sandbox_contexts = if restricted_file_system {
            environments
                .turn_environments()
                .map(|environment| {
                    (
                        environment.selection.environment_id.clone(),
                        environment.sandbox_context(/*additional_permissions*/ None),
                    )
                })
                .collect::<HashMap<_, _>>()
        } else {
            HashMap::new()
        };
        let cache = self
            .services
            .thread_extension_data
            .get_or_init(ExecutorCapabilityDiscoveryCache::default);
        let selected_capability_roots = resolved_roots
            .iter()
            .filter(|root| {
                let selected_root = root.selected_root();
                if !restricted_file_system {
                    return true;
                }
                let CapabilityRootLocation::Environment { environment_id, .. } =
                    &selected_root.location;
                if sandbox_contexts.contains_key(environment_id) {
                    return true;
                }
                warn!(
                    selected_root = selected_root.id,
                    environment_id, "skipping capability root without a filesystem sandbox context"
                );
                false
            })
            .cloned()
            .collect::<Vec<_>>();
        let discovery = cache
            .snapshot(&selected_capability_roots, &sandbox_contexts)
            .await;
        (Some(Arc::new(discovery)), cache.take_recovered_discovery())
    }

    /// Acquires V2 inventories and reports changes without scheduling an MCP refresh.
    pub(super) async fn executor_capability_discovery_for_step_v2(
        &self,
        config: &Config,
        environments: &TurnEnvironmentSnapshot,
    ) -> (Option<Vec<ExecutorCapabilityDiscoveryV2Snapshot>>, bool) {
        if !config
            .features
            .enabled(Feature::ExecutorCapabilityDiscoveryV2)
        {
            return (None, false);
        }
        // Retain this cache if a new turn replaces the thread's current cache.
        let cache = self
            .services
            .thread_extension_data
            .get_or_init(ExecutorCapabilityDiscoveryV2Cache::default);
        let remote_environment_count = environments
            .turn_environments()
            .filter(|selected| selected.environment.is_remote())
            .count();
        cache.ensure_capacity(remote_environment_count);
        let mut snapshots = Vec::new();
        let mut skipped_incompatible_executor = false;
        for selected in environments.turn_environments() {
            if !selected.environment.is_remote() {
                continue;
            }
            let environment_id = &selected.selection.environment_id;
            let info = match selected.environment.info().await {
                Ok(info) => info,
                Err(_) => {
                    tracing::warn!(
                        environment_id,
                        "could not read executor discovery V2 support"
                    );
                    continue;
                }
            };
            if !supports_capability_discovery_v2(&info) {
                skipped_incompatible_executor = true;
                continue;
            }
            let request = DiscoverV2CapabilitiesRequest {
                cwd: selected.cwd().clone(),
                sandbox: Some(selected.sandbox_context(/*additional_permissions*/ None)),
            };
            let result = cache
                .discover(environment_id, &selected.environment, request)
                .await;
            snapshots.push(ExecutorCapabilityDiscoveryV2Snapshot {
                environment_id: environment_id.clone(),
                result,
            });
        }
        if snapshots.is_empty() && skipped_incompatible_executor {
            let changed = self
                .services
                .thread_extension_data
                .remove::<Vec<ExecutorCapabilityDiscoveryV2Snapshot>>()
                .is_some();
            return (None, changed);
        }
        let previous = self
            .services
            .thread_extension_data
            .insert(snapshots.clone());
        let inventory_changed = previous.as_ref().map_or(!snapshots.is_empty(), |previous| {
            previous.len() != snapshots.len()
                || previous
                    .iter()
                    .zip(&snapshots)
                    .any(|(previous, current)| !previous.same_discovery(current))
        });
        let inventory_changed = cache.take_recovered_discovery() || inventory_changed;
        (Some(snapshots), inventory_changed)
    }

    pub(crate) async fn resolve_selected_capability_roots_for_step(
        &self,
        environments: &TurnEnvironmentSnapshot,
    ) -> Vec<ResolvedSelectedCapabilityRoot> {
        let captured_environments = environments.captured_environments();
        let thread_roots = environments.selected_capability_roots();
        let thread_root_count = thread_roots.len();
        let mut root_locations_by_id = HashMap::new();
        let mut selected_capability_roots = Vec::new();
        let mut ready_environment_root_count = 0;
        let combined_roots = combine_selected_capability_roots(
            &thread_roots,
            environments.turn_environments().map(|environment| {
                environment
                    .config_origin
                    .selected_capability_roots(&environment.environment, environment.config())
            }),
        );
        for (index, root) in combined_roots.into_iter().enumerate() {
            let CapabilityRootLocation::Environment { environment_id, .. } = &root.location;
            // Filter before readiness resolution, which can reconnect a historical executor.
            if !captured_environments.contains_key(environment_id) {
                tracing::warn!(
                    root_id = root.id,
                    environment_id,
                    "ignoring capability root without a captured turn environment"
                );
                continue;
            }
            if let Some(kept_location) = root_locations_by_id.get(&root.id) {
                if kept_location != &root.location {
                    tracing::warn!(
                        root_id = root.id,
                        ?kept_location,
                        ignored_location = ?root.location,
                        "ignoring selected capability root with conflicting location"
                    );
                }
                continue;
            }
            if index >= thread_root_count {
                if ready_environment_root_count == MAX_SELECTED_CAPABILITY_ROOTS {
                    tracing::warn!(
                        max_root_count = MAX_SELECTED_CAPABILITY_ROOTS,
                        "ignoring excess selected capability roots from ready environments"
                    );
                    break;
                }
                ready_environment_root_count += 1;
            }
            root_locations_by_id.insert(root.id.clone(), root.location.clone());
            selected_capability_roots.push(root);
        }
        self.services
            .turn_environments
            .environment_manager()
            .resolve_selected_capability_roots(&selected_capability_roots, &captured_environments)
            .await
    }

    pub(crate) fn mcp_elicitation_lifecycle(&self) -> codex_mcp::ElicitationLifecycle {
        self.mcp_elicitation_lifecycle_handle
            .get_or_init(|| {
                let elicitations = self.services.elicitations.clone();
                codex_mcp::ElicitationLifecycle::new(move || elicitations.register())
            })
            .clone()
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub async fn request_mcp_server_elicitation(
        &self,
        turn_context: &TurnContext,
        server_name: String,
        request_id: RequestId,
        request: ElicitationRequest,
    ) -> McpServerElicitationOutcome {
        if self.services.mcp_runtime.elicitations_auto_deny() {
            return McpServerElicitationOutcome {
                response: Some(ElicitationResponse {
                    action: codex_rmcp_client::ElicitationAction::Accept,
                    content: Some(serde_json::json!({})),
                    meta: None,
                }),
                sent: false,
            };
        }

        // This callback carries a request ID, not a reliable tool invocation owner.
        // Do not report its human wait as ordinary tool execution.
        if turn_context
            .config
            .features
            .enabled(Feature::CriticalPathTiming)
        {
            turn_context.turn_timing_state.invalidate_critical_path();
        }
        let _elicitation = self.services.elicitations.register();
        let (tx_response, rx_response) = oneshot::channel();
        let prev_entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.insert_pending_elicitation(
                        server_name.clone(),
                        request_id.clone(),
                        tx_response,
                    )
                }
                None => None,
            }
        };
        if prev_entry.is_some() {
            warn!(
                "Overwriting existing pending elicitation for server_name: {server_name}, request_id: {request_id}"
            );
        }
        let id = match request_id {
            rmcp::model::NumberOrString::String(value) => {
                codex_protocol::mcp::RequestId::String(value.to_string())
            }
            rmcp::model::NumberOrString::Number(value) => {
                codex_protocol::mcp::RequestId::Integer(value)
            }
        };
        let event = EventMsg::ElicitationRequest(ElicitationRequestEvent {
            turn_id: Some(turn_context.sub_id.clone()),
            server_name,
            id,
            request,
        });
        turn_context
            .turn_metadata_state
            .mark_user_input_requested_during_turn();
        self.send_event(turn_context, event).await;
        McpServerElicitationOutcome {
            response: rx_response.await.ok(),
            sent: true,
        }
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and manager fallback must stay serialized"
    )]
    pub async fn resolve_elicitation(
        &self,
        server_name: String,
        id: RequestId,
        response: ElicitationResponse,
    ) -> anyhow::Result<()> {
        let entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.remove_pending_elicitation(&server_name, &id)
                }
                None => None,
            }
        };
        if let Some(tx_response) = entry {
            tx_response
                .send(response)
                .map_err(|e| anyhow::anyhow!("failed to send elicitation response: {e:?}"))?;
            return Ok(());
        }

        self.services
            .mcp_runtime
            .resolve_elicitation(server_name, id, response)
            .await
    }

    pub(crate) async fn refresh_mcp_servers_now(
        &self,
        added_servers: HashMap<String, McpServerConfig>,
        elicitation_reviewer: Option<ElicitationReviewerHandle>,
    ) {
        let Ok(_refresh) = self.mcp_refresh.acquire().await else {
            error!("MCP runtime refresh semaphore closed");
            return;
        };
        {
            let mut state = self.state.lock().await;
            let mut config = (*state.session_configuration.original_config_do_not_use).clone();
            let mut servers = config.mcp_servers.get().clone();
            for (name, server) in added_servers {
                servers.entry(name).or_insert(server);
            }
            if let Err(err) = config.mcp_servers.set(servers) {
                warn!("failed to refresh MCP dependencies for mentioned skills: {err}");
                return;
            }
            state.session_configuration.original_config_do_not_use = Arc::new(config);
        }
        // Claim after config mutation, before capturing the state this forced refresh handles.
        self.mcp_refresh.claim();
        let mut refresh_invalidation = McpRefreshInvalidationGuard {
            refresh: &self.mcp_refresh,
            published: false,
        };
        let auth = self.services.auth_manager.auth().await;
        let ready_selected_capability_roots = self
            .services
            .mcp_runtime
            .current_ready_selected_capability_roots();
        let environments = self.services.turn_environments.snapshot().await;
        // Adding MCP dependencies preserves the published root selection.
        let mut captured_environments = environments.captured_environments();
        for root in &ready_selected_capability_roots {
            let CapabilityRootLocation::Environment { environment_id, .. } = &root.location;
            // A previously published root must not reconnect an environment absent from this capture.
            captured_environments
                .entry(environment_id.clone())
                .or_insert(None);
        }
        let selected_capability_roots = self
            .services
            .turn_environments
            .environment_manager()
            .resolve_selected_capability_roots(
                &ready_selected_capability_roots,
                &captured_environments,
            )
            .await;
        let environment_selections = environments.all_selections();
        let desired = self.latest_mcp_desired_state(auth, environments).await;
        let (executor_capability_discovery_v2, _) = self
            .executor_capability_discovery_for_step_v2(&desired.config, &desired.environments)
            .await;
        let (executor_capability_discovery, _) = self
            .executor_capability_discovery_for_step(
                &desired.config,
                &selected_capability_roots,
                &desired.environments,
                executor_capability_discovery_v2.as_deref(),
            )
            .await;
        let mcp_projection = self
            .services
            .mcp_manager
            .runtime_config_for_step(
                &desired.config,
                &self.services.mcp_thread_init,
                &self.services.thread_extension_data,
                McpThreadIdentity {
                    auth_changed: !self
                        .services
                        .mcp_runtime
                        .current_auth_matches(desired.auth.as_ref()),
                    session_source: &desired.session_source,
                    originator: &desired.originator,
                    environments: McpEnvironmentScope::Selected(&environment_selections),
                },
                &ready_selected_capability_roots,
                executor_capability_discovery.as_deref(),
                executor_capability_discovery_v2.as_deref(),
            )
            .await;
        self.publish_mcp_runtime(
            &desired,
            mcp_projection,
            &ready_selected_capability_roots,
            elicitation_reviewer,
        )
        .await;
        refresh_invalidation.published = true;
    }

    pub(crate) fn ready_selected_capability_roots(
        selected_capability_roots: &[ResolvedSelectedCapabilityRoot],
    ) -> Vec<SelectedCapabilityRoot> {
        selected_capability_roots
            .iter()
            .map(|root| root.selected_root().clone())
            .collect()
    }

    pub(crate) fn cancel_mcp_startup(&self) {
        self.services.mcp_runtime.cancel_startup();
    }
}

