mod workspace_routing;


#[cfg(test)]
use serial_test::serial;
use std::env;
use std::fmt::Debug;
use std::future::Future;
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::RwLock;
use std::sync::Weak;
use tokio::sync::watch;
use tracing::instrument;

use codex_protocol::auth::AuthMode;
use codex_protocol::config_types::ForcedLoginMethod;

use super::api_key::is_fedramp_api_key;
use super::change_state::AuthChangeState;
use super::change_state::same_login;
use super::change_state::same_owner;
pub use crate::auth::storage::AuthDotJson;
pub use crate::auth::storage::AuthKeyringBackendKind;
use crate::auth::storage::AuthStorageBackend;
use crate::auth::storage::auth_storage_commit_guard;
use crate::auth::storage::create_auth_storage;
use crate::outbound_proxy::AuthRouteConfig;
use codex_config::ManagedAuthPolicy;
use codex_config::types::AuthCredentialsStoreMode;
use codex_http_client::HttpClientFactory;
use thiserror::Error;
pub use workspace_routing::WorkspaceRouting;
pub use workspace_routing::WorkspaceRoutingRequest;
pub use workspace_routing::WorkspaceRoutingResolver;
pub use workspace_routing::WorkspaceRoutingSession;

/// Authentication mechanism used by the current user.
#[derive(Debug, Clone, PartialEq)]
pub enum CodexAuth {
    ApiKey(ApiKeyAuth),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApiKeyAuth {
    api_key: String,
}

/// Error returned when constructing an [`AuthManager`] from resolved configuration.
#[derive(Debug, Error)]
#[error(transparent)]
pub struct AuthManagerInitializationError(#[from] std::io::Error);

impl From<AuthManagerInitializationError> for std::io::Error {
    fn from(error: AuthManagerInitializationError) -> Self {
        error.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalAuthRefreshReason {
    Unauthorized,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalAuthRefreshContext {
    pub reason: ExternalAuthRefreshReason,
    pub previous_account_id: Option<String>,
}

/// Pluggable auth provider used by `AuthManager` for externally managed auth flows.
///
/// Implementations own the current auth value and any source-specific refresh mechanism.
pub trait ExternalAuth: Send + Sync {
    /// Returns the provider's current auth value.
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth>;

    /// Refreshes auth and makes the returned value current for future `resolve()` calls.
    fn refresh(&self, context: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth>;

    /// Maps a provider error into the retry policy used by external-auth reload and recovery.
    fn classify_error(&self, error: std::io::Error) -> std::io::Error {
        error
    }
}

pub type ExternalAuthFuture<'a, T> = Pin<Box<dyn Future<Output = std::io::Result<T>> + Send + 'a>>;

impl CodexAuth {
    fn from_auth_dot_json(auth_dot_json: AuthDotJson) -> std::io::Result<Self> {
        let auth_mode = auth_dot_json.resolved_mode();
        if auth_mode != AuthMode::ApiKey {
            return Err(std::io::Error::other(
                "stored credentials use an authentication mode that is no longer supported; log in with an API key.",
            ));
        }
        let Some(api_key) = auth_dot_json.openai_api_key.as_deref() else {
            return Err(std::io::Error::other("API key auth is missing a key."));
        };
        Ok(Self::from_api_key(api_key))
    }

    pub async fn from_auth_storage(
        codex_home: &Path,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        chatgpt_base_url: Option<&str>,
        keyring_backend_kind: AuthKeyringBackendKind,
        auth_route_config: &AuthRouteConfig,
    ) -> std::io::Result<Option<Self>> {
        let _ = chatgpt_base_url;
        load_auth(
            codex_home,
            /*enable_codex_api_key_env*/ false,
            auth_credentials_store_mode,
            /*allowed_login_methods*/ None,
            /*forced_chatgpt_workspace_id*/ None,
            keyring_backend_kind,
            auth_route_config,
            /*cached_auth*/ None,
            /*allowed_auth_modes*/ None,
        )
        .await
    }

    /// Returns the effective backend auth mode.
    pub fn auth_mode(&self) -> AuthMode {
        AuthMode::ApiKey
    }

    /// Returns the precise kind of credentials backing this authentication.
    pub fn api_auth_mode(&self) -> AuthMode {
        AuthMode::ApiKey
    }

    pub fn is_api_key_auth(&self) -> bool {
        true
    }

    /// Returns `None` if `auth_mode() != AuthMode::ApiKey`.
    pub fn api_key(&self) -> Option<&str> {
        match self {
            Self::ApiKey(auth) => Some(auth.api_key.as_str()),
        }
    }

    /// Returns the token string used for bearer authentication.
    pub fn get_token(&self) -> Result<String, std::io::Error> {
        match self {
            Self::ApiKey(auth) => Ok(auth.api_key.clone()),
        }
    }

    /// Returns whether auth carries a FedRAMP routing hint from an API-key prefix.
    pub fn is_fedramp_account(&self) -> bool {
        match self {
            Self::ApiKey(auth) => is_fedramp_api_key(&auth.api_key),
        }
    }

    pub fn from_api_key(api_key: &str) -> Self {
        Self::ApiKey(ApiKeyAuth {
            api_key: api_key.to_owned(),
        })
    }
}

pub const OPENAI_API_KEY_ENV_VAR: &str = "OPENAI_API_KEY";
pub const CODEX_API_KEY_ENV_VAR: &str = "CODEX_API_KEY";
/// Retained for configuration/telemetry compatibility; token refresh is no longer supported.
pub const REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR: &str = "CODEX_REFRESH_TOKEN_URL_OVERRIDE";

pub fn read_openai_api_key_from_env() -> Option<String> {
    env::var(OPENAI_API_KEY_ENV_VAR)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn read_codex_api_key_from_env() -> Option<String> {
    read_non_empty_env_var(CODEX_API_KEY_ENV_VAR)
}

fn read_non_empty_env_var(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Delete credentials for `codex_home` through the selected storage backend.
/// Returns `Ok(true)` if credentials were removed, `Ok(false)` if none were present.
pub fn logout(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<bool> {
    if auth_credentials_store_mode == AuthCredentialsStoreMode::File && !codex_home.try_exists()? {
        return Ok(false);
    }
    let storage = create_auth_storage(
        codex_home.to_path_buf(),
        auth_credentials_store_mode,
        keyring_backend_kind,
    );
    let _commit = storage.lock_for_write()?;
    storage.delete()
}

/// Store API-key credentials through the selected storage backend.
pub fn login_with_api_key(
    codex_home: &Path,
    api_key: &str,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<()> {
    let auth_dot_json = AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: Some(api_key.to_string()),
    };
    save_auth(
        codex_home,
        &auth_dot_json,
        auth_credentials_store_mode,
        keyring_backend_kind,
    )
}

/// Persist the provided auth payload using the specified backend.
pub fn save_auth(
    codex_home: &Path,
    auth: &AuthDotJson,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<()> {
    let storage = create_auth_storage(
        codex_home.to_path_buf(),
        auth_credentials_store_mode,
        keyring_backend_kind,
    );
    let _commit = storage.lock_for_write()?;
    storage.save(auth)
}

/// Load the raw stored auth payload without applying environment overrides.
///
/// Returns `None` when no credentials are stored. Prefer `AuthManager` for
/// ordinary production reads; this helper is for tests and write-side
/// maintenance that must inspect the exact payload in storage.
pub fn load_auth_dot_json(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<Option<AuthDotJson>> {
    let _commit = auth_storage_commit_guard()?;
    let storage = create_auth_storage(
        codex_home.to_path_buf(),
        auth_credentials_store_mode,
        keyring_backend_kind,
    );
    storage.load()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthConfig {
    pub codex_home: PathBuf,
    pub auth_credentials_store_mode: AuthCredentialsStoreMode,
    pub keyring_backend_kind: AuthKeyringBackendKind,
    pub forced_login_method: Option<ForcedLoginMethod>,
    pub chatgpt_base_url: Option<String>,
    pub forced_chatgpt_workspace_id: Option<Vec<String>>,
    pub managed_auth_policy: ManagedAuthPolicy,
    pub auth_route_config: AuthRouteConfig,
}

impl AuthConfig {
    pub fn is_login_method_allowed(&self, method: ForcedLoginMethod) -> bool {
        self.managed_auth_policy.allows_login_method(
            method,
            self.forced_login_method,
            self.forced_chatgpt_workspace_id.as_deref(),
        )
    }

    pub fn effective_chatgpt_workspaces(&self) -> Option<Vec<String>> {
        self.managed_auth_policy
            .effective_chatgpt_workspaces(self.forced_chatgpt_workspace_id.as_deref())
    }

    pub fn validate(&self) -> std::io::Result<()> {
        if self.is_login_method_allowed(ForcedLoginMethod::Api) {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "authentication requirements do not permit any usable login method",
            ))
        }
    }

    pub fn allows_auth(&self, auth: &CodexAuth) -> bool {
        let allowed_login_methods = self.allowed_login_methods();
        let workspaces = self.effective_chatgpt_workspaces();
        validate_auth_restrictions(Some(&allowed_login_methods), workspaces.as_deref(), auth)
            .is_ok()
    }

    pub async fn load_auth(
        &self,
        enable_codex_api_key_env: bool,
    ) -> std::io::Result<Option<CodexAuth>> {
        self.load_auth_with_modes(enable_codex_api_key_env, /*allowed_auth_modes*/ None)
            .await
    }

    async fn load_auth_with_modes(
        &self,
        enable_codex_api_key_env: bool,
        allowed_auth_modes: Option<&[AuthMode]>,
    ) -> std::io::Result<Option<CodexAuth>> {
        let allowed_login_methods = self.allowed_login_methods();
        let workspaces = self.effective_chatgpt_workspaces();
        let auth = load_auth(
            &self.codex_home,
            enable_codex_api_key_env,
            self.auth_credentials_store_mode,
            Some(&allowed_login_methods),
            workspaces.as_deref(),
            self.keyring_backend_kind,
            &self.auth_route_config,
            /*cached_auth*/ None,
            allowed_auth_modes,
        )
        .await?;
        Ok(auth.filter(|auth| self.allows_auth(auth)))
    }

    fn allowed_login_methods(&self) -> Vec<ForcedLoginMethod> {
        self.managed_auth_policy.allowed_login_methods(
            self.forced_login_method,
            self.forced_chatgpt_workspace_id.as_deref(),
        )
    }
}

fn auth_mode_is_allowed(
    allowed_login_methods: Option<&[ForcedLoginMethod]>,
    mode: AuthMode,
) -> bool {
    mode == AuthMode::ApiKey
        && allowed_login_methods
            .is_none_or(|allowed| allowed.contains(&ForcedLoginMethod::Api))
}

fn validate_auth_restrictions(
    allowed_login_methods: Option<&[ForcedLoginMethod]>,
    _expected_workspaces: Option<&[String]>,
    auth: &CodexAuth,
) -> Result<(), String> {
    if auth_mode_is_allowed(allowed_login_methods, auth.auth_mode()) {
        Ok(())
    } else {
        Err("API key login is required".to_string())
    }
}

/// Enforces configured login restrictions using auth-owned HTTP settings.
pub async fn enforce_login_restrictions(config: &AuthConfig) -> std::io::Result<()> {
    enforce_login_restrictions_inner(config).await
}

async fn enforce_login_restrictions_inner(config: &AuthConfig) -> std::io::Result<()> {
    // Managed-only restrictions are enforced by AuthManager.
    if config.forced_login_method.is_none() && config.forced_chatgpt_workspace_id.is_none() {
        return Ok(());
    }

    let Some(auth) = load_auth(
        &config.codex_home,
        /*enable_codex_api_key_env*/ true,
        config.auth_credentials_store_mode,
        /*allowed_login_methods*/ None,
        /*forced_chatgpt_workspace_id*/ None,
        config.keyring_backend_kind,
        &config.auth_route_config,
        /*cached_auth*/ None,
        /*allowed_auth_modes*/ None,
    )
    .await?
    else {
        return Ok(());
    };

    if let Some(required_method) = config.forced_login_method
        && required_method != ForcedLoginMethod::Api
    {
        return logout_with_message(
            &config.codex_home,
            "ChatGPT login is required, but an API key is currently being used. Logging out."
                .to_string(),
            config.auth_credentials_store_mode,
            config.keyring_backend_kind,
        );
    }

    // API-key auth does not carry a ChatGPT workspace identifier, so workspace restrictions
    // are not applicable.
    let _ = &auth;

    Ok(())
}


fn logout_with_message(
    codex_home: &Path,
    message: String,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<()> {
    // External auth tokens live in the ephemeral store, but persistent auth may still exist
    // from earlier logins. Clear both so a forced logout truly removes all active auth.
    let removal_result = logout_all_stores(
        codex_home,
        auth_credentials_store_mode,
        keyring_backend_kind,
    );
    let error_message = match removal_result {
        Ok(_) => message,
        Err(err) => format!("{message}. Failed to remove stored credentials: {err}"),
    };
    Err(std::io::Error::other(error_message))
}

fn logout_all_stores(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<bool> {
    if auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral {
        return logout(
            codex_home,
            AuthCredentialsStoreMode::Ephemeral,
            AuthKeyringBackendKind::default(),
        );
    }
    let removed_ephemeral = logout(
        codex_home,
        AuthCredentialsStoreMode::Ephemeral,
        AuthKeyringBackendKind::default(),
    )?;
    let removed_managed = logout(
        codex_home,
        auth_credentials_store_mode,
        keyring_backend_kind,
    )?;
    Ok(removed_ephemeral || removed_managed)
}

#[allow(clippy::too_many_arguments)]
async fn load_auth(
    codex_home: &Path,
    enable_codex_api_key_env: bool,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    allowed_login_methods: Option<&[ForcedLoginMethod]>,
    _forced_chatgpt_workspace_id: Option<&[String]>,
    keyring_backend_kind: AuthKeyringBackendKind,
    _auth_route_config: &AuthRouteConfig,
    _cached_auth: Option<&CodexAuth>,
    allowed_auth_modes: Option<&[AuthMode]>,
) -> std::io::Result<Option<CodexAuth>> {
    let mode_allowed = |mode| allowed_auth_modes.is_none_or(|modes| modes.contains(&mode));
    // API key via env var takes precedence over any other auth method.
    if enable_codex_api_key_env
        && mode_allowed(AuthMode::ApiKey)
        && auth_mode_is_allowed(allowed_login_methods, AuthMode::ApiKey)
        && let Some(api_key) = read_codex_api_key_from_env()
    {
        return Ok(Some(CodexAuth::from_api_key(api_key.as_str())));
    }

    let storage = create_auth_storage(
        codex_home.to_path_buf(),
        auth_credentials_store_mode,
        keyring_backend_kind,
    );
    let Some(auth_dot_json) = load_stored_auth(storage).await? else {
        return Ok(None);
    };
    if !mode_allowed(auth_dot_json.resolved_mode())
        || !auth_mode_is_allowed(allowed_login_methods, auth_dot_json.resolved_mode())
    {
        return Ok(None);
    }

    Ok(Some(CodexAuth::from_auth_dot_json(auth_dot_json)?))
}

// Blocking keyring access must not prevent an async caller's deadline from firing.
async fn load_stored_auth(
    storage: Arc<dyn AuthStorageBackend>,
) -> std::io::Result<Option<AuthDotJson>> {
    let originator = codex_otel::auth_storage::AuthStorageOriginator::current();
    tokio::task::spawn_blocking(move || originator.sync_scope(|| storage.load()))
        .await
        .map_err(|_| std::io::Error::other("credential storage read failed"))?
}

// Ordinary auth tolerates legacy metadata loss; EMA grant checks remain strict.

impl AuthDotJson {
    pub(super) fn resolved_mode(&self) -> AuthMode {
        if let Some(mode) = self.auth_mode {
            return mode;
        }
        if self.openai_api_key.is_some() {
            return AuthMode::ApiKey;
        }
        AuthMode::ApiKey
    }
}

struct AuthReload {
    changed: bool,
}

/// Internal cached auth state.
#[derive(Clone)]
struct CachedAuth {
    auth: Option<CodexAuth>,
}

impl Debug for CachedAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedAuth")
            .field(
                "auth_mode",
                &self.auth.as_ref().map(CodexAuth::api_auth_mode),
            )
            .finish()
    }
}

pub struct AuthManager {
    codex_home: PathBuf,
    inner: RwLock<CachedAuth>,
    auth_change_tx: watch::Sender<u64>,
    auth_change_state_tx: watch::Sender<AuthChangeState>,
    workspace_routing_resolver: OnceLock<Weak<dyn WorkspaceRoutingResolver>>,
    enable_codex_api_key_env: bool,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
    forced_login_method: Option<ForcedLoginMethod>,
    forced_chatgpt_workspace_id: RwLock<Option<Vec<String>>>,
    managed_auth_policy: ManagedAuthPolicy,
    chatgpt_base_url: Option<String>,
    external_auth: RwLock<Option<Arc<dyn ExternalAuth>>>,
    auth_route_config: AuthRouteConfig,
    allowed_auth_modes: RwLock<Option<Vec<AuthMode>>>,
}

/// Configuration view required to construct a shared [`AuthManager`].
///
/// Implementations should return the auth-related config values for the
/// already-resolved runtime configuration. The primary implementation is
/// `codex_core::config::Config`, but this trait keeps `codex-login` independent
/// from `codex-core`.
pub trait AuthManagerConfig {
    /// Returns the Codex home directory used for auth storage.
    fn codex_home(&self) -> PathBuf;

    /// Returns the CLI auth credential storage mode for auth loading.
    fn cli_auth_credentials_store_mode(&self) -> AuthCredentialsStoreMode;

    /// Returns the backend to use when CLI auth keyring storage is selected.
    fn auth_keyring_backend_kind(&self) -> AuthKeyringBackendKind;

    /// Returns the resolved login-method restriction, if any.
    fn forced_login_method(&self) -> Option<ForcedLoginMethod>;

    /// Returns the workspace IDs that ChatGPT auth should be restricted to, if any.
    fn forced_chatgpt_workspace_id(&self) -> Option<Vec<String>>;

    /// Returns administrator-managed authentication restrictions.
    fn managed_auth_policy(&self) -> ManagedAuthPolicy;

    /// Returns the ChatGPT backend base URL used for first-party backend authorization.
    fn chatgpt_base_url(&self) -> String;

    /// Returns route-selection settings for auth-owned clients.
    fn auth_route_config(&self) -> AuthRouteConfig;
}

/// Runtime storage and network policy shared by independent credential managers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthRuntimeConfig {
    pub codex_home: PathBuf,
    pub auth_route_config: AuthRouteConfig,
}

impl Debug for AuthManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthManager")
            .field("codex_home", &self.codex_home)
            .field("inner", &self.inner)
            .field("enable_codex_api_key_env", &self.enable_codex_api_key_env)
            .field(
                "auth_credentials_store_mode",
                &self.auth_credentials_store_mode,
            )
            .field("keyring_backend_kind", &self.keyring_backend_kind)
            .field("forced_login_method", &self.forced_login_method)
            .field(
                "forced_chatgpt_workspace_id",
                &self.forced_chatgpt_workspace_id,
            )
            .field("managed_auth_policy", &self.managed_auth_policy)
            .field("chatgpt_base_url", &self.chatgpt_base_url)
            .field("auth_route_config", &self.auth_route_config)
            .field("has_external_auth", &self.has_external_auth())
            .finish_non_exhaustive()
    }
}

impl AuthManager {
    /// Returns the application policy associated with this account owner.
    pub fn application_network_policy(&self) -> codex_http_client::NetworkPolicy {
        self.auth_route_config.application_network_policy().clone()
    }

    /// Creates content clients bound to the account currently owned by this manager.
    pub fn http_client_factory(&self) -> HttpClientFactory {
        self.auth_route_config
            .http_client_factory()
            .clone()
            .with_network_policy(self.application_network_policy().for_current_account())
    }
    /// Create a new manager loading the initial auth using the provided
    /// preferred auth method. Errors loading auth are swallowed; `auth()` will
    /// simply return `None` in that case so callers can treat it as an
    /// unauthenticated state.
    pub async fn new(
        codex_home: PathBuf,
        enable_codex_api_key_env: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        forced_chatgpt_workspace_id: Option<Vec<String>>,
        chatgpt_base_url: Option<String>,
        keyring_backend_kind: AuthKeyringBackendKind,
        auth_route_config: AuthRouteConfig,
    ) -> Self {
        Self::new_from_auth_config(
            AuthConfig {
                codex_home,
                auth_credentials_store_mode,
                keyring_backend_kind,
                forced_login_method: None,
                chatgpt_base_url,
                forced_chatgpt_workspace_id,
                managed_auth_policy: ManagedAuthPolicy::default(),
                auth_route_config,
            },
            enable_codex_api_key_env,
            /*allowed_auth_modes*/ None,
        )
        .await
    }

    async fn new_from_auth_config(
        auth_config: AuthConfig,
        enable_codex_api_key_env: bool,
        allowed_auth_modes: Option<Vec<AuthMode>>,
    ) -> Self {
        let managed_auth = auth_config
            .load_auth_with_modes(enable_codex_api_key_env, allowed_auth_modes.as_deref())
            .await
            .ok()
            .flatten();
        let AuthConfig {
            codex_home,
            auth_credentials_store_mode,
            keyring_backend_kind,
            forced_login_method,
            chatgpt_base_url,
            forced_chatgpt_workspace_id,
            managed_auth_policy,
            auth_route_config,
        } = auth_config;
        let (auth_change_tx, _auth_change_rx) = watch::channel(0);
        Self {
            codex_home,
            inner: RwLock::new(CachedAuth {
                auth: managed_auth,
            }),
            auth_change_tx,
            auth_change_state_tx: watch::channel(AuthChangeState::default()).0,
            workspace_routing_resolver: OnceLock::new(),
            enable_codex_api_key_env,
            auth_credentials_store_mode,
            keyring_backend_kind,
            forced_login_method,
            forced_chatgpt_workspace_id: RwLock::new(forced_chatgpt_workspace_id),
            managed_auth_policy,
            chatgpt_base_url,
            external_auth: RwLock::new(None),
            auth_route_config,
            allowed_auth_modes: RwLock::new(allowed_auth_modes),
        }
    }

    /// Create an AuthManager with a specific CodexAuth, for testing only.
    pub fn from_auth_for_testing(auth: CodexAuth) -> Arc<Self> {
        Self::from_optional_auth_for_testing(Some(auth))
    }

    /// Create an AuthManager with an optional CodexAuth, for testing only.
    pub(crate) fn from_optional_auth_for_testing(auth: Option<CodexAuth>) -> Arc<Self> {
        let cached = CachedAuth {
            auth,
        };
        let (auth_change_tx, _auth_change_rx) = watch::channel(0);

        Arc::new(Self {
            codex_home: PathBuf::from("non-existent"),
            inner: RwLock::new(cached),
            auth_change_tx,
            auth_change_state_tx: watch::channel(AuthChangeState::default()).0,
            workspace_routing_resolver: OnceLock::new(),
            enable_codex_api_key_env: false,
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            keyring_backend_kind: AuthKeyringBackendKind::default(),
            forced_login_method: None,
            forced_chatgpt_workspace_id: RwLock::new(None),
            managed_auth_policy: ManagedAuthPolicy::default(),
            chatgpt_base_url: None,
            external_auth: RwLock::new(None),
            allowed_auth_modes: RwLock::new(None),
            auth_route_config: crate::test_support::transport_default_auth_route_config(),
        })
    }

    /// Create an AuthManager with a specific CodexAuth and codex home, for testing only.
    pub fn from_auth_for_testing_with_home(auth: CodexAuth, codex_home: PathBuf) -> Arc<Self> {
        let cached = CachedAuth {
            auth: Some(auth),
        };
        let (auth_change_tx, _auth_change_rx) = watch::channel(0);
        Arc::new(Self {
            codex_home,
            inner: RwLock::new(cached),
            auth_change_tx,
            auth_change_state_tx: watch::channel(AuthChangeState::default()).0,
            workspace_routing_resolver: OnceLock::new(),
            enable_codex_api_key_env: false,
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            keyring_backend_kind: AuthKeyringBackendKind::default(),
            forced_login_method: None,
            forced_chatgpt_workspace_id: RwLock::new(None),
            managed_auth_policy: ManagedAuthPolicy::default(),
            chatgpt_base_url: None,
            external_auth: RwLock::new(None),
            allowed_auth_modes: RwLock::new(None),
            auth_route_config: crate::test_support::transport_default_auth_route_config(),
        })
    }

    /// Current cached auth (clone) without attempting a refresh.
    pub fn auth_cached(&self) -> Option<CodexAuth> {
        self.inner
            .read()
            .ok()
            .and_then(|cached| cached.auth.clone())
    }

    /// Current revisions if the selected credential still belongs to this manager's source.
    pub fn auth_change_state_for(&self, expected: &CodexAuth) -> Option<AuthChangeState> {
        let cached = self.inner.read().ok()?;
        if !super::source::same_source(cached.auth.as_ref(), Some(expected)) {
            return None;
        }
        Some(*self.auth_change_state_tx.borrow())
    }

    /// Subscribes to cached auth changes that can affect request recovery.
    pub fn auth_change_receiver(&self) -> watch::Receiver<u64> {
        self.auth_change_tx.subscribe()
    }

    /// Subscribes to credential and owner revisions published together, including when changes coalesce.
    pub fn auth_change_state_receiver(&self) -> watch::Receiver<AuthChangeState> {
        self.auth_change_state_tx.subscribe()
    }

    /// Current cached auth (clone). May be `None` if not logged in or load failed.
    #[instrument(level = "trace", skip_all)]
    pub async fn auth(&self) -> Option<CodexAuth> {
        if self.has_external_auth() {
            self.reload().await;
        }
        self.auth_cached()
    }

    /// Refreshes auth, then captures credentials and their account-bound factory together.
    /// The auth read lock prevents an identity change between the two snapshots.
    pub async fn auth_with_http_client_factory(&self) -> Option<(CodexAuth, HttpClientFactory)> {
        self.auth().await;
        let cached = self.inner.read().ok()?;
        Some((cached.auth.clone()?, self.http_client_factory()))
    }


    /// Reloads auth from the active source. Returns whether the auth value changed.
    pub async fn reload(&self) -> bool {
        tracing::info!("Reloading auth");
        loop {
            let generation = self.auth_change_state_tx.borrow().generation;
            let new_auth = self.load_auth(/*cached_auth*/ None).await.ok().flatten();
            if let Some(publication) = self.publish_reloaded_auth(generation, new_auth) {
                return publication.changed;
            }
            if self.inner.is_poisoned() {
                return false;
            }
        }
    }

    /// Pins the stored credential kind before any subsequent load or hydration.
    /// This prevents a running client from silently acquiring a different kind of secret.
    pub fn restrict_auth_modes(&self, mut modes: Vec<AuthMode>) -> std::io::Result<()> {
        let mut allowed = self
            .allowed_auth_modes
            .write()
            .map_err(|_| std::io::Error::other("credential policy is unavailable"))?;
        if let Some(previous) = allowed.as_ref() {
            modes.retain(|mode| previous.contains(mode));
        }
        *allowed = Some(modes);
        Ok(())
    }

    /// Reloads stored credentials without hiding storage or hydration failures.
    /// Failed loads retain the last known owner so a temporary outage does not
    /// invalidate application network policy as if the user had logged out.
    /// Callers must serialize reload and refresh and reject cached headers on error.
    pub async fn reload_with_cached_auth(&self) -> std::io::Result<bool> {
        loop {
            let generation = self.auth_change_state_tx.borrow().generation;
            let cached = self.auth_cached();
            let new_auth = self.try_load_managed_auth(cached.as_ref()).await?;
            if let Some(publication) = self.publish_reloaded_auth(generation, new_auth) {
                return Ok(publication.changed);
            }
            if self.inner.is_poisoned() {
                return Err(std::io::Error::other("credential cache is unavailable"));
            }
        }
    }


    fn auths_equal(a: Option<&CodexAuth>, b: Option<&CodexAuth>) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }


    async fn load_auth(
        &self,
        cached_auth: Option<&CodexAuth>,
    ) -> std::io::Result<Option<CodexAuth>> {
        if let Some(external_auth) = self.external_auth_provider() {
            let cached_auth = self.auth_cached();
            let auth = match self.resolve_external_auth(external_auth.as_ref()).await {
                Ok(auth) => Some(auth),
                Err(err) => {
                    tracing::error!("Failed to resolve external auth: {err}");
                    cached_auth
                }
            };
            return Ok(auth);
        }

        self.try_load_managed_auth(cached_auth).await
    }

    async fn try_load_managed_auth(
        &self,
        cached_auth: Option<&CodexAuth>,
    ) -> std::io::Result<Option<CodexAuth>> {
        let allowed_auth_modes = self
            .allowed_auth_modes
            .read()
            .map_err(|_| std::io::Error::other("credential policy is unavailable"))?
            .clone();
        let allowed_login_methods = self.allowed_login_methods();
        let effective_chatgpt_workspaces = self.effective_chatgpt_workspaces();
        let auth = load_auth(
            &self.codex_home,
            self.enable_codex_api_key_env,
            self.auth_credentials_store_mode,
            Some(&allowed_login_methods),
            effective_chatgpt_workspaces.as_deref(),
            self.keyring_backend_kind,
            &self.auth_route_config,
            cached_auth,
            allowed_auth_modes.as_deref(),
        )
        .await?;
        Ok(auth.filter(|auth| {
            validate_auth_restrictions(
                Some(&allowed_login_methods),
                effective_chatgpt_workspaces.as_deref(),
                auth,
            )
            .is_ok()
        }))
    }

    fn set_cached_auth(&self, new_auth: Option<CodexAuth>) -> bool {
        let Ok(mut guard) = self.inner.write() else {
            return false;
        };
        self.set_cached_auth_locked(&mut guard, new_auth)
    }

    fn publish_reloaded_auth(
        &self,
        generation: u64,
        auth: Option<CodexAuth>,
    ) -> Option<AuthReload> {
        let mut guard = self.inner.write().ok()?;
        if self.auth_change_state_tx.borrow().generation != generation {
            return None;
        }
        let changed = self.set_cached_auth_locked(&mut guard, auth);
        Some(AuthReload { changed })
    }

    fn invalidate_pending_auth_reads(&self) {
        if let Ok(_guard) = self.inner.write() {
            self.auth_change_state_tx
                .send_modify(|state| state.generation += 1);
        }
    }

    fn set_cached_auth_locked(&self, guard: &mut CachedAuth, new_auth: Option<CodexAuth>) -> bool {
        let previous = guard.auth.as_ref();
        let changed = !AuthManager::auths_equal(previous, new_auth.as_ref());
        let owner_changed = changed && !same_owner(previous, new_auth.as_ref());
        let source_changed = changed && !super::source::same_source(previous, new_auth.as_ref());
        let login_changed = changed && !same_login(previous, new_auth.as_ref());
        if owner_changed {
            self.auth_route_config
                .application_network_policy()
                .invalidate();
        }
        guard.auth = new_auth;
        if changed {
            self.auth_change_state_tx.send_modify(|state| {
                state.generation += 1;
                if owner_changed {
                    state.owner_generation += 1;
                }
                if source_changed {
                    state.source_generation += 1;
                }
                if login_changed {
                    state.login_generation += 1;
                }
            });
            self.auth_change_tx.send_modify(|revision| *revision += 1);
        }
        tracing::info!(changed, "Reloaded auth");
        changed
    }

    fn external_auth_provider(&self) -> Option<Arc<dyn ExternalAuth>> {
        self.external_auth
            .read()
            .ok()
            .and_then(|external_auth| external_auth.clone())
    }

    pub async fn set_external_auth(
        &self,
        external_auth: Arc<dyn ExternalAuth>,
    ) -> std::io::Result<()> {
        self.install_external_auth(external_auth).await
    }

    async fn install_external_auth(
        &self,
        external_auth: Arc<dyn ExternalAuth>,
    ) -> std::io::Result<()> {
        let auth = self.resolve_external_auth(external_auth.as_ref()).await?;
        let mut external_auth_slot = self
            .external_auth
            .write()
            .map_err(|_| std::io::Error::other("external auth lock is poisoned"))?;
        *external_auth_slot = Some(external_auth);
        drop(external_auth_slot);
        self.commit_external_auth(auth)
    }

    pub fn clear_external_auth(&self) {
        if let Ok(mut external_auth) = self.external_auth.write()
            && external_auth.take().is_some()
        {
            self.set_cached_auth(/*new_auth*/ None);
        }
    }

    pub fn set_forced_chatgpt_workspace_id(&self, workspace_id: Option<Vec<String>>) {
        if let Ok(mut guard) = self.forced_chatgpt_workspace_id.write()
            && *guard != workspace_id
        {
            *guard = workspace_id;
        }
    }

    pub fn forced_chatgpt_workspace_id(&self) -> Option<Vec<String>> {
        self.forced_chatgpt_workspace_id
            .read()
            .ok()
            .and_then(|guard| guard.clone())
    }

    pub fn effective_chatgpt_workspaces(&self) -> Option<Vec<String>> {
        self.managed_auth_policy
            .effective_chatgpt_workspaces(self.forced_chatgpt_workspace_id().as_deref())
    }

    pub fn is_login_method_allowed(&self, method: ForcedLoginMethod) -> bool {
        self.managed_auth_policy.allows_login_method(
            method,
            self.forced_login_method,
            self.forced_chatgpt_workspace_id().as_deref(),
        )
    }

    /// Returns the login methods permitted by the current effective authentication policy.
    pub fn allowed_login_methods(&self) -> Vec<ForcedLoginMethod> {
        self.managed_auth_policy.allowed_login_methods(
            self.forced_login_method,
            self.forced_chatgpt_workspace_id().as_deref(),
        )
    }

    pub fn has_external_auth(&self) -> bool {
        self.external_auth_provider().is_some()
    }

    pub fn codex_api_key_env_enabled(&self) -> bool {
        self.enable_codex_api_key_env
    }

    /// Returns policy only; independent credential managers own their own state and lifecycle.
    pub fn runtime_config(&self) -> AuthRuntimeConfig {
        AuthRuntimeConfig {
            codex_home: self.codex_home.clone(),
            auth_route_config: self.auth_route_config.clone(),
        }
    }

    /// Convenience constructor returning an `Arc` wrapper.
    pub async fn shared(
        codex_home: PathBuf,
        enable_codex_api_key_env: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        forced_chatgpt_workspace_id: Option<Vec<String>>,
        chatgpt_base_url: Option<String>,
        keyring_backend_kind: AuthKeyringBackendKind,
        auth_route_config: AuthRouteConfig,
    ) -> Arc<Self> {
        Arc::new(
            Self::new(
                codex_home,
                enable_codex_api_key_env,
                auth_credentials_store_mode,
                forced_chatgpt_workspace_id,
                chatgpt_base_url,
                keyring_backend_kind,
                auth_route_config,
            )
            .await,
        )
    }

    /// Builds a shared manager and activates process-configured workload identity when selected.
    pub async fn shared_from_config(
        config: &impl AuthManagerConfig,
        enable_codex_api_key_env: bool,
    ) -> Result<Arc<Self>, AuthManagerInitializationError> {
        Self::shared_from_auth_config(auth_config_from(config), enable_codex_api_key_env).await
    }

    /// Activates workload identity against an auth config resolved before full runtime config.
    pub async fn shared_from_auth_config(
        auth_config: AuthConfig,
        enable_codex_api_key_env: bool,
    ) -> Result<Arc<Self>, AuthManagerInitializationError> {
        Self::shared_from_auth_config_with_modes(
            auth_config,
            enable_codex_api_key_env,
            /*allowed_auth_modes*/ None,
        )
        .await
    }

    /// Restricts credential modes before loading or hydrating the initial credential.
    pub async fn shared_from_config_with_auth_modes(
        config: &impl AuthManagerConfig,
        enable_codex_api_key_env: bool,
        allowed_auth_modes: Vec<AuthMode>,
    ) -> Result<Arc<Self>, AuthManagerInitializationError> {
        Self::shared_from_auth_config_with_auth_modes(
            auth_config_from(config),
            enable_codex_api_key_env,
            allowed_auth_modes,
        )
        .await
    }

    /// Restricts credential modes during bootstrap, before loading runtime configuration.
    pub async fn shared_from_auth_config_with_auth_modes(
        auth_config: AuthConfig,
        enable_codex_api_key_env: bool,
        allowed_auth_modes: Vec<AuthMode>,
    ) -> Result<Arc<Self>, AuthManagerInitializationError> {
        Self::shared_from_auth_config_with_modes(
            auth_config,
            enable_codex_api_key_env,
            Some(allowed_auth_modes),
        )
        .await
    }

    async fn shared_from_auth_config_with_modes(
        auth_config: AuthConfig,
        enable_codex_api_key_env: bool,
        allowed_auth_modes: Option<Vec<AuthMode>>,
    ) -> Result<Arc<Self>, AuthManagerInitializationError> {
        let manager =
            Self::new_from_auth_config(auth_config, enable_codex_api_key_env, allowed_auth_modes)
                .await;
        Ok(Arc::new(manager))
    }


    async fn resolve_external_auth(
        &self,
        external_auth: &dyn ExternalAuth,
    ) -> std::io::Result<CodexAuth> {
        let auth = external_auth.resolve().await.map_err(|error| external_auth.classify_error(error))?;
        self.validate_external_auth(&auth, external_auth)?;
        Ok(auth)
    }

    pub async fn logout(&self) -> std::io::Result<bool> {
        self.ensure_logout_allowed()?;
        let removed = logout_all_stores(
            &self.codex_home,
            self.auth_credentials_store_mode,
            self.keyring_backend_kind,
        )?;
        // Always reload to clear cached auth, even if no stored credentials were present.
        self.invalidate_pending_auth_reads();
        self.clear_external_auth();
        self.reload().await;
        Ok(removed)
    }

    fn ensure_logout_allowed(&self) -> std::io::Result<()> {
        Ok(())
    }

    /// Returns the precise kind of credentials backing the current authentication.
    pub fn get_api_auth_mode(&self) -> Option<AuthMode> {
        self.auth_cached().as_ref().map(CodexAuth::api_auth_mode)
    }

    /// Returns the effective backend auth mode for the current authentication.
    pub fn auth_mode(&self) -> Option<AuthMode> {
        self.auth_cached().as_ref().map(CodexAuth::auth_mode)
    }

    fn commit_external_auth(&self, auth: CodexAuth) -> std::io::Result<()> {
        self.set_cached_auth(Some(auth));
        Ok(())
    }

    fn validate_external_auth(
        &self,
        auth: &CodexAuth,
        external_auth: &dyn ExternalAuth,
    ) -> std::io::Result<()> {
        let allowed_login_methods = self.allowed_login_methods();
        validate_auth_restrictions(
            Some(&allowed_login_methods),
            self.effective_chatgpt_workspaces().as_deref(),
            auth,
        )
        .map_err(|error| external_auth.classify_error(std::io::Error::other(error)))
    }

}

fn auth_config_from(config: &impl AuthManagerConfig) -> AuthConfig {
    AuthConfig {
        codex_home: config.codex_home(),
        auth_credentials_store_mode: config.cli_auth_credentials_store_mode(),
        keyring_backend_kind: config.auth_keyring_backend_kind(),
        forced_login_method: config.forced_login_method(),
        chatgpt_base_url: Some(config.chatgpt_base_url()),
        forced_chatgpt_workspace_id: config.forced_chatgpt_workspace_id(),
        managed_auth_policy: config.managed_auth_policy(),
        auth_route_config: config.auth_route_config(),
    }
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
