//! Provider and authentication dimensions that partition model catalogs.

use super::*;
use pretty_assertions::assert_eq;

#[test]
fn cache_identity_tracks_provider_routing_and_effective_api_credentials() {
    let auth = CodexAuth::from_api_key("sk-proj-fedramp-synthetic-test-key");
    let provider = ModelProviderInfo::create_openai_provider(Some("https://one.example/v1".into()));
    let key = identity(&provider, Some(&auth)).unwrap();
    let other_key = CodexAuth::from_api_key("sk-proj-fedramp-rotated-test-key");
    assert_ne!(identity(&provider, Some(&other_key)).unwrap(), key);
    let mut other_provider = provider.clone();
    other_provider.base_url = Some("https://two.example/v1".into());
    assert_ne!(identity(&other_provider, Some(&auth)).unwrap(), key);
    other_provider = provider.clone();
    other_provider.experimental_bearer_token = Some("provider-key".into());
    assert_ne!(identity(&other_provider, Some(&auth)).unwrap(), key);
    other_provider = provider;
    other_provider.http_headers = Some(std::collections::HashMap::from([(
        "openai-project".into(),
        "another-project".into(),
    )]));
    assert_ne!(identity(&other_provider, Some(&auth)).unwrap(), key);

    let mut provider = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
    // Keep identities stable while the endpoint residency test runs in parallel.
    provider.http_headers = Some(std::collections::HashMap::from([(
        codex_login::default_client::RESIDENCY_HEADER_NAME.to_string(),
        "us".into(),
    )]));
    let key = identity(&provider, Some(&auth)).unwrap();
    provider.base_url = Some("https://gov.api.openai.com/v1".into());
    assert_eq!(identity(&provider, Some(&auth)).unwrap(), key);
    provider.base_url = Some("https://api.openai.com/v1".into());
    assert_ne!(identity(&provider, Some(&auth)).unwrap(), key);
}

#[test]
fn cache_identity_distinguishes_auth_requirements() {
    let auth = CodexAuth::from_api_key("test-key");
    let mut provider =
        ModelProviderInfo::create_openai_provider(Some("https://example.com/v1".into()));
    let key = identity(&provider, Some(&auth)).unwrap();
    assert_ne!(identity(&provider, /*auth*/ None).unwrap(), key);
    provider.requires_openai_auth = false;
    assert_ne!(key, identity(&provider, Some(&auth)).unwrap());
}

#[test]
fn cache_identity_tracks_catalog_url() {
    let auth = CodexAuth::from_api_key("test-key");
    let mut provider =
        ModelProviderInfo::create_openai_provider(Some("https://gateway.example/v1".into()));
    let bundled = identity(&provider, Some(&auth)).unwrap();
    provider.model_catalog_url = Some("https://gateway.example/catalog-one".into());
    let first = identity(&provider, Some(&auth)).unwrap();
    provider.model_catalog_url = Some("https://gateway.example/catalog-two".into());
    let second = identity(&provider, Some(&auth)).unwrap();
    assert_ne!(bundled, first);
    assert_ne!(first, second);
}
