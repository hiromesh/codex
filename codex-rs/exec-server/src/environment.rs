mod connect_options;

pub use connect_options::RemoteEnvironmentOptions;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::RwLock;
use std::time::Duration;

use arc_swap::ArcSwapOption;
use codex_build_info::BuildInfo;
use codex_config::ScopedSkillsConfig;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_protocol::capabilities::SelectedCapabilityRoot;

use crate::CapabilityRootsDiscoverParams;
use crate::CapabilityRootsDiscoverResponse;
use crate::DiscoverV2CapabilitiesRequest;
use crate::DiscoverV2CapabilitiesResponse;
use crate::EnvironmentConfigReadParams;
use crate::EnvironmentConfigReadResponse;
use crate::ExecServerError;
use crate::ExecServerRuntimeOptions;
use crate::ExecutorFileSystem;
use crate::HttpClient;
use crate::client::http_client::RouteAwareHttpClient;
use crate::environment_bootstrap::PreparedEnvironmentManager;
use crate::environment_config::read_environment_config;
use crate::local_file_system::LocalFileSystem;
use crate::local_process::LocalProcess;
use crate::process::ExecBackend;
use crate::protocol::EnvironmentInfo;

pub const CODEX_EXEC_SERVER_URL_ENV_VAR: &str = "CODEX_EXEC_SERVER_URL";
pub const CODEX_EXEC_SERVER_NOISE_REGISTRY_URL_ENV_VAR: &str =
    "CODEX_EXEC_SERVER_NOISE_REGISTRY_URL";
pub const CODEX_EXEC_SERVER_NOISE_ENVIRONMENT_ID_ENV_VAR: &str =
    "CODEX_EXEC_SERVER_NOISE_ENVIRONMENT_ID";
pub const CODEX_EXEC_SERVER_NOISE_CHATGPT_ACCOUNT_ID_ENV_VAR: &str =
    "CODEX_EXEC_SERVER_NOISE_CHATGPT_ACCOUNT_ID";

/// The current connection state for one concrete environment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnvironmentConnectionState {
    /// An initialized exec-server connection is available.
    Connected,
    /// An established session is unavailable while its transport is being recovered.
    Recovering,
    /// No initialized exec-server connection is currently available.
    Disconnected,
}

/// Outcome of an initial connection operation, including its internal retries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionAttemptOutcome {
    Success,
    Failure,
    Cancelled,
}

/// Owns the execution/filesystem environments available to the Codex runtime.
///
/// Only the local environment remains. `CODEX_EXEC_SERVER_URL=none` disables
/// environment access by leaving the default environment unset and omitting the
/// local environment. Callers use `default_environment().is_some()` as the
/// signal for model-facing shell/filesystem tool availability.
#[derive(Debug)]
pub struct EnvironmentManager {
    default_environment: Option<String>,
    pub(super) environments: RwLock<HashMap<String, Arc<Environment>>>,
    local_environment: Option<Arc<Environment>>,
    http_client_factory: HttpClientFactory,
}

/// Information supplied by the environment owner when an environment is ready.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EnvironmentReadyInfo {
    /// Ordered capability roots selected for this environment.
    pub selected_capability_roots: Vec<SelectedCapabilityRoot>,
}

/// Maximum capability roots accepted from environment ready information.
pub const MAX_SELECTED_CAPABILITY_ROOTS: usize = 256;

pub const LOCAL_ENVIRONMENT_ID: &str = "local";
pub const REMOTE_ENVIRONMENT_ID: &str = "remote";

/// Non-mutating connection status observed by an environment owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentObservedStatus {
    /// A local environment, or an environment whose existing connection answered a probe.
    Ready,
    /// The configured environment has no ready connection and no observed connection failure.
    Pending,
    /// A connection attempt, prior connection, or fail-fast status probe observed a failure.
    Disconnected {
        /// Human-readable reason recorded by the failed connection attempt or probe.
        error: String,
    },
}

impl EnvironmentManager {
    /// Builds a test-only manager without configured sandbox helper paths.
    pub fn default_for_tests() -> Self {
        Self::from_local_preparation(
            /*include_local*/ true,
            /*local_runtime_paths*/ None,
            HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
        )
    }

    /// Builds a manager with no configured execution environments.
    pub fn without_environments(http_client_factory: HttpClientFactory) -> Self {
        Self {
            default_environment: None,
            environments: RwLock::new(HashMap::new()),
            local_environment: None,
            http_client_factory,
        }
    }

    /// Builds a test-only manager from a raw exec-server URL value.
    ///
    /// Remote transports are no longer supported; any non-`none` value resolves
    /// to the local environment.
    pub async fn create_for_tests(
        exec_server_url: Option<String>,
        local_runtime_paths: Option<ExecServerRuntimeOptions>,
    ) -> Self {
        Self::from_local_preparation(
            !exec_server_disabled(exec_server_url.as_deref()),
            local_runtime_paths,
            // Test-only construction has no application config from which to resolve proxy policy.
            HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
        )
    }

    /// Builds a test-only manager that also allows tests to select the local environment.
    pub async fn create_for_tests_with_local(
        exec_server_url: Option<String>,
        local_runtime_paths: ExecServerRuntimeOptions,
    ) -> Self {
        Self::from_local_preparation(
            !exec_server_disabled(exec_server_url.as_deref()),
            Some(local_runtime_paths),
            // Test-only construction has no application config from which to resolve proxy policy.
            HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
        )
    }

    /// Discovers configured environments without connecting.
    ///
    /// `CODEX_HOME/environments.toml` is no longer consulted because remote
    /// environments have been removed. Only `CODEX_EXEC_SERVER_URL=none`
    /// changes the result.
    pub async fn prepare_from_codex_home(
        _codex_home: impl AsRef<Path>,
    ) -> Result<PreparedEnvironmentManager, ExecServerError> {
        Self::prepare_from_env().await
    }

    /// Builds a manager from `CODEX_HOME` with an explicit outbound HTTP policy.
    pub async fn from_codex_home(
        codex_home: impl AsRef<Path>,
        local_runtime_paths: Option<ExecServerRuntimeOptions>,
        http_client_factory: HttpClientFactory,
    ) -> Result<Self, ExecServerError> {
        Self::prepare_from_codex_home(codex_home)
            .await?
            .build(local_runtime_paths, http_client_factory)
    }

    /// Discovers environment-variable environments without connecting.
    pub async fn prepare_from_env() -> Result<PreparedEnvironmentManager, ExecServerError> {
        let disabled = match std::env::var(CODEX_EXEC_SERVER_URL_ENV_VAR) {
            Ok(value) => exec_server_disabled(Some(&value)),
            Err(_) => false,
        };
        Ok(PreparedEnvironmentManager::new(!disabled))
    }

    /// Builds a manager from environment variables with an explicit outbound HTTP policy.
    pub async fn from_env(
        local_runtime_paths: Option<ExecServerRuntimeOptions>,
        http_client_factory: HttpClientFactory,
    ) -> Result<Self, ExecServerError> {
        Self::prepare_from_env()
            .await?
            .build(local_runtime_paths, http_client_factory)
    }

    pub(crate) fn from_local_preparation(
        include_local: bool,
        local_runtime_paths: Option<ExecServerRuntimeOptions>,
        http_client_factory: HttpClientFactory,
    ) -> Self {
        if !include_local {
            return Self::without_environments(http_client_factory);
        }

        let environment = match local_runtime_paths {
            Some(local_runtime_paths) => {
                Environment::local(local_runtime_paths, http_client_factory.clone())
            }
            None => Environment::local_without_runtime_paths(http_client_factory.clone()),
        };
        let environment = Arc::new(environment);
        let mut environments = HashMap::new();
        environments.insert(LOCAL_ENVIRONMENT_ID.to_string(), Arc::clone(&environment));
        Self {
            default_environment: Some(LOCAL_ENVIRONMENT_ID.to_string()),
            environments: RwLock::new(environments),
            local_environment: Some(environment),
            http_client_factory,
        }
    }

    /// Returns the default environment instance.
    pub fn default_environment(&self) -> Option<Arc<Environment>> {
        self.default_environment
            .as_deref()
            .and_then(|environment_id| self.get_environment(environment_id))
    }

    /// Returns the id of the default environment.
    pub fn default_environment_id(&self) -> Option<&str> {
        self.default_environment.as_deref()
    }

    /// Returns the ordered environment ids used for new thread startup.
    pub fn default_environment_ids(&self) -> Vec<String> {
        let Some(default_environment_id) = self.default_environment.as_ref() else {
            return Vec::new();
        };
        let environments = self
            .environments
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut environment_ids = Vec::with_capacity(environments.len());
        environment_ids.push(default_environment_id.clone());
        environment_ids.extend(
            environments
                .keys()
                .filter(|environment_id| *environment_id != default_environment_id)
                .cloned(),
        );
        environment_ids
    }

    /// Returns the local environment instance when one is configured.
    pub fn try_local_environment(&self) -> Option<Arc<Environment>> {
        self.local_environment.as_ref().map(Arc::clone)
    }

    /// Returns a named environment instance.
    pub fn get_environment(&self, environment_id: &str) -> Option<Arc<Environment>> {
        self.environments
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(environment_id)
            .cloned()
    }

    /// Returns the outbound HTTP policy carried by this manager.
    pub fn http_client_factory(&self) -> &HttpClientFactory {
        &self.http_client_factory
    }

    /// Returns the current status of one named environment when it is configured.
    pub async fn get_environment_status(
        &self,
        environment_id: &str,
    ) -> Option<EnvironmentObservedStatus> {
        let environment = self.get_environment(environment_id)?;
        Some(environment.status().await)
    }

    /// Adds or replaces a named remote environment.
    ///
    /// Remote execution environments have been removed; this compatibility entry
    /// point reports the unsupported operation.
    pub fn upsert_environment(
        &self,
        environment_id: String,
        exec_server_url: String,
        connect_timeout: Option<Duration>,
    ) -> Result<(), ExecServerError> {
        self.upsert_environment_with_options(
            environment_id,
            RemoteEnvironmentOptions {
                exec_server_url,
                connect_timeout,
                http_headers: HashMap::new(),
            },
            ScopedSkillsConfig::default(),
        )
    }

    /// Adds or replaces a direct remote environment with trusted host-owned options.
    ///
    /// Remote execution environments have been removed; this compatibility entry
    /// point reports the unsupported operation.
    pub fn upsert_environment_with_options(
        &self,
        environment_id: String,
        _options: RemoteEnvironmentOptions,
        _skills: ScopedSkillsConfig,
    ) -> Result<(), ExecServerError> {
        validate_environment_id(&environment_id)?;
        Err(ExecServerError::Protocol(
            "remote execution environments are not supported".to_string(),
        ))
    }
}

fn exec_server_disabled(exec_server_url: Option<&str>) -> bool {
    matches!(
        exec_server_url.map(str::trim),
        Some(url) if url.eq_ignore_ascii_case("none")
    )
}

fn validate_environment_id(environment_id: &str) -> Result<(), ExecServerError> {
    if environment_id.is_empty() {
        return Err(ExecServerError::Protocol(
            "environment id cannot be empty".to_string(),
        ));
    }
    if environment_id == LOCAL_ENVIRONMENT_ID {
        return Err(ExecServerError::Protocol(format!(
            "environment id `{LOCAL_ENVIRONMENT_ID}` is reserved for EnvironmentManager"
        )));
    }
    Ok(())
}

/// Concrete execution/filesystem environment selected for a session.
///
/// This bundles the selected backend metadata together with the local runtime
/// paths used by filesystem helpers.
#[derive(Clone)]
pub struct Environment {
    skills: ScopedSkillsConfig,
    ready_info: Arc<ArcSwapOption<EnvironmentReadyInfo>>,
    exec_backend: Arc<dyn ExecBackend>,
    filesystem: Arc<dyn ExecutorFileSystem>,
    http_client: Arc<dyn HttpClient>,
    local_runtime_paths: Option<ExecServerRuntimeOptions>,
}

impl Environment {
    /// Builds a test-only local environment without configured sandbox helper paths.
    pub fn default_for_tests() -> Self {
        Self::local_without_runtime_paths(HttpClientFactory::new(
            OutboundProxyPolicy::ReqwestDefault,
        ))
    }
}

impl std::fmt::Debug for Environment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Environment").finish_non_exhaustive()
    }
}

impl Environment {
    /// Builds an environment using the caller's effective outbound HTTP policy.
    ///
    /// Any configured exec-server URL is ignored because remote execution
    /// environments have been removed; a disabled URL still disables creation.
    pub fn create(
        exec_server_url: Option<String>,
        local_runtime_paths: ExecServerRuntimeOptions,
        http_client_factory: HttpClientFactory,
    ) -> Result<Self, ExecServerError> {
        Self::create_inner(
            exec_server_url,
            Some(local_runtime_paths),
            http_client_factory,
        )
    }

    /// Builds a test-only environment without configured sandbox helper paths.
    pub fn create_for_tests(exec_server_url: Option<String>) -> Result<Self, ExecServerError> {
        Self::create_inner(
            exec_server_url,
            /*local_runtime_paths*/ None,
            HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
        )
    }

    /// Builds an environment from the raw `CODEX_EXEC_SERVER_URL` value and
    /// local runtime paths used when creating local filesystem helpers.
    fn create_inner(
        exec_server_url: Option<String>,
        local_runtime_paths: Option<ExecServerRuntimeOptions>,
        http_client_factory: HttpClientFactory,
    ) -> Result<Self, ExecServerError> {
        if exec_server_disabled(exec_server_url.as_deref()) {
            return Err(ExecServerError::Protocol(
                "disabled mode does not create an Environment".to_string(),
            ));
        }

        Ok(match local_runtime_paths {
            Some(local_runtime_paths) => Self::local(local_runtime_paths, http_client_factory),
            None => Self::local_without_runtime_paths(http_client_factory),
        })
    }

    pub(crate) fn local(
        local_runtime_paths: ExecServerRuntimeOptions,
        http_client_factory: HttpClientFactory,
    ) -> Self {
        Self {
            skills: ScopedSkillsConfig::default(),
            ready_info: Arc::new(ArcSwapOption::empty()),
            exec_backend: Arc::new(LocalProcess::with_local_runtime_paths(
                local_runtime_paths.clone(),
            )),
            filesystem: Arc::new(LocalFileSystem::with_runtime_paths(
                local_runtime_paths.clone(),
            )),
            http_client: Arc::new(RouteAwareHttpClient::new(http_client_factory)),
            local_runtime_paths: Some(local_runtime_paths),
        }
    }

    fn local_without_runtime_paths(http_client_factory: HttpClientFactory) -> Self {
        Self {
            skills: ScopedSkillsConfig::default(),
            ready_info: Arc::new(ArcSwapOption::empty()),
            exec_backend: Arc::new(LocalProcess::default()),
            filesystem: Arc::new(LocalFileSystem::unsandboxed()),
            http_client: Arc::new(RouteAwareHttpClient::new(http_client_factory)),
            local_runtime_paths: None,
        }
    }

    /// Skill names required whenever this registration is selected by a turn.
    pub fn required_skills(&self) -> &[String] {
        &self.skills.required
    }

    pub fn is_remote(&self) -> bool {
        false
    }

    /// Returns a snapshot of the last accepted Ready report.
    ///
    /// Local environments never receive Ready reports, so this is always `None`.
    pub fn last_ready_info(&self) -> Option<Arc<EnvironmentReadyInfo>> {
        self.ready_info.load_full()
    }

    /// Returns the capability roots most recently reported for this environment.
    pub fn selected_capability_roots(&self) -> Vec<SelectedCapabilityRoot> {
        self.ready_info
            .load()
            .as_ref()
            .map_or_else(Vec::new, |ready_info| {
                ready_info.selected_capability_roots.clone()
            })
    }

    /// Local environments do not expose connection attempts.
    pub fn observe_connection_attempts(
        &self,
        _observer: impl Fn(Duration, ConnectionAttemptOutcome) + Send + Sync + 'static,
    ) {
    }

    /// Local environments have no remote connection state.
    pub fn subscribe_connection_state(
        &self,
    ) -> Option<tokio::sync::watch::Receiver<EnvironmentConnectionState>> {
        None
    }

    pub fn local_runtime_paths(&self) -> Option<&ExecServerRuntimeOptions> {
        self.local_runtime_paths.as_ref()
    }

    /// Returns environment information from the selected execution/filesystem environment.
    #[tracing::instrument(
        name = "exec_server.environment.info",
        skip_all,
        fields(remote = self.is_remote())
    )]
    pub async fn info(&self) -> Result<EnvironmentInfo, ExecServerError> {
        Ok(EnvironmentInfo::local())
    }

    /// Local environments have no executor registration.
    pub fn cached_executor_registration_id(&self) -> Option<String> {
        None
    }

    /// Refreshes the connection to a remote executor.
    ///
    /// Local environments have no remote connection to refresh.
    pub async fn refresh_connection(&self) -> Result<(), ExecServerError> {
        Err(ExecServerError::Protocol(
            "connection refresh requires a remote environment".to_string(),
        ))
    }

    /// Fetches uncached environment metadata.
    #[tracing::instrument(
        name = "exec_server.environment.force_info",
        skip_all,
        fields(remote = self.is_remote())
    )]
    pub async fn force_info(&self) -> Result<EnvironmentInfo, ExecServerError> {
        Ok(EnvironmentInfo::local())
    }

    /// Reads selected executor-local configuration fields for this environment.
    pub async fn read_environment_config(
        &self,
        params: EnvironmentConfigReadParams,
    ) -> Result<EnvironmentConfigReadResponse, ExecServerError> {
        read_environment_config(self.filesystem.as_ref(), params, /*prefer_mxc*/ None)
            .await
            .map_err(|error| ExecServerError::Protocol(error.to_string()))
    }

    /// Reads configuration with a timeout once connected.
    ///
    /// Local reads retain their existing behavior without a timeout.
    pub async fn read_environment_config_with_timeout(
        &self,
        params: EnvironmentConfigReadParams,
        _rpc_timeout: Duration,
    ) -> Result<EnvironmentConfigReadResponse, ExecServerError> {
        self.read_environment_config(params).await
    }

    /// Discovers skill manifests through the environment's discovery API.
    pub async fn discover_capability_roots(
        &self,
        params: CapabilityRootsDiscoverParams,
    ) -> Result<CapabilityRootsDiscoverResponse, ExecServerError> {
        crate::discover_capability_roots(self.filesystem.as_ref(), params)
            .await
            .map_err(|error| ExecServerError::Protocol(error.to_string()))
    }

    /// Local environments are always connected.
    pub fn start_connecting(&self) {}

    /// Local environments start immediately.
    pub(crate) fn start_connecting_for_use(_environment: &Arc<Self>) {}

    /// Local environments have finished startup.
    pub fn startup_finished(&self) -> bool {
        true
    }

    /// Local environments are always ready.
    #[tracing::instrument(
        name = "exec_server.environment.wait_until_ready",
        skip_all,
        fields(remote = self.is_remote())
    )]
    pub async fn wait_until_ready(&self) -> Result<(), ExecServerError> {
        Ok(())
    }

    /// Local environments can serve requests without reconnecting.
    pub(crate) fn readiness_result(&self) -> Option<Result<(), ExecServerError>> {
        Some(Ok(()))
    }

    /// Local environments are always ready.
    pub async fn status(&self) -> EnvironmentObservedStatus {
        EnvironmentObservedStatus::Ready
    }

    pub fn get_exec_backend(&self) -> Arc<dyn ExecBackend> {
        Arc::clone(&self.exec_backend)
    }

    pub fn get_http_client(&self) -> Arc<dyn HttpClient> {
        Arc::clone(&self.http_client)
    }

    pub fn get_filesystem(&self) -> Arc<dyn ExecutorFileSystem> {
        Arc::clone(&self.filesystem)
    }

    /// Borrows the shared filesystem identity without extending the environment's lifetime.
    pub fn filesystem_ref(&self) -> &Arc<dyn ExecutorFileSystem> {
        &self.filesystem
    }

    /// Returns a filesystem view that fails instead of reconnecting.
    pub fn get_filesystem_without_reconnect(&self) -> Arc<dyn ExecutorFileSystem> {
        Arc::clone(&self.filesystem)
    }
}

impl Environment {
    /// Identifies the initialized executor session; local environments have none.
    pub(crate) async fn executor_session_id(&self) -> Result<Option<String>, ExecServerError> {
        Ok(None)
    }

    /// Returns metadata for the caller's cwd from the remote session's V2 lifecycle.
    ///
    /// Local environments retain the V1 selected-root API.
    pub async fn discover_capabilities_v2(
        &self,
        _params: DiscoverV2CapabilitiesRequest,
    ) -> Result<DiscoverV2CapabilitiesResponse, ExecServerError> {
        Err(ExecServerError::Protocol(
            "capabilities/discoverV2 requires a remote environment".to_string(),
        ))
    }
}

/// The final DiscoveryV2 contract starts at stable 0.164.0.
pub fn supports_capability_discovery_v2(info: &EnvironmentInfo) -> bool {
    let build = BuildInfo::from_version(&info.executor_version);
    let minimum = BuildInfo::from_version("0.164.0");
    info.capabilities.capability_discovery_v2 && build.version() >= minimum.version()
}
