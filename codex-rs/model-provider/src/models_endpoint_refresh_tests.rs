//! A catalog deadline must not interrupt rotating OAuth credentials before persistence.
#![cfg(test)]

use super::*;
use base64::Engine;
use codex_http_client::OutboundProxyPolicy;
use codex_login::AuthCredentialsStoreMode;
use codex_login::AuthKeyringBackendKind;
use codex_login::AuthRouteConfig;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_partial_json;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[tokio::test]
async fn catalog_timeout_preserves_refreshed_tokens() {
    let server = MockServer::start().await;
    let home = tempfile::tempdir().unwrap();
    let token = |expires| {
        let payload = json!({
            "iss": server.uri(), "session_id": "session-a", "exp": expires,
            "https://api.openai.com/auth": {
                "chatgpt_user_id": "user-a", "chatgpt_account_id": "workspace-a",
                "chatgpt_plan_type": "plus",
            },
        });
        format!(
            "e30.{}.signature",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string())
        )
    };
    let old_token = token(0);
    let new_token = token(chrono::Utc::now().timestamp() + 3600);
    let stored = serde_json::from_value(json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": old_token, "access_token": old_token,
            "refresh_token": "refresh-before", "account_id": "workspace-a",
            "oauth_provenance": {"issuer": server.uri(), "client_id": "test-client"},
        },
        "last_refresh": "2020-01-01T00:00:00Z",
    }))
    .unwrap();
    codex_login::save_auth(
        home.path(),
        &stored,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::Direct,
    )
    .unwrap();
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_partial_json(
            json!({"refresh_token": "refresh-before"}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(MODELS_REFRESH_TIMEOUT + Duration::from_secs(1))
                .set_body_json(json!({
                    "id_token": new_token, "access_token": new_token,
                    "refresh_token": "refresh-after",
                })),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    // Reusing the consumed refresh token must fail, as it would at the issuer.
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {"code": "refresh_token_reused"},
        })))
        .with_priority(2)
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models": []})))
        .expect(1)
        .mount(&server)
        .await;

    let factory = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault);
    let credentials = Arc::new(
        AuthManager::new(
            home.path().to_path_buf(),
            /*enable_codex_api_key_env*/ false,
            AuthCredentialsStoreMode::File,
            /*forced_chatgpt_workspace_id*/ None,
            /*chatgpt_base_url*/ None,
            AuthKeyringBackendKind::Direct,
            AuthRouteConfig::from_http_client_factory(factory.clone()),
        )
        .await,
    );
    let mut info = ModelProviderInfo::create_openai_provider(Some(server.uri()));
    info.env_http_headers = None;
    let endpoint = OpenAiModelsEndpoint::new(info, Some(Arc::clone(&credentials)));
    let error = endpoint
        .list_models("test", factory.clone())
        .await
        .unwrap_err();
    assert!(matches!(
        error.details(),
        codex_protocol::error::CodexErrorDetails::RequestTimeout
    ));

    // Wait through the existing refresh lock; this must reuse the completed refresh.
    let current = timeout(Duration::from_secs(5), credentials.auth())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.get_token().unwrap(), new_token);
    let saved = codex_login::load_auth_dot_json(
        home.path(),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::Direct,
    )
    .unwrap()
    .unwrap()
    .tokens
    .unwrap();
    assert_eq!(saved.access_token, new_token);
    assert_eq!(saved.refresh_token, "refresh-after");

    endpoint.list_models("test", factory).await.unwrap();
    let requests = server.received_requests().await.unwrap();
    let models = requests
        .iter()
        .find(|request| request.url.path() == "/models")
        .unwrap();
    assert_eq!(
        models.headers["authorization"],
        format!("Bearer {new_token}")
    );
    server.verify().await;
}
