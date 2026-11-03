use std::sync::Arc;

use codex_api::AuthError;
use codex_api::AuthHeadersFuture;
use codex_api::AuthProvider;
use codex_api::SharedAuthProvider;
use codex_login::AuthHeaders;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::error::CodexErr;
use http::HeaderMap;

use crate::bearer_auth_provider::BearerAuthProvider;

const BEDROCK_API_KEY_UNSUPPORTED_MESSAGE: &str =
    "Bedrock API key auth is only supported by the Amazon Bedrock model provider";

/// Credential ownership for one harness/thread context.
/// Publish these together so auxiliary clients cannot select another thread's credentials.
#[derive(Clone, Debug)]
pub struct ProviderAuthContext {
    pub credentials: Arc<AuthManager>,
}

impl ProviderAuthContext {
    pub fn model_provider(&self, info: ModelProviderInfo) -> crate::SharedModelProvider {
        crate::provider::create_model_provider(info, Some(Arc::clone(&self.credentials)))
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProviderAuthScope {}

/// Provider auth resolved for a request.
#[derive(Clone)]
pub struct ResolvedProviderAuth {
    pub auth: SharedAuthProvider,
}

impl ResolvedProviderAuth {
    pub(crate) fn new(auth: SharedAuthProvider) -> Self {
        Self { auth }
    }
}

#[derive(Clone, Debug)]
struct HeaderAuthProvider {
    auth: AuthHeaders,
}

impl AuthProvider for HeaderAuthProvider {
    fn add_auth_headers(&self, headers: &mut HeaderMap) -> Result<(), AuthError> {
        headers.extend(self.auth.headers().clone());
        Ok(())
    }
}

struct AuthManagerAuthProvider {
    auth_manager: Arc<AuthManager>,
    // Startup auth is only the account-scoped identity anchor. Request
    // headers always come from the current AuthManager snapshot below.
    expected_auth: CodexAuth,
}

impl AuthManagerAuthProvider {
    fn is_expected_auth(&self, auth: &CodexAuth) -> bool {
        auth.uses_codex_backend()
            && auth.get_account_id() == self.expected_auth.get_account_id()
            && auth.get_chatgpt_user_id() == self.expected_auth.get_chatgpt_user_id()
            && auth.is_workspace_account() == self.expected_auth.is_workspace_account()
    }

    fn current_auth(&self) -> Option<CodexAuth> {
        self.auth_manager
            .auth_cached()
            .filter(|auth| self.is_expected_auth(auth))
    }
}

impl AuthProvider for AuthManagerAuthProvider {
    fn add_auth_headers(&self, headers: &mut HeaderMap) -> Result<(), AuthError> {
        let auth = self.current_auth().ok_or_else(|| {
            AuthError::Transient("managed authentication is unavailable".to_string())
        })?;
        auth_provider_from_auth(&auth).add_auth_headers(headers)
    }

    fn apply_auth_headers(&self, mut headers: HeaderMap) -> AuthHeadersFuture<'_> {
        Box::pin(async move {
            let auth = self
                .auth_manager
                .auth()
                .await
                .filter(|auth| self.is_expected_auth(auth))
                .ok_or_else(|| {
                    AuthError::Transient("managed authentication is unavailable".to_string())
                })?;
            headers.extend(auth_provider_from_auth(&auth).to_auth_headers()?);
            Ok(headers)
        })
    }

    fn refresh_after_unauthorized(
        &self,
        rejected_headers: HeaderMap,
    ) -> codex_api::AuthRefreshFuture<'_> {
        Box::pin(async move {
            let Some(auth) = self.current_auth() else {
                return false;
            };
            let Ok(headers) = auth_provider_from_auth(&auth).to_auth_headers() else {
                return false;
            };
            if headers.is_empty() {
                return false;
            }
            if headers
                .iter()
                .any(|(name, value)| rejected_headers.get(name) != Some(value))
                && self.auth_manager.refresh_failure_for_auth(&auth).is_none()
            {
                return true;
            }
            if !self.auth_manager.unauthorized_recovery().has_next() {
                return false;
            }
            let result = self.auth_manager.refresh_token_if_unchanged(&auth).await;
            let Some(refreshed_auth) = self.current_auth() else {
                return false;
            };
            if self
                .auth_manager
                .refresh_failure_for_auth(&refreshed_auth)
                .is_some()
            {
                return false;
            }
            let Ok(refreshed_headers) = auth_provider_from_auth(&refreshed_auth).to_auth_headers()
            else {
                return false;
            };
            !refreshed_headers.is_empty()
                && refreshed_headers != headers
                && !matches!(result, Err(codex_login::RefreshTokenError::Policy(_)))
        })
    }
}

// Some providers are meant to send no auth headers. Examples include local OSS
// providers and custom test providers with `requires_openai_auth = false`.
#[derive(Clone, Debug)]
struct UnauthenticatedAuthProvider;

impl AuthProvider for UnauthenticatedAuthProvider {
    fn add_auth_headers(&self, _headers: &mut HeaderMap) -> Result<(), AuthError> {
        Ok(())
    }
}

pub fn unauthenticated_auth_provider() -> SharedAuthProvider {
    Arc::new(UnauthenticatedAuthProvider)
}

/// Returns the provider-scoped auth manager when one is supplied.
///
/// Command-backed external bearer auth is no longer supported.
pub(crate) fn auth_manager_for_provider(
    auth_manager: Option<Arc<AuthManager>>,
    _provider: &ModelProviderInfo,
) -> Option<Arc<AuthManager>> {
    auth_manager
}

pub(crate) fn resolve_provider_auth(
    auth: Option<&CodexAuth>,
    provider: &ModelProviderInfo,
) -> codex_protocol::error::Result<SharedAuthProvider> {
    if provider.is_amazon_bedrock() {
        return Err(CodexErr::UnsupportedOperation(
            "Amazon Bedrock model providers are not supported".to_string(),
        ));
    }

    if let Some(auth) = bearer_auth_for_provider(provider)? {
        return Ok(Arc::new(auth));
    }

    if !provider.requires_openai_auth && provider.auth.is_none() {
        return Ok(unauthenticated_auth_provider());
    }

    if matches!(
        auth,
        Some(CodexAuth::BedrockApiKey(_) | CodexAuth::BedrockAccessKeys(_))
    ) {
        return Err(CodexErr::UnsupportedOperation(
            BEDROCK_API_KEY_UNSUPPORTED_MESSAGE.to_string(),
        ));
    }

    Ok(match auth {
        Some(auth) => auth_provider_from_auth(auth),
        None => unauthenticated_auth_provider(),
    })
}

pub(crate) async fn resolve_provider_auth_for_scope(
    _auth_manager: Option<Arc<AuthManager>>,
    auth: Option<&CodexAuth>,
    provider: &ModelProviderInfo,
    _scope: ProviderAuthScope,
) -> codex_protocol::error::Result<ResolvedProviderAuth> {
    resolve_provider_auth(auth, provider).map(ResolvedProviderAuth::new)
}

fn bearer_auth_for_provider(
    provider: &ModelProviderInfo,
) -> codex_protocol::error::Result<Option<BearerAuthProvider>> {
    if let Some(api_key) = provider.api_key()? {
        return Ok(Some(BearerAuthProvider::new(api_key)));
    }

    if let Some(token) = provider.experimental_bearer_token.clone() {
        return Ok(Some(BearerAuthProvider::new(token.into_inner())));
    }

    Ok(None)
}

/// Whether the selected API key enables OpenAI's default FedRAMP endpoints.
/// Command-backed providers retain their configured routing across token refreshes.
pub(crate) fn uses_fedramp_api_key(
    auth: Option<&CodexAuth>,
    provider: &ModelProviderInfo,
) -> codex_protocol::error::Result<bool> {
    if !provider.is_openai() || provider.has_command_auth() {
        return Ok(false);
    }
    if let Some(auth) = bearer_auth_for_provider(provider)? {
        return Ok(auth.is_fedramp_account);
    }
    Ok(provider.requires_openai_auth
        && matches!(auth, Some(CodexAuth::ApiKey(_)))
        && auth.is_some_and(CodexAuth::is_fedramp_account))
}

/// Builds request-header auth for a first-party Codex auth snapshot.
pub fn auth_provider_from_auth(auth: &CodexAuth) -> SharedAuthProvider {
    match auth {
        CodexAuth::Headers(auth) => Arc::new(HeaderAuthProvider { auth: auth.clone() }),
        CodexAuth::BedrockApiKey(_) | CodexAuth::BedrockAccessKeys(_) => {
            unreachable!("{BEDROCK_API_KEY_UNSUPPORTED_MESSAGE}")
        }
        CodexAuth::ApiKey(_)
        | CodexAuth::Chatgpt(_)
        | CodexAuth::ChatgptAuthTokens(_)
        | CodexAuth::PersonalAccessToken(_) => Arc::new(BearerAuthProvider {
            token: auth.get_token().ok(),
            account_id: auth.get_account_id(),
            is_fedramp_account: auth.is_fedramp_account(),
        }),
    }
}

/// Builds request-header auth that reads the current managed auth snapshot on
/// every request while remaining scoped to the expected auth identity.
///
/// Callers with account-scoped state should pass the same snapshot that keyed
/// that state so a later account switch cannot reuse it.
pub fn auth_provider_from_auth_manager(
    auth_manager: Arc<AuthManager>,
    expected_auth: &CodexAuth,
) -> SharedAuthProvider {
    Arc::new(AuthManagerAuthProvider {
        auth_manager,
        expected_auth: expected_auth.clone(),
    })
}

#[cfg(test)]
#[path = "auth_refresh_tests.rs"]
mod auth_refresh_tests;

#[cfg(test)]
mod tests {
    use codex_login::AuthCredentialsStoreMode;
    use codex_login::AuthKeyringBackendKind;
    use codex_login::auth::BedrockApiKeyAuth;
    use codex_login::auth::login_with_chatgpt_auth_tokens;
    use codex_model_provider_info::WireApi;
    use codex_model_provider_info::create_oss_provider_with_base_url;
    use codex_protocol::config_types::ModelProviderAuthInfo;
    use codex_protocol::error::CodexErrorDetails;
    use http::HeaderValue;
    use http::header::AUTHORIZATION;
    use pretty_assertions::assert_eq;
    use std::num::NonZeroU64;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use super::*;

    static NEXT_CODEX_HOME_ID: AtomicUsize = AtomicUsize::new(0);

    fn test_codex_home() -> PathBuf {
        std::env::temp_dir().join(format!(
            "codex-model-provider-auth-test-{pid}-{id}",
            pid = std::process::id(),
            id = NEXT_CODEX_HOME_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn unauthenticated_auth_provider_adds_no_headers() {
        let provider =
            create_oss_provider_with_base_url("http://localhost:11434/v1", WireApi::Responses);
        let auth = resolve_provider_auth(/*auth*/ None, &provider).expect("auth should resolve");

        assert!(auth.to_auth_headers().unwrap().is_empty());
    }

    #[test]
    fn custom_provider_does_not_inherit_ambient_auth_headers() {
        let provider =
            create_oss_provider_with_base_url("http://localhost:11434/v1", WireApi::Responses);
        let mut ambient_headers = HeaderMap::new();
        ambient_headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer ambient-token"),
        );
        ambient_headers.insert(
            "ChatGPT-Account-ID",
            HeaderValue::from_static("account-123"),
        );
        for ambient_auth in [
            CodexAuth::Headers(AuthHeaders::new(ambient_headers)),
            CodexAuth::from_api_key("sk-proj-fedramp-synthetic-test-key"),
        ] {
            let auth =
                resolve_provider_auth(Some(&ambient_auth), &provider).expect("auth should resolve");
            assert!(auth.to_auth_headers().unwrap().is_empty());
        }
    }

    #[test]
    fn custom_provider_does_not_inherit_ambient_bedrock_auth() {
        let provider =
            create_oss_provider_with_base_url("http://localhost:11434/v1", WireApi::Responses);
        let ambient_auth = CodexAuth::BedrockApiKey(BedrockApiKeyAuth {
            api_key: "bedrock-api-key-test".to_string(),
            region: "us-east-1".to_string(),
        });

        let auth =
            resolve_provider_auth(Some(&ambient_auth), &provider).expect("auth should resolve");

        assert!(auth.to_auth_headers().unwrap().is_empty());
    }

    #[test]
    fn custom_provider_uses_explicit_bearer_instead_of_ambient_auth() {
        let mut provider =
            create_oss_provider_with_base_url("http://localhost:11434/v1", WireApi::Responses);
        provider.experimental_bearer_token = Some("provider-token".into());
        let ambient_auth = CodexAuth::BedrockApiKey(BedrockApiKeyAuth {
            api_key: "bedrock-api-key-test".to_string(),
            region: "us-east-1".to_string(),
        });

        let headers = resolve_provider_auth(Some(&ambient_auth), &provider)
            .expect("auth should resolve")
            .to_auth_headers()
            .unwrap();

        assert_eq!(
            headers.get(AUTHORIZATION),
            Some(&HeaderValue::from_static("Bearer provider-token"))
        );
        assert_eq!(headers.len(), 1);
    }

    #[test]
    fn api_key_fedramp_routing_follows_the_selected_provider_credential() {
        let project_key = "sk-proj-fedramp-synthetic-test-key";
        let service_account_key = "sk-svcacct-fedramp-synthetic-test-key";
        let legacy_key = "sk-proj-synthetic-test-key";

        for (ambient_key, provider_key, expected_fedramp) in [
            (project_key, None, true),
            (service_account_key, None, true),
            (legacy_key, None, false),
            (project_key, Some(legacy_key), false),
            (legacy_key, Some(project_key), true),
            (legacy_key, Some(service_account_key), true),
        ] {
            let mut provider = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
            provider.experimental_bearer_token = provider_key.map(Into::into);
            let ambient_auth = CodexAuth::from_api_key(ambient_key);

            let headers = resolve_provider_auth(Some(&ambient_auth), &provider)
                .expect("auth should resolve")
                .to_auth_headers()
                .unwrap();

            let key = provider_key.unwrap_or(ambient_key);
            let mut expected_headers = HeaderMap::from_iter([(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {key}")).unwrap(),
            )]);
            if expected_fedramp {
                expected_headers.insert("X-OpenAI-Fedramp", HeaderValue::from_static("true"));
            }
            assert_eq!(headers, expected_headers);
        }
    }

    #[test]
    fn custom_provider_uses_command_resolved_auth() {
        let mut provider =
            create_oss_provider_with_base_url("http://localhost:11434/v1", WireApi::Responses);
        provider.auth = Some(ModelProviderAuthInfo {
            command: "print-token".to_string(),
            args: Vec::new(),
            timeout_ms: NonZeroU64::new(5_000).expect("timeout should be non-zero"),
            refresh_interval_ms: 300_000,
            cwd: std::env::current_dir()
                .expect("current directory should be available")
                .try_into()
                .expect("current directory should be absolute"),
        });
        let command_auth = CodexAuth::from_api_key("command-token");

        let headers = resolve_provider_auth(Some(&command_auth), &provider)
            .expect("auth should resolve")
            .to_auth_headers()
            .unwrap();

        assert_eq!(
            headers.get(AUTHORIZATION),
            Some(&HeaderValue::from_static("Bearer command-token"))
        );
    }

    #[test]
    fn openai_provider_preserves_ambient_auth_headers() {
        let provider = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
        let mut expected = HeaderMap::new();
        expected.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer ambient-token"),
        );
        expected.insert(
            "ChatGPT-Account-ID",
            HeaderValue::from_static("account-123"),
        );
        let ambient_auth = CodexAuth::Headers(AuthHeaders::new(expected.clone()));

        let auth =
            resolve_provider_auth(Some(&ambient_auth), &provider).expect("auth should resolve");

        assert_eq!(auth.to_auth_headers().unwrap(), expected);
    }

    #[test]
    fn header_auth_adds_predefined_headers() {
        let mut expected = HeaderMap::new();
        expected.insert(
            http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer external"),
        );
        expected.insert("x-external-auth", HeaderValue::from_static("enabled"));
        let auth = CodexAuth::Headers(AuthHeaders::new(expected.clone()));

        let actual = auth_provider_from_auth(&auth).to_auth_headers().unwrap();

        assert_eq!(actual, expected);
    }

    #[test]
    fn openai_provider_rejects_bedrock_api_key_auth() {
        let provider = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
        let auth = CodexAuth::BedrockApiKey(BedrockApiKeyAuth {
            api_key: "bedrock-api-key-test".to_string(),
            region: "us-east-1".to_string(),
        });

        match resolve_provider_auth(Some(&auth), &provider) {
            Err(err) => match err.details() {
                CodexErrorDetails::UnsupportedOperation(message) => {
                    assert_eq!(message, BEDROCK_API_KEY_UNSUPPORTED_MESSAGE);
                }
                details => panic!("unexpected auth error: {details:?}"),
            },
            Ok(_) => panic!("Bedrock API key auth should be rejected"),
        }
    }

    #[tokio::test]
    async fn auth_manager_provider_follows_refreshes_but_not_account_switches() {
        let codex_home = test_codex_home();
        login_with_chatgpt_auth_tokens(
            &codex_home,
            "header.e30.first",
            "test-account",
            /*chatgpt_plan_type*/ None,
        )
        .expect("save initial auth");
        let auth_manager = Arc::new(
            AuthManager::new(
                codex_home.clone(),
                /*enable_codex_api_key_env*/ false,
                AuthCredentialsStoreMode::Ephemeral,
                /*forced_chatgpt_workspace_id*/ None,
                /*chatgpt_base_url*/ None,
                AuthKeyringBackendKind::default(),
                codex_login::test_support::transport_default_auth_route_config(),
            )
            .await,
        );
        let expected_auth = auth_manager
            .auth_cached()
            .expect("initial auth should be cached");
        let provider = auth_provider_from_auth_manager(Arc::clone(&auth_manager), &expected_auth);
        let initial_headers = provider
            .apply_auth_headers(HeaderMap::new())
            .await
            .expect("initial auth headers");

        assert_eq!(
            provider.to_auth_headers().unwrap().get(AUTHORIZATION),
            Some(&HeaderValue::from_static("Bearer header.e30.first"))
        );

        login_with_chatgpt_auth_tokens(
            &codex_home,
            "header.e30.reloaded",
            "test-account",
            /*chatgpt_plan_type*/ None,
        )
        .expect("save reloaded auth");

        // Ordinary requests retain the manager's cache until refresh or recovery runs.
        assert_eq!(
            provider
                .apply_auth_headers(HeaderMap::new())
                .await
                .expect("cached auth headers"),
            initial_headers
        );
        assert_eq!(
            tokio::join!(
                provider.refresh_after_unauthorized(initial_headers.clone()),
                provider.refresh_after_unauthorized(initial_headers),
            ),
            (true, true)
        );

        let resolved_headers = provider
            .apply_auth_headers(HeaderMap::new())
            .await
            .expect("managed auth headers should resolve");
        assert_eq!(
            resolved_headers.get(AUTHORIZATION),
            Some(&HeaderValue::from_static("Bearer header.e30.reloaded"))
        );

        login_with_chatgpt_auth_tokens(
            &codex_home,
            "header.e30.other-account",
            "other-account",
            /*chatgpt_plan_type*/ None,
        )
        .expect("save switched-account auth");
        assert!(!provider.refresh_after_unauthorized(resolved_headers).await);
        auth_manager.reload().await;

        assert!(matches!(
            provider.to_auth_headers(),
            Err(AuthError::Transient(_))
        ));
        assert!(matches!(
            provider.apply_auth_headers(HeaderMap::new()).await,
            Err(AuthError::Transient(_))
        ));
    }

    #[derive(Clone, Copy)]
    enum RefreshFailure {
        Transient,
        Permanent,
        Policy,
    }

    struct FailingRefreshAuth {
        auth: tokio::sync::watch::Receiver<CodexAuth>,
        barrier: tokio::sync::Barrier,
        failure: RefreshFailure,
    }

    impl codex_login::ExternalAuth for FailingRefreshAuth {
        fn resolve(&self) -> codex_login::ExternalAuthFuture<'_, CodexAuth> {
            Box::pin(async { Ok(self.auth.borrow().clone()) })
        }

        fn refresh(
            &self,
            _context: codex_login::ExternalAuthRefreshContext,
        ) -> codex_login::ExternalAuthFuture<'_, CodexAuth> {
            Box::pin(async {
                self.barrier.wait().await;
                self.barrier.wait().await;
                Err(std::io::Error::other("refresh service unavailable"))
            })
        }

        fn classify_error(&self, error: std::io::Error) -> codex_login::RefreshTokenError {
            match self.failure {
                RefreshFailure::Transient => codex_login::RefreshTokenError::Transient(error),
                RefreshFailure::Permanent => codex_login::auth::RefreshTokenFailedError::new(
                    codex_login::auth::RefreshTokenFailedReason::Other,
                    error.to_string(),
                )
                .into(),
                RefreshFailure::Policy => codex_login::RefreshTokenError::Policy(
                    codex_http_client::NetworkPolicyDenied::Revoked,
                ),
            }
        }
    }

    #[tokio::test]
    async fn failed_refresh_retries_only_with_newer_credentials() {
        for failure in [
            RefreshFailure::Transient,
            RefreshFailure::Permanent,
            RefreshFailure::Policy,
        ] {
            for (token, account, changed) in [
                ("Bearer old", "account", false),
                ("Bearer new", "account", true),
                ("Bearer new", "other-account", false),
            ] {
                let initial = CodexAuth::Headers(AuthHeaders::new(HeaderMap::from_iter([
                    (AUTHORIZATION, HeaderValue::from_static("Bearer old")),
                    (
                        http::HeaderName::from_static("chatgpt-account-id"),
                        HeaderValue::from_static("account"),
                    ),
                ])));
                let (updates, auth) = tokio::sync::watch::channel(initial.clone());
                let external = Arc::new(FailingRefreshAuth {
                    auth,
                    barrier: tokio::sync::Barrier::new(2),
                    failure,
                });
                let manager = AuthManager::from_auth_for_testing(initial.clone());
                manager.set_external_auth(external.clone()).await.unwrap();
                let provider = auth_provider_from_auth_manager(manager.clone(), &initial);
                let rejected = provider.to_auth_headers().expect("initial auth headers");
                let recovery = provider.refresh_after_unauthorized(rejected);
                let concurrent_update = async {
                    external.barrier.wait().await;
                    updates.send_replace(CodexAuth::Headers(AuthHeaders::new(
                        HeaderMap::from_iter([
                            (AUTHORIZATION, HeaderValue::from_static(token)),
                            (
                                http::HeaderName::from_static("chatgpt-account-id"),
                                HeaderValue::from_static(account),
                            ),
                        ]),
                    )));
                    manager.reload().await;
                    external.barrier.wait().await;
                };
                let (retry, ()) = tokio::join!(recovery, concurrent_update);
                assert_eq!(retry, changed && !matches!(failure, RefreshFailure::Policy));
                if matches!(failure, RefreshFailure::Permanent) && token == "Bearer old" {
                    // Even a late rejection must not reuse a credential known to have failed.
                    assert!(!provider.refresh_after_unauthorized(HeaderMap::new()).await);
                }
            }
        }
    }
}
