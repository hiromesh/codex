use codex_http_client::HttpClientFactory;

use crate::EnvironmentManager;
use crate::ExecServerError;
use crate::ExecServerRuntimeOptions;

/// Holds discovered execution-environment availability before the HTTP policy is resolved.
///
/// Preparing environments does not start any connection. Callers can inspect
/// the default environment selection to choose config-loading behavior and then
/// build the manager with the effective outbound HTTP policy.
#[derive(Debug)]
pub struct PreparedEnvironmentManager {
    include_local: bool,
}

impl PreparedEnvironmentManager {
    pub(crate) fn new(include_local: bool) -> Self {
        Self { include_local }
    }

    /// Returns whether the discovered default environment is remote.
    ///
    /// Remote execution environments have been removed, so this is always false.
    pub fn default_environment_is_remote(&self) -> bool {
        false
    }

    /// Builds the manager using the supplied policy.
    pub fn build(
        self,
        local_runtime_paths: Option<ExecServerRuntimeOptions>,
        http_client_factory: HttpClientFactory,
    ) -> Result<EnvironmentManager, ExecServerError> {
        Ok(EnvironmentManager::from_local_preparation(
            self.include_local,
            local_runtime_paths,
            http_client_factory,
        ))
    }
}
