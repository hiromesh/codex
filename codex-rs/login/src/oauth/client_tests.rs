//! Exercises OAuth operations over HTTP, including grant binding and rejection diagnostics.

use std::collections::BTreeMap;

use codex_http_client::HttpClient;
use codex_http_client::HttpClientBuilder;
use http::HeaderMap;
use http::HeaderValue;
use http::StatusCode;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;

use super::*;

fn oauth<'a>(http: &'a HttpClient, url: &'a str, limit: ErrorBodyLimit) -> OAuthClient<'a> {
    OAuthClient::new(
        http,
        TokenEndpoint {
            url,
            client_id: "client id",
            encoding: TokenEncoding::Json,
            timeout: None,
            error_body_limit: limit,
        },
    )
}

#[tokio::test]
async fn rejection_preserves_status_and_omits_oversized_diagnostics() {
    let http = HttpClientBuilder::new().build_direct().unwrap();
    const BODY_LIMIT: usize = 8192;
    let refresh_token = "credential+crossing&the=limit /%";
    let form_encoded: String =
        url::form_urlencoded::byte_serialize(refresh_token.as_bytes()).collect();
    let long_body = format!("{}{form_encoded}", "x".repeat(BODY_LIMIT - 2));
    let redacted = long_body.replace(&form_encoded, "[REDACTED]");
    let top_level_error = json!({
        "code": "configuration_error",
        "message": format!("Unknown tenant: {form_encoded}"),
    })
    .to_string();
    let redacted_top_level_error = top_level_error.replace(&form_encoded, "[REDACTED]");
    for (limit, body, expected_display) in [
        (
            ErrorBodyLimit::Bytes(BODY_LIMIT),
            json!({"error": "invalid_grant", "error_description": form_encoded}).to_string(),
            "[REDACTED]",
        ),
        (
            ErrorBodyLimit::Unlimited,
            top_level_error,
            redacted_top_level_error.as_str(),
        ),
        (
            ErrorBodyLimit::Bytes(BODY_LIMIT),
            long_body.clone(),
            "unknown error",
        ),
        (
            ErrorBodyLimit::Unlimited,
            long_body.clone(),
            redacted.as_str(),
        ),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(move |request: &wiremock::Request| {
                decode_form(&request.body)
                    == json!({
                        "client_id": "client id", "grant_type": "refresh_token",
                        "refresh_token": refresh_token, "resource": "urn:gateway",
                    })
            })
            .respond_with(
                ResponseTemplate::new(/*s*/ 401)
                    .insert_header("x-request-id", format!("request-{form_encoded}"))
                    .set_body_string(body),
            )
            .expect(/*r*/ 1)
            .mount(&server)
            .await;
        let endpoint = server.uri();
        let result = oauth(&http, &endpoint, limit)
            .refresh::<Value>(RefreshTokenGrant {
                refresh_token,
                resource: Some("urn:gateway"),
                refresh_reason: None,
            })
            .await;
        let Err(OAuthError::Rejected(error)) = result else {
            panic!("expected OAuth rejection")
        };
        assert_eq!(
            (
                error.status,
                error.request_id.as_deref(),
                error.detail.to_string(),
                error.body_read_error.is_none()
            ),
            (
                StatusCode::UNAUTHORIZED,
                Some("request-[REDACTED]"),
                expected_display.to_string(),
                true
            )
        );
    }
}

#[tokio::test]
async fn rejection_keeps_status_when_reading_the_body_fails() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"refresh_token=refresh") {
            let mut buffer = [0; 4096];
            let read = stream.read(&mut buffer).await.unwrap();
            assert!(read > 0);
            request.extend_from_slice(&buffer[..read]);
        }
        stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 100\r\nConnection: close\r\n\r\npartial").await.unwrap();
    });
    let http = HttpClientBuilder::new().build_direct().unwrap();
    let endpoint = format!("http://{address}");
    let result = oauth(&http, &endpoint, ErrorBodyLimit::Unlimited)
        .refresh::<Value>(RefreshTokenGrant {
            refresh_token: "refresh",
            resource: None,
            refresh_reason: None,
        })
        .await;
    let Err(OAuthError::Rejected(error)) = result else {
        panic!("expected OAuth rejection")
    };
    assert_eq!(error.status, StatusCode::UNAUTHORIZED);
    assert!(error.body_read_error.is_some());
    server.await.unwrap();
}

fn decode_form(body: &[u8]) -> Value {
    serde_json::to_value(
        url::form_urlencoded::parse(body)
            .into_owned()
            .collect::<BTreeMap<_, _>>(),
    )
    .unwrap()
}

#[tokio::test]
async fn success_body_revocation_preserves_the_policy_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let controller = codex_http_client::NetworkPolicyController::default();
    controller.publish(
        controller.policy().revision(),
        codex_http_client::DestinationPolicy::Unrestricted,
    );
    let http = codex_http_client::HttpClientFactory::new(
        codex_http_client::OutboundProxyPolicy::ReqwestDefault,
    )
    .with_network_policy(controller.policy())
    .build_client(&endpoint, codex_http_client::ClientRouteClass::Auth)
    .unwrap();
    let server = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"refresh_token=refresh") {
            let mut buffer = [0; 4096];
            let read = stream.read(&mut buffer).await.unwrap();
            assert!(read > 0);
            request.extend_from_slice(&buffer[..read]);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{\"access_token\":")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(/*millis*/ 50)).await;
        controller.publish(
            controller.policy().revision(),
            codex_http_client::DestinationPolicy::Restricted {
                allowed_hosts: Default::default(),
            },
        );
    };
    let form = oauth(&http, &endpoint, ErrorBodyLimit::Unlimited);
    let (result, ()) = tokio::time::timeout(Duration::from_secs(/*secs*/ 5), async {
        tokio::join!(
            form.refresh::<Value>(RefreshTokenGrant {
                refresh_token: "refresh",
                resource: None,
                refresh_reason: None,
            }),
            server
        )
    })
    .await
    .unwrap();
    assert!(matches!(
        result,
        Err(OAuthError::Transport(codex_http_client::HttpError::Policy(
            codex_http_client::NetworkPolicyDenied::Revoked
        )))
    ));
}
