use super::*;
use crate::auth::storage::FileAuthStorage;
use crate::auth::storage::get_auth_file;
use crate::token_data::IdTokenInfo;
use codex_protocol::account::PlanType as AccountPlanType;
use codex_protocol::auth::AuthMode;
use codex_protocol::auth::KnownPlan as InternalKnownPlan;
use codex_protocol::auth::PlanType as InternalPlanType;

use anyhow::Context;
use base64::Engine;
use codex_protocol::config_types::ForcedLoginMethod;
use pretty_assertions::assert_eq;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use tempfile::TempDir;
use tempfile::tempdir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

const WORKSPACE_ID_ALLOWED: &str = "123e4567-e89b-42d3-a456-426614174000";
const WORKSPACE_ID_SECOND_ALLOWED: &str = "123e4567-e89b-42d3-a456-426614174001";
const WORKSPACE_ID_DISALLOWED: &str = "123e4567-e89b-42d3-a456-426614174002";

#[test]
fn header_auth_exposes_a_valid_chatgpt_account_id() {
    for (account_id_header, expected_account_id) in [
        (
            Some(WORKSPACE_ID_ALLOWED.as_bytes()),
            Some(WORKSPACE_ID_ALLOWED),
        ),
        (None, None),
        (Some(b""), None),
        (Some(b" account "), None),
        (Some(&[0xff]), None),
    ] {
        let mut headers = http::HeaderMap::new();
        if let Some(account_id_header) = account_id_header {
            headers.insert(
                "chatgpt-account-id",
                http::HeaderValue::from_bytes(account_id_header).expect("valid header bytes"),
            );
        }

        let auth = CodexAuth::Headers(AuthHeaders::new(headers));
        assert_eq!(auth.get_account_id().as_deref(), expected_account_id);
    }
}

#[tokio::test]
async fn cancelled_refresh_marks_only_started_exchanges_as_unsettled() -> anyhow::Result<()> {
    let home = tempdir()?;
    write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".into()),
            chatgpt_account_id: Some(WORKSPACE_ID_ALLOWED.into()),
        },
        home.path(),
    )?;
    let server = MockServer::start().await;
    let storage = FileAuthStorage::new(home.path().to_path_buf());
    let mut stored = storage.load()?.context("stored credentials")?;
    let tokens = stored.tokens.as_mut().context("stored tokens")?;
    tokens.oauth_provenance = Some(AccountOAuthProvenance {
        issuer: server.uri(),
        client_id: "codex-client".into(),
    });
    // Workspace switching retains the original ID and refresh tokens.
    tokens.account_id = Some(WORKSPACE_ID_SECOND_ALLOWED.into());
    tokens.access_token = fake_jwt_for_auth_file_params(&AuthFileParams {
        openai_api_key: None,
        chatgpt_plan_type: Some("business".into()),
        chatgpt_account_id: Some(WORKSPACE_ID_SECOND_ALLOWED.into()),
    })?;
    storage.save(&stored)?;
    let manager = Arc::new(
        AuthManager::new_from_auth_config(
            build_config(
                home.path(),
                /*forced_login_method*/ None,
                /*forced_chatgpt_workspace_id*/ None,
            )
            .await,
            /*enable_codex_api_key_env*/ false,
            /*allowed_auth_modes*/ None,
        )
        .await,
    );
    let initial = manager.auth_cached().context("initial auth")?;

    // Poll directly while the guard is held, then cancel before any OAuth request.
    let guard = manager.refresh_lock.acquire().await?;
    let mut waiting = Box::pin(manager.refresh_token_if_unchanged(&initial));
    assert!(
        waiting
            .as_mut()
            .poll(&mut std::task::Context::from_waker(
                std::task::Waker::noop(),
            ))
            .is_pending()
    );
    drop(waiting);
    assert!(!manager.is_chatgpt_refresh_unsettled(&initial)?);
    assert!(server.received_requests().await.unwrap().is_empty());
    drop(guard);

    let received = Arc::new(tokio::sync::Notify::new());
    let notify = Arc::clone(&received);
    Mock::given(path("/oauth/token"))
        .respond_with(move |_: &wiremock::Request| {
            notify.notify_one();
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(60))
                .set_body_json(json!({"access_token": "late", "refresh_token": "late-refresh"}))
        })
        .expect(1)
        .mount(&server)
        .await;
    let refreshing = Arc::clone(&manager);
    let task = tokio::spawn(async move { refreshing.refresh_token_from_authority().await });
    received.notified().await;
    assert!(manager.is_chatgpt_refresh_unsettled(&initial)?);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(manager.is_chatgpt_refresh_unsettled(&initial)?);

    // A later known failure cannot resolve an earlier cancelled exchange.
    server.reset().await;
    Mock::given(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;
    assert!(manager.refresh_token_from_authority().await.is_err());
    assert!(manager.is_chatgpt_refresh_unsettled(&initial)?);

    // A replacement is usable, and a completed refresh settles its own generation.
    server.reset().await;
    Mock::given(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "completed", "refresh_token": "completed-refresh",
        })))
        .expect(1)
        .mount(&server)
        .await;
    let tokens = stored.tokens.as_mut().context("replacement tokens")?;
    tokens.access_token = "replacement".into();
    tokens.refresh_token = "replacement-refresh".into();
    storage.save(&stored)?;
    manager.reload().await;
    let replacement = manager.auth_cached().context("replacement auth")?;
    assert!(!manager.is_chatgpt_refresh_unsettled(&replacement)?);
    manager.refresh_token_from_authority().await?;
    assert!(!manager.is_chatgpt_refresh_unsettled(&replacement)?);
    assert_eq!(
        manager
            .auth_cached()
            .context("refreshed auth")?
            .get_token()?,
        "completed"
    );
    Ok(())
}

#[tokio::test]
async fn refresh_without_id_token() -> anyhow::Result<()> {
    let codex_home = tempdir()?;
    let fake_jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: None,
        },
        codex_home.path(),
    )?;

    let storage = create_auth_storage(
        codex_home.path().to_path_buf(),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    );
    let provenance = AccountOAuthProvenance {
        issuer: "https://idp.example".to_string(),
        client_id: "codex-client".to_string(),
    };
    let initial = storage
        .load()?
        .context("stored auth")?
        .tokens
        .context("stored tokens")?;
    let updated = super::persist_tokens(
        &storage,
        &initial,
        Some(provenance.clone()),
        /*id_token*/ None,
        Some("new-access-token".to_string()),
        Some("new-refresh-token".to_string()),
    )?;

    let tokens = updated.tokens.context("persisted auth is missing tokens")?;
    assert_eq!(tokens.id_token.raw_jwt, fake_jwt);
    assert_eq!(tokens.access_token, "new-access-token");
    assert_eq!(tokens.refresh_token, "new-refresh-token");
    assert_eq!(tokens.oauth_provenance, Some(provenance.clone()));

    let updated = super::persist_tokens(
        &storage,
        &tokens,
        /*provenance*/ None,
        /*id_token*/ None,
        Some("later-access-token".to_string()),
        /*refresh_token*/ None,
    )?;
    let previous = tokens;
    let tokens = updated.tokens.context("persisted auth is missing tokens")?;
    assert_eq!(tokens.refresh_token, "new-refresh-token");
    assert_eq!(tokens.oauth_provenance, None);

    // Reject this stale refresh response: newer tokens have already been saved
    // for the same account.
    let before = storage.load()?;
    assert!(
        super::persist_tokens(
            &storage,
            &previous,
            Some(provenance),
            /*id_token*/ None,
            Some("stale-access-token".into()),
            Some("stale-refresh-token".into()),
        )
        .is_err()
    );
    assert_eq!(storage.load()?, before);

    Ok(())
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
        assert!(auth.tokens.is_none(), "tokens should be cleared");
    }
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn missing_auth_json_returns_none() {
    let dir = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
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
async fn pro_account_with_no_api_key_uses_chatgpt_auth() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let fake_jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: None,
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let auth = super::load_auth(
        codex_home.path(),
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
    assert_eq!(None, auth.api_key());
    assert_eq!(AuthMode::Chatgpt, auth.auth_mode());
    assert_eq!(auth.get_chatgpt_user_id().as_deref(), Some("user-12345"));

    let auth_dot_json = auth
        .get_current_auth_json()
        .expect("AuthDotJson should exist");
    let last_refresh = auth_dot_json
        .last_refresh
        .expect("last_refresh should be recorded");

    assert_eq!(
        AuthDotJson {
            auth_mode: None,
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: IdTokenInfo {
                    email: Some("user@example.com".to_string()),
                    chatgpt_plan_type: Some(InternalPlanType::Known(InternalKnownPlan::Pro)),
                    chatgpt_user_id: Some("user-12345".to_string()),
                    chatgpt_account_id: None,
                    chatgpt_account_is_fedramp: false,
                    raw_jwt: fake_jwt,
                },
                access_token: "test-access-token".to_string(),
                refresh_token: "test-refresh-token".to_string(),
                oauth_provenance: None,
                login_owner_id: None,
                account_id: None,
            }),
            last_refresh: Some(last_refresh),
            personal_access_token: None,
            bedrock_api_key: None,
            bedrock_access_keys: None,
        },
        auth_dot_json
    );
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn loads_api_key_from_auth_json() {
    let dir = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
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

        assert!(auth.get_token_data().is_err());
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
        tokens: None,
        last_refresh: None,
        personal_access_token: None,
        bedrock_api_key: None,
        bedrock_access_keys: None,
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

#[tokio::test]
#[serial(codex_auth_env)]
async fn unauthorized_recovery_reports_mode_and_step_names() {
    let dir = tempdir().unwrap();
    let manager = AuthManager::shared(
        dir.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        crate::test_support::transport_default_auth_route_config(),
    )
    .await;
    let managed = UnauthorizedRecovery {
        manager: Arc::clone(&manager),
        step: UnauthorizedRecoveryStep::Reload,
        expected_account_id: None,
        mode: UnauthorizedRecoveryMode::Managed,
    };
    assert_eq!(managed.mode_name(), "managed");
    assert_eq!(managed.step_name(), "reload");

    let external = UnauthorizedRecovery {
        manager,
        step: UnauthorizedRecoveryStep::ExternalRefresh,
        expected_account_id: None,
        mode: UnauthorizedRecoveryMode::External,
    };
    assert_eq!(external.mode_name(), "external");
    assert_eq!(external.step_name(), "external_refresh");
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn refresh_failure_is_scoped_to_the_matching_auth_snapshot() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: Some(WORKSPACE_ID_ALLOWED.to_string()),
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let auth = super::load_auth(
        codex_home.path(),
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
    .expect("load auth")
    .expect("auth available");
    let mut updated_auth_dot_json = auth
        .get_current_auth_json()
        .expect("AuthDotJson should exist");
    let updated_tokens = updated_auth_dot_json
        .tokens
        .as_mut()
        .expect("tokens should exist");
    updated_tokens.access_token = "new-access-token".to_string();
    updated_tokens.refresh_token = "new-refresh-token".to_string();
    let updated_auth = CodexAuth::from_auth_dot_json(
        codex_home.path(),
        updated_auth_dot_json,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::Direct,
        &crate::test_support::transport_default_auth_route_config(),
        /*cached_auth*/ None,
    )
    .await
    .expect("updated auth should parse");

    let manager = AuthManager::from_auth_for_testing(auth.clone());
    let error = RefreshTokenFailedError::new(
        RefreshTokenFailedReason::Exhausted,
        "refresh token already used",
    );
    manager.record_permanent_refresh_failure_if_unchanged(&auth, &error);

    assert_eq!(manager.refresh_failure_for_auth(&auth), Some(error));
    assert_eq!(manager.refresh_failure_for_auth(&updated_auth), None);
}

#[derive(Clone)]
struct StaticExternalAuth(CodexAuth);

impl ExternalAuth for StaticExternalAuth {
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async { Ok(self.0.clone()) })
    }

    fn refresh(&self, _context: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async { Ok(self.0.clone()) })
    }
}

struct RefreshingExternalAuth {
    initial: CodexAuth,
    refreshed: CodexAuth,
}

impl ExternalAuth for RefreshingExternalAuth {
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async { Ok(self.initial.clone()) })
    }

    fn refresh(&self, _context: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async { Ok(self.refreshed.clone()) })
    }
}

fn external_header_auth(account_id: Option<&'static str>) -> CodexAuth {
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::AUTHORIZATION,
        http::HeaderValue::from_static("Bearer external"),
    );
    if let Some(account_id) = account_id {
        headers.insert(
            "chatgpt-account-id",
            http::HeaderValue::from_static(account_id),
        );
    }
    CodexAuth::Headers(AuthHeaders::new(headers))
}

struct FailingExternalAuth {
    auth: CodexAuth,
    resolve_count: AtomicUsize,
}

impl ExternalAuth for FailingExternalAuth {
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth> {
        let resolve_count = self.resolve_count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if resolve_count == 0 {
                Ok(self.auth.clone())
            } else {
                Err(std::io::Error::other("external auth failed"))
            }
        })
    }

    fn refresh(&self, _context: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async { Err(std::io::Error::other("external auth failed")) })
    }

    fn classify_error(&self, error: std::io::Error) -> RefreshTokenError {
        RefreshTokenError::Permanent(RefreshTokenFailedError::new(
            RefreshTokenFailedReason::Other,
            error.to_string(),
        ))
    }
}

#[tokio::test]
async fn external_auth_keeps_cached_credentials_after_permanent_reload_failure() {
    let manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("seed"));
    let auth = CodexAuth::from_api_key("configured-token");
    let external_auth = Arc::new(FailingExternalAuth {
        auth: auth.clone(),
        resolve_count: AtomicUsize::new(0),
    });
    manager
        .set_external_auth(external_auth.clone())
        .await
        .expect("external auth should install");

    assert_eq!(external_auth.resolve_count.load(Ordering::SeqCst), 1);

    assert_eq!(manager.auth().await, Some(auth.clone()));
    assert_eq!(external_auth.resolve_count.load(Ordering::SeqCst), 2);
    assert_eq!(
        manager
            .refresh_failure_for_auth(&auth)
            .expect("permanent failure should be recorded")
            .to_string(),
        "external auth failed"
    );

    assert_eq!(manager.auth().await, Some(auth));
    assert_eq!(external_auth.resolve_count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn replacing_external_auth_clears_permanent_failure() {
    let manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("seed"));
    let auth = CodexAuth::from_api_key("external-token");
    manager
        .set_external_auth(Arc::new(FailingExternalAuth {
            auth: auth.clone(),
            resolve_count: AtomicUsize::new(0),
        }))
        .await
        .expect("external auth should install");

    assert_eq!(manager.auth().await, Some(auth.clone()));
    assert!(manager.refresh_failure_for_auth(&auth).is_some());

    manager
        .set_external_auth(Arc::new(StaticExternalAuth(auth.clone())))
        .await
        .expect("replacement external auth should install");

    manager
        .refresh_token_from_authority()
        .await
        .expect("replacement external auth should refresh");
    assert_eq!(manager.auth_cached(), Some(auth));
}

#[tokio::test]
async fn runtime_external_auth_uses_provider_error_classification() {
    let manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("seed"));
    manager
        .set_external_auth(Arc::new(FailingExternalAuth {
            auth: CodexAuth::from_api_key("runtime-token"),
            resolve_count: AtomicUsize::new(0),
        }))
        .await
        .expect("runtime auth should install");

    assert!(matches!(
        manager.refresh_token_from_authority().await,
        Err(RefreshTokenError::Permanent(_))
    ));
}

#[tokio::test]
async fn external_auth_provider_can_install_headers() {
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::AUTHORIZATION,
        http::HeaderValue::from_static("Bearer external"),
    );
    headers.insert("x-external-auth", http::HeaderValue::from_static("enabled"));
    let auth = CodexAuth::Headers(AuthHeaders::new(headers));
    let codex_home = tempdir().expect("tempdir");
    let manager = AuthManager::new(
        codex_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::Ephemeral,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        crate::test_support::transport_default_auth_route_config(),
    )
    .await;

    manager
        .set_external_auth(Arc::new(StaticExternalAuth(auth.clone())))
        .await
        .expect("external auth should install");

    assert_eq!(manager.auth_cached(), Some(auth));
    assert!(
        manager
            .auth_cached()
            .is_some_and(|auth| auth.uses_codex_backend())
    );
    assert!(
        !manager
            .auth_cached()
            .is_some_and(|auth| auth.is_chatgpt_auth())
    );
}

#[tokio::test]
async fn external_header_auth_obeys_workspace_policy() {
    for (account_id, should_succeed) in [
        (Some(WORKSPACE_ID_ALLOWED), true),
        (Some(WORKSPACE_ID_DISALLOWED), false),
        (None, false),
    ] {
        let auth = external_header_auth(account_id);
        let expected_auth = should_succeed.then_some(auth.clone());
        let manager = AuthManager::from_optional_auth_for_testing(/*auth*/ None);
        manager.set_forced_chatgpt_workspace_id(Some(vec![WORKSPACE_ID_ALLOWED.to_string()]));

        let result = manager
            .set_external_auth(Arc::new(StaticExternalAuth(auth)))
            .await;

        assert_eq!(result.is_ok(), should_succeed, "account ID: {account_id:?}");
        assert_eq!(manager.auth_cached(), expected_auth);
    }
}

#[tokio::test]
async fn external_header_auth_rejects_a_disallowed_workspace_on_refresh() {
    let allowed_auth = external_header_auth(Some(WORKSPACE_ID_ALLOWED));
    let disallowed_auth = external_header_auth(Some(WORKSPACE_ID_DISALLOWED));
    let manager = AuthManager::from_optional_auth_for_testing(/*auth*/ None);
    manager.set_forced_chatgpt_workspace_id(Some(vec![WORKSPACE_ID_ALLOWED.to_string()]));
    manager
        .set_external_auth(Arc::new(RefreshingExternalAuth {
            initial: allowed_auth.clone(),
            refreshed: disallowed_auth,
        }))
        .await
        .expect("initial external header auth should install");

    manager
        .refresh_token_from_authority()
        .await
        .expect_err("external header auth from a disallowed workspace must not replace the cache");

    assert_eq!(manager.auth_cached(), Some(allowed_auth));
}

struct AuthFileParams {
    openai_api_key: Option<String>,
    chatgpt_plan_type: Option<String>,
    chatgpt_account_id: Option<String>,
}

fn write_auth_file(params: AuthFileParams, codex_home: &Path) -> std::io::Result<String> {
    let fake_jwt = fake_jwt_for_auth_file_params(&params)?;
    let auth_file = get_auth_file(codex_home);
    let auth_json_data = json!({
        "OPENAI_API_KEY": params.openai_api_key,
        "tokens": {
            "id_token": fake_jwt,
            "access_token": "test-access-token",
            "refresh_token": "test-refresh-token"
        },
        "last_refresh": Utc::now(),
    });
    let auth_json = serde_json::to_string_pretty(&auth_json_data)?;
    std::fs::write(auth_file, auth_json)?;
    Ok(fake_jwt)
}

fn fake_jwt_for_auth_file_params(params: &AuthFileParams) -> std::io::Result<String> {
    #[derive(Serialize)]
    struct Header {
        alg: &'static str,
        typ: &'static str,
    }

    let header = Header {
        alg: "none",
        typ: "JWT",
    };
    let mut auth_payload = serde_json::json!({
        "chatgpt_user_id": "user-12345",
        "user_id": "user-12345",
    });

    if let Some(chatgpt_plan_type) = params.chatgpt_plan_type.as_ref() {
        auth_payload["chatgpt_plan_type"] = serde_json::Value::String(chatgpt_plan_type.clone());
    }

    if let Some(chatgpt_account_id) = params.chatgpt_account_id.as_ref() {
        auth_payload["chatgpt_account_id"] = serde_json::Value::String(chatgpt_account_id.clone());
    }

    let payload = serde_json::json!({
        "email": "user@example.com",
        "email_verified": true,
        "https://api.openai.com/auth": auth_payload,
    });
    let b64 = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
    let header_b64 = b64(&serde_json::to_vec(&header)?);
    let payload_b64 = b64(&serde_json::to_vec(&payload)?);
    let signature_b64 = b64(b"sig");
    Ok(format!("{header_b64}.{payload_b64}.{signature_b64}"))
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

fn remove_access_token_env_var() -> EnvVarGuard {
    EnvVarGuard::remove(CODEX_ACCESS_TOKEN_ENV_VAR)
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
    let _access_token_guard = EnvVarGuard::set(CODEX_ACCESS_TOKEN_ENV_VAR, "at-env-token");
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
    let _access_token_guard = remove_access_token_env_var();
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
    let _access_token_guard = remove_access_token_env_var();
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
async fn workspace_policy_checks_the_selected_request_account() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: Some(WORKSPACE_ID_ALLOWED.to_string()),
        },
        codex_home.path(),
    )
    .expect("seed ChatGPT credentials");
    let auth_path = codex_home.path().join("auth.json");
    let mut stored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&auth_path).unwrap()).unwrap();
    stored["tokens"]["account_id"] = json!(WORKSPACE_ID_DISALLOWED);
    std::fs::write(&auth_path, serde_json::to_vec(&stored).unwrap()).unwrap();
    let config = build_config(
        codex_home.path(),
        /*forced_login_method*/ None,
        Some(vec![WORKSPACE_ID_ALLOWED.to_string()]),
    )
    .await;
    let manager =
        AuthManager::shared_from_auth_config(config, /*enable_codex_api_key_env*/ false)
            .await
            .expect("auth manager");

    assert_eq!(manager.auth().await, None);
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn enforce_login_restrictions_logs_out_for_workspace_mismatch() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: Some(WORKSPACE_ID_DISALLOWED.to_string()),
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let config = build_config(
        codex_home.path(),
        /*forced_login_method*/ None,
        Some(vec![WORKSPACE_ID_ALLOWED.to_string()]),
    )
    .await;

    let err = super::enforce_login_restrictions(&config)
        .await
        .expect_err("expected workspace mismatch to error");
    assert!(
        err.to_string()
            .contains(&format!("workspace(s) {WORKSPACE_ID_ALLOWED}"))
    );
    assert!(
        !codex_home.path().join("auth.json").exists(),
        "auth.json should be removed on mismatch"
    );
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn enforce_login_restrictions_allows_matching_workspace() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: Some(WORKSPACE_ID_ALLOWED.to_string()),
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

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
async fn enforce_login_restrictions_allows_any_matching_workspace_in_list() {
    let codex_home = tempdir().unwrap();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: Some(WORKSPACE_ID_ALLOWED.to_string()),
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let config = build_config(
        codex_home.path(),
        /*forced_login_method*/ None,
        Some(vec![
            WORKSPACE_ID_SECOND_ALLOWED.to_string(),
            WORKSPACE_ID_ALLOWED.to_string(),
        ]),
    )
    .await;

    super::enforce_login_restrictions(&config)
        .await
        .expect("any matching workspace in the allowed list should succeed");
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn enforce_login_restrictions_allows_api_key_if_login_method_not_set_but_forced_chatgpt_workspace_id_is_set()
 {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
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
    let _access_token_guard = remove_access_token_env_var();
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

#[tokio::test]
#[serial(codex_auth_env)]
async fn plan_type_maps_known_plan() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: None,
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let auth = super::load_auth(
        codex_home.path(),
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
    .expect("load auth")
    .expect("auth available");

    pretty_assertions::assert_eq!(auth.account_plan_type(), Some(AccountPlanType::Pro));
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn plan_type_maps_self_serve_business_usage_based_plan() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("self_serve_business_usage_based".to_string()),
            chatgpt_account_id: None,
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let auth = super::load_auth(
        codex_home.path(),
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
    .expect("load auth")
    .expect("auth available");

    pretty_assertions::assert_eq!(
        auth.account_plan_type(),
        Some(AccountPlanType::SelfServeBusinessUsageBased)
    );
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn plan_type_maps_enterprise_cbp_usage_based_plan() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("enterprise_cbp_usage_based".to_string()),
            chatgpt_account_id: None,
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let auth = super::load_auth(
        codex_home.path(),
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
    .expect("load auth")
    .expect("auth available");

    pretty_assertions::assert_eq!(
        auth.account_plan_type(),
        Some(AccountPlanType::EnterpriseCbpUsageBased)
    );
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn plan_type_maps_unknown_to_unknown() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("mystery-tier".to_string()),
            chatgpt_account_id: None,
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let auth = super::load_auth(
        codex_home.path(),
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
    .expect("load auth")
    .expect("auth available");

    pretty_assertions::assert_eq!(auth.account_plan_type(), Some(AccountPlanType::Unknown));
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn missing_plan_type_maps_to_unknown() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: None,
            chatgpt_account_id: None,
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let auth = super::load_auth(
        codex_home.path(),
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
    .expect("load auth")
    .expect("auth available");

    pretty_assertions::assert_eq!(auth.account_plan_type(), Some(AccountPlanType::Unknown));
}
