use super::*;
use crate::auth::storage::FileAuthStorage;
use crate::auth::storage::get_auth_file;
use codex_http_client::OutboundProxyPolicy;
use codex_protocol::auth::AuthMode;

use codex_protocol::config_types::ForcedLoginMethod;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::Arc;
use tempfile::tempdir;

const WORKSPACE_ID_ALLOWED: &str = "123e4567-e89b-42d3-a456-426614174000";

struct StaticExternalAuth(CodexAuth);

impl ExternalAuth for StaticExternalAuth {
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async { Ok(self.0.clone()) })
    }

    fn refresh(&self, _context: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async { Ok(self.0.clone()) })
    }
}

#[test]
fn login_with_api_key_overwrites_existing_auth_json() {
    let dir = tempdir().unwrap();
    let auth_path = dir.path().join("auth.json");
    let stale_auth = json!({
        "OPENAI_API_KEY": "sk-old",
        "tokens": {
            "id_token": "stale.header.payload",
            "access_token": "stale-access",
            "refresh_token": "stale-refresh",
            "account_id": "stale-acc"
        }
    });
    for api_key in [
        "sk-new",
        "sk-proj-fedramp-synthetic-test-key",
        "sk-svcacct-fedramp-synthetic-test-key",
    ] {
        std::fs::write(
            &auth_path,
            serde_json::to_string_pretty(&stale_auth).unwrap(),
        )
        .unwrap();

        super::login_with_api_key(
            dir.path(),
            api_key,
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("login_with_api_key should succeed");

        let storage = FileAuthStorage::new(dir.path().to_path_buf());
        let auth = storage
            .try_read_auth_json(&auth_path)
            .expect("auth.json should parse");
        assert_eq!(auth.openai_api_key.as_deref(), Some(api_key));
    }
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn missing_auth_json_returns_none() {
    let dir = tempdir().unwrap();
    let auth = CodexAuth::from_auth_storage(
        dir.path(),
        AuthCredentialsStoreMode::File,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .expect("call should succeed");
    assert_eq!(auth, None);
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn loads_api_key_from_auth_json() {
    let dir = tempdir().unwrap();
    let auth_file = dir.path().join("auth.json");
    for (api_key, expected_fedramp) in [
        ("sk-test-key", false),
        ("sk-proj-fedramp-synthetic-test-key", true),
        ("sk-svcacct-fedramp-synthetic-test-key", true),
        ("sk-proj-synthetic-test-key", false),
        ("sk-svcacct-synthetic-test-key", false),
        ("sk-proj-fedramp", false),
        ("sk-svcacct-fedrampish-test-key", false),
        ("sk-proj-FEDRAMP-test-key", false),
        ("sk-admin-fedramp-test-key", false),
        ("other-sk-proj-fedramp-test-key", false),
    ] {
        std::fs::write(
            &auth_file,
            json!({"OPENAI_API_KEY": api_key, "tokens": null, "last_refresh": null}).to_string(),
        )
        .unwrap();

        let auth = super::load_auth(
            dir.path(),
            /*enable_codex_api_key_env*/ false,
            AuthCredentialsStoreMode::File,
            /*allowed_login_methods*/ None,
            /*forced_chatgpt_workspace_id*/ None,
            AuthKeyringBackendKind::Direct,
            &crate::test_support::transport_default_auth_route_config(),
            /*cached_auth*/ None,
            /*allowed_auth_modes*/ None,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(auth.auth_mode(), AuthMode::ApiKey);
        assert_eq!(auth.api_key(), Some(api_key));
        assert_eq!(
            (auth.get_token().unwrap(), auth.is_fedramp_account()),
            (api_key.to_string(), expected_fedramp)
        );

    }
}

#[test]
fn logout_does_not_create_a_missing_file_store() -> std::io::Result<()> {
    let root = tempdir()?;
    let missing_home = root.path().join("missing");
    assert!(!logout(
        &missing_home,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default()
    )?);
    assert!(!missing_home.exists());
    Ok(())
}

#[test]
fn logout_removes_auth_file() -> Result<(), std::io::Error> {
    let dir = tempdir()?;
    let auth_dot_json = AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: Some("sk-test-key".to_string()),
    };
    super::save_auth(
        dir.path(),
        &auth_dot_json,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;
    let auth_file = get_auth_file(dir.path());
    assert!(auth_file.exists());
    assert!(logout(
        dir.path(),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?);
    assert!(!auth_file.exists());
    Ok(())
}

async fn build_config(
    codex_home: &Path,
    forced_login_method: Option<ForcedLoginMethod>,
    forced_chatgpt_workspace_id: Option<Vec<String>>,
) -> AuthConfig {
    AuthConfig {
        codex_home: codex_home.to_path_buf(),
        auth_credentials_store_mode: AuthCredentialsStoreMode::File,
        keyring_backend_kind: AuthKeyringBackendKind::Direct,
        forced_login_method,
        forced_chatgpt_workspace_id,
        managed_auth_policy: ManagedAuthPolicy::default(),
        chatgpt_base_url: None,
        auth_route_config: crate::test_support::transport_default_auth_route_config(),
    }
}

/// Use sparingly.
/// TODO (gpeal): replace this with an injectable env var provider.
#[cfg(test)]
struct EnvVarGuard {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

#[cfg(test)]
impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let original = env::var_os(key);
        unsafe {
            env::set_var(key, value);
        }
        Self { key, original }
    }

    fn remove(key: &'static str) -> Self {
        let original = env::var_os(key);
        unsafe {
            env::remove_var(key);
        }
        Self { key, original }
    }
}

#[cfg(test)]
impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.original {
                Some(value) => env::set_var(self.key, value),
                None => env::remove_var(self.key),
            }
        }
    }
}


struct TestAuthManagerConfig(AuthConfig);

impl AuthManagerConfig for TestAuthManagerConfig {
    fn codex_home(&self) -> PathBuf {
        self.0.codex_home.clone()
    }

    fn cli_auth_credentials_store_mode(&self) -> AuthCredentialsStoreMode {
        self.0.auth_credentials_store_mode
    }

    fn auth_keyring_backend_kind(&self) -> AuthKeyringBackendKind {
        self.0.keyring_backend_kind
    }

    fn forced_login_method(&self) -> Option<ForcedLoginMethod> {
        self.0.forced_login_method
    }

    fn forced_chatgpt_workspace_id(&self) -> Option<Vec<String>> {
        self.0.forced_chatgpt_workspace_id.clone()
    }

    fn managed_auth_policy(&self) -> ManagedAuthPolicy {
        self.0.managed_auth_policy.clone()
    }

    fn chatgpt_base_url(&self) -> String {
        self.0
            .chatgpt_base_url
            .clone()
            .expect("test config should include a ChatGPT base URL")
    }

    fn auth_route_config(&self) -> AuthRouteConfig {
        self.0.auth_route_config.clone()
    }
}

fn test_auth_manager_config(codex_home: &Path) -> TestAuthManagerConfig {
    TestAuthManagerConfig(AuthConfig {
        codex_home: codex_home.to_path_buf(),
        auth_credentials_store_mode: AuthCredentialsStoreMode::File,
        keyring_backend_kind: AuthKeyringBackendKind::Direct,
        forced_login_method: Some(ForcedLoginMethod::Chatgpt),
        chatgpt_base_url: Some("https://chatgpt-staging.com/backend-api".to_string()),
        forced_chatgpt_workspace_id: Some(vec!["forced-workspace".to_string()]),
        managed_auth_policy: ManagedAuthPolicy {
            allowed_login_methods: Some(vec![ForcedLoginMethod::Chatgpt]),
            allowed_chatgpt_workspaces: Some(vec![
                "forced-workspace".to_string(),
                "managed-workspace".to_string(),
            ]),
        },
        auth_route_config: AuthRouteConfig::from_http_client_factory(HttpClientFactory::new(
            OutboundProxyPolicy::RespectSystemProxy,
        )),
    })
}

#[test]
fn auth_config_from_preserves_all_fields() {
    let codex_home = tempdir().expect("tempdir");
    let config = test_auth_manager_config(codex_home.path());

    assert_eq!(auth_config_from(&config), config.0);
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn load_auth_keeps_codex_api_key_env_precedence() {
    let codex_home = tempdir().unwrap();
    let _api_key_guard = EnvVarGuard::set(CODEX_API_KEY_ENV_VAR, "sk-env");

    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ true,
        AuthCredentialsStoreMode::File,
        /*allowed_login_methods*/ None,
        /*forced_chatgpt_workspace_id*/ None,
        AuthKeyringBackendKind::Direct,
        &crate::test_support::transport_default_auth_route_config(),
        /*cached_auth*/ None,
        /*allowed_auth_modes*/ None,
    )
    .await
    .expect("env auth should load")
    .expect("env auth should be present");

    assert_eq!(auth.api_key(), Some("sk-env"));
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn enforce_login_restrictions_logs_out_for_method_mismatch() {
    let codex_home = tempdir().unwrap();
    login_with_api_key(
        codex_home.path(),
        "sk-test",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("seed api key");

    let config = build_config(
        codex_home.path(),
        Some(ForcedLoginMethod::Chatgpt),
        /*forced_chatgpt_workspace_id*/ None,
    )
    .await;

    let err = super::enforce_login_restrictions(&config)
        .await
        .expect_err("expected method mismatch to error");
    assert!(err.to_string().contains("ChatGPT login is required"));
    assert!(
        !codex_home.path().join("auth.json").exists(),
        "auth.json should be removed on mismatch"
    );
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn auth_manager_rejects_disallowed_stored_and_external_auth() {
    let codex_home = tempdir().unwrap();
    login_with_api_key(
        codex_home.path(),
        "sk-test",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("seed api key");
    let mut config = build_config(
        codex_home.path(),
        /*forced_login_method*/ None,
        /*forced_chatgpt_workspace_id*/ None,
    )
    .await;
    config.managed_auth_policy.allowed_login_methods = Some(vec![ForcedLoginMethod::Chatgpt]);
    let manager =
        AuthManager::shared_from_auth_config(config, /*enable_codex_api_key_env*/ false)
            .await
            .expect("auth manager");

    assert_eq!(manager.auth().await, None);
    assert!(
        manager
            .set_external_auth(Arc::new(StaticExternalAuth(CodexAuth::from_api_key(
                "sk-external",
            ))))
            .await
            .is_err(),
        "external auth cannot bypass managed login policy"
    );
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn enforce_login_restrictions_allows_api_key_if_login_method_not_set_but_forced_chatgpt_workspace_id_is_set()
 {
    let codex_home = tempdir().unwrap();
    login_with_api_key(
        codex_home.path(),
        "sk-test",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("seed api key");

    let config = build_config(
        codex_home.path(),
        /*forced_login_method*/ None,
        Some(vec![WORKSPACE_ID_ALLOWED.to_string()]),
    )
    .await;

    super::enforce_login_restrictions(&config)
        .await
        .expect("matching workspace should succeed");
    assert!(
        codex_home.path().join("auth.json").exists(),
        "auth.json should remain when restrictions pass"
    );
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn enforce_login_restrictions_blocks_env_api_key_when_chatgpt_required() {
    let _guard = EnvVarGuard::set(CODEX_API_KEY_ENV_VAR, "sk-env");
    let codex_home = tempdir().unwrap();

    let config = build_config(
        codex_home.path(),
        Some(ForcedLoginMethod::Chatgpt),
        /*forced_chatgpt_workspace_id*/ None,
    )
    .await;

    let err = super::enforce_login_restrictions(&config)
        .await
        .expect_err("environment API key should not satisfy forced ChatGPT login");
    assert!(
        err.to_string()
            .contains("ChatGPT login is required, but an API key is currently being used.")
    );
}

