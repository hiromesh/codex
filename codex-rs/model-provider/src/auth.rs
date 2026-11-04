use std::sync::Arc;

use codex_api::AuthError;
use codex_api::AuthProvider;
use codex_api::SharedAuthProvider;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::error::CodexErr;
use http::HeaderMap;

use crate::bearer_auth_provider::BearerAuthProvider;

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

    if provider.auth.is_some() {
        return Err(CodexErr::UnsupportedOperation(
            "command-backed provider auth is no longer supported".to_string(),
        ));
    }

    if !provider.requires_openai_auth {
        return Ok(unauthenticated_auth_provider());
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
    Ok(provider.requires_openai_auth && auth.is_some_and(CodexAuth::is_fedramp_account))
}

/// Builds request-header auth for a first-party Codex auth snapshot.
pub fn auth_provider_from_auth(auth: &CodexAuth) -> SharedAuthProvider {
    Arc::new(BearerAuthProvider {
        token: auth.get_token().ok(),
        account_id: None,
        is_fedramp_account: auth.is_fedramp_account(),
    })
}

#[cfg(test)]
mod tests {
    use codex_model_provider_info::WireApi;
    use codex_model_provider_info::create_oss_provider_with_base_url;
    use http::HeaderValue;
    use http::header::AUTHORIZATION;
    use pretty_assertions::assert_eq;
    use super::*;

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
        for ambient_auth in [CodexAuth::from_api_key("sk-proj-fedramp-synthetic-test-key")] {
            let auth =
                resolve_provider_auth(Some(&ambient_auth), &provider).expect("auth should resolve");
            assert!(auth.to_auth_headers().unwrap().is_empty());
        }
    }

    #[test]
    fn custom_provider_uses_explicit_bearer_instead_of_ambient_auth() {
        let mut provider =
            create_oss_provider_with_base_url("http://localhost:11434/v1", WireApi::Responses);
        provider.experimental_bearer_token = Some("provider-token".into());
        let ambient_auth = CodexAuth::from_api_key("sk-proj-fedramp-synthetic-test-key");

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

    
}
