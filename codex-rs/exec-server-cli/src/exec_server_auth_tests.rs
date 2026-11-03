#![cfg(test)]

use std::path::Path;
use std::time::Duration;

use codex_login::AuthCredentialsStoreMode;
use codex_login::AuthKeyringBackendKind;
use http::HeaderValue;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::*;

fn write_chatgpt_auth(codex_home: &Path, token: &str, account: &str) {
    write_chatgpt_auth_with_claims(
        codex_home,
        token,
        Some(account),
        serde_json::json!({"chatgpt_user_id": "user", "chatgpt_plan_type": "pro"}),
    );
}

fn write_chatgpt_auth_with_claims(
    codex_home: &Path,
    token: &str,
    account: Option<&str>,
    claims: serde_json::Value,
) {
    let id_token = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &serde_json::json!({"https://api.openai.com/auth": claims}),
        &jsonwebtoken::EncodingKey::from_secret(b"executor-auth-test"),
    )
    .unwrap();
    std::fs::write(
        codex_home.join("auth.json"),
        serde_json::to_vec(&serde_json::json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": id_token,
                "access_token": token,
                "refresh_token": "test-refresh-token",
                "account_id": account,
            },
            "last_refresh": chrono::Utc::now(),
        }))
        .unwrap(),
    )
    .unwrap();
}

async fn stored_chatgpt_provider(codex_home: &Path) -> SharedAuthProvider {
    let auth_manager = Arc::new(
        AuthManager::new(
            codex_home.to_path_buf(),
            /*enable_codex_api_key_env*/ false,
            AuthCredentialsStoreMode::File,
            /*forced_chatgpt_workspace_id*/ None,
            /*chatgpt_base_url*/ None,
            AuthKeyringBackendKind::default(),
            codex_login::test_support::transport_default_auth_route_config(),
        )
        .await,
    );
    let initial_auth = auth_manager.auth_cached().expect("load initial auth");
    chatgpt_auth_provider(auth_manager, &initial_auth).unwrap()
}

#[tokio::test]
async fn chatgpt_executor_follows_disk_rotation_without_restart() {
    let home = TempDir::new().unwrap();
    write_chatgpt_auth(home.path(), "initial-token", "account");
    let provider = stored_chatgpt_provider(home.path()).await;
    for token in ["initial-token", "rotated-token", "rotated-again"] {
        write_chatgpt_auth(home.path(), token, "account");
        let headers = provider.apply_auth_headers(HeaderMap::new()).await.unwrap();
        let mut expected = HeaderMap::new();
        expected.insert(
            http::header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        expected.insert("ChatGPT-Account-ID", HeaderValue::from_static("account"));
        assert_eq!(headers, expected);
    }
}

#[tokio::test]
async fn chatgpt_executor_rejects_account_switch_and_recovers_original_identity() {
    let home = TempDir::new().unwrap();
    write_chatgpt_auth(home.path(), "initial-token", "account");
    let provider = stored_chatgpt_provider(home.path()).await;

    let rejected = provider.to_auth_headers().unwrap();
    write_chatgpt_auth(home.path(), "other-token", "other-account");
    assert!(!provider.refresh_after_unauthorized(rejected.clone()).await);
    assert!(provider.apply_auth_headers(HeaderMap::new()).await.is_err());
    assert!(provider.to_auth_headers().is_err());

    write_chatgpt_auth(home.path(), "restored-token", "account");
    assert!(provider.refresh_after_unauthorized(rejected).await);
    assert_eq!(
        provider.apply_auth_headers(HeaderMap::new()).await.unwrap()[http::header::AUTHORIZATION],
        "Bearer restored-token"
    );
}

#[tokio::test]
async fn chatgpt_executor_does_not_reuse_credentials_after_logout() {
    let home = TempDir::new().unwrap();
    write_chatgpt_auth(home.path(), "initial-token", "account");
    let provider = stored_chatgpt_provider(home.path()).await;
    std::fs::remove_file(home.path().join("auth.json")).unwrap();
    assert!(provider.apply_auth_headers(HeaderMap::new()).await.is_err());
    assert!(provider.to_auth_headers().is_err());
}

#[tokio::test]
async fn executor_rejects_incomplete_or_empty_bearer() {
    for missing_refresh in [true, false] {
        let home = TempDir::new().unwrap();
        write_chatgpt_auth(home.path(), "initial", "account");
        let provider = stored_chatgpt_provider(home.path()).await;
        let path = home.path().join("auth.json");
        let mut doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        if missing_refresh {
            doc.as_object_mut().unwrap().remove("last_refresh");
        } else {
            doc["tokens"]["access_token"] = serde_json::json!("");
        }
        std::fs::write(path, serde_json::to_vec(&doc).unwrap()).unwrap();
        assert!(matches!(
            provider.apply_auth_headers(HeaderMap::new()).await,
            Err(AuthError::Build(_))
        ));
    }
}

#[tokio::test]
async fn chatgpt_executor_rejects_rotation_with_incomplete_identity() {
    for (account, user) in [
        (Some("account"), None),
        (None, Some("user")),
        (Some("account"), Some("")),
        (Some(" "), Some("user")),
    ] {
        let home = TempDir::new().unwrap();
        let claims = serde_json::json!({"chatgpt_user_id": user});
        write_chatgpt_auth_with_claims(home.path(), "initial", account, claims.clone());
        let provider = stored_chatgpt_provider(home.path()).await;
        assert!(provider.apply_auth_headers(HeaderMap::new()).await.is_ok());
        assert!(
            !provider
                .refresh_after_unauthorized(provider.to_auth_headers().unwrap())
                .await
        );
        write_chatgpt_auth_with_claims(home.path(), "replacement", account, claims);
        assert!(provider.apply_auth_headers(HeaderMap::new()).await.is_err());
        assert!(provider.to_auth_headers().is_err());
    }
}

#[tokio::test]
async fn chatgpt_executor_rejects_changed_user_and_workspace_policy() {
    for claims in [
        serde_json::json!({"chatgpt_user_id": "other-user", "chatgpt_plan_type": "pro"}),
        serde_json::json!({"chatgpt_user_id": "user", "chatgpt_plan_type": "enterprise"}),
        serde_json::json!({"chatgpt_user_id": "user", "chatgpt_plan_type": "pro", "chatgpt_account_is_fedramp": true}),
    ] {
        let home = TempDir::new().unwrap();
        write_chatgpt_auth(home.path(), "initial", "account");
        let provider = stored_chatgpt_provider(home.path()).await;
        write_chatgpt_auth_with_claims(home.path(), "replacement", Some("account"), claims);
        assert!(provider.apply_auth_headers(HeaderMap::new()).await.is_err());
        assert!(provider.to_auth_headers().is_err());
    }
}

#[tokio::test]
async fn executor_concurrent_rotation_checks_identity_before_refresh() {
    const CHILD_MODE: &str = "CODEX_EXECUTOR_AUTH_TEST_MODE";
    let Ok(mode) = std::env::var(CHILD_MODE) else {
        // Run in subprocesses so endpoint overrides cannot affect parallel tests.
        let server = MockServer::start().await;
        for token in [
            "at-initial",
            "at-rotated",
            "at-retry-original",
            "at-retry-new",
        ] {
            Mock::given(method("GET"))
                .and(path("/v1/user-auth-credential/whoami"))
                .and(header("authorization", format!("Bearer {token}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "chatgpt_user_id": "user",
                    "chatgpt_account_id": "account",
                    "chatgpt_plan_type": "pro",
                    "chatgpt_account_is_fedramp": false,
                })))
                .with_priority(2)
                .up_to_n_times(1)
                .expect(1)
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/v1/user-auth-credential/whoami"))
            .and(header("authorization", "Bearer at-retry-new"))
            .respond_with(ResponseTemplate::new(503))
            .with_priority(1)
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "refreshed",
                "refresh_token": "refreshed-refresh-token",
            })))
            .with_priority(1)
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(503))
            .with_priority(10)
            .expect(0)
            .mount(&server)
            .await;
        for mode in ["pat", "chatgpt", "kind-switch", "pat-outage"] {
            let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "exec_server_auth::tests::executor_concurrent_rotation_checks_identity_before_refresh",
                    "--nocapture",
                ])
                .env(CHILD_MODE, mode)
                .env_remove("CODEX_ACCESS_TOKEN")
                .env("NO_PROXY", "127.0.0.1,localhost")
                .env("no_proxy", "127.0.0.1,localhost")
                .env("CODEX_AUTHAPI_BASE_URL", server.uri())
                .env("CODEX_AGENT_IDENTITY_AUTHAPI_BASE_URL", server.uri())
                .env("CODEX_REFRESH_TOKEN_URL_OVERRIDE", format!("{}/oauth/token", server.uri()))
                .kill_on_drop(true);
            let output = tokio::time::timeout(Duration::from_secs(30), child.output())
                .await
                .unwrap()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        }
        server.verify().await;
        return;
    };

    let home = TempDir::new().unwrap();
    let auth_path = home.path().join("auth.json");
    if mode == "pat" || mode == "pat-outage" {
        let token = if mode == "pat" {
            "at-initial"
        } else {
            "at-retry-original"
        };
        std::fs::write(
            &auth_path,
            serde_json::to_vec(&serde_json::json!({"personal_access_token":token})).unwrap(),
        )
        .unwrap();
    } else {
        write_chatgpt_auth(home.path(), "initial", "account");
    }
    let provider = stored_chatgpt_provider(home.path()).await;
    if mode == "pat-outage" {
        std::fs::write(&auth_path, r#"{"personal_access_token":"at-retry-new"}"#).unwrap();
        assert!(matches!(
            provider.apply_auth_headers(HeaderMap::new()).await,
            Err(AuthError::Transient(_))
        ));
        assert!(provider.to_auth_headers().is_err());
        let headers = provider.apply_auth_headers(HeaderMap::new()).await.unwrap();
        assert_eq!(headers[http::header::AUTHORIZATION], "Bearer at-retry-new");
        assert_eq!(provider.to_auth_headers().unwrap(), headers);
        return;
    }
    if mode == "kind-switch" {
        std::fs::write(&auth_path, r#"{"personal_access_token":"at-initial"}"#).unwrap();
        assert!(matches!(
            provider.apply_auth_headers(HeaderMap::new()).await,
            Err(AuthError::Build(_))
        ));
        return;
    }
    let expected_bearer = if mode == "pat" {
        std::fs::write(&auth_path, r#"{"personal_access_token":"at-rotated"}"#).unwrap();
        "Bearer at-rotated"
    } else {
        let expired = jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &serde_json::json!({"exp": chrono::Utc::now().timestamp() - 60}),
            &jsonwebtoken::EncodingKey::from_secret(b"executor-auth-test"),
        )
        .unwrap();
        for (account, user) in [
            ("other-account", "user"),
            ("account", "other-user"),
            ("account", "user"),
        ] {
            write_chatgpt_auth_with_claims(
                home.path(),
                &expired,
                Some(account),
                serde_json::json!({"chatgpt_user_id": user, "chatgpt_plan_type": "pro"}),
            );
            let original = std::fs::read(&auth_path).unwrap();
            if account != "account" || user != "user" {
                assert!(provider.apply_auth_headers(HeaderMap::new()).await.is_err());
                assert_eq!(std::fs::read(&auth_path).unwrap(), original);
            }
        }
        "Bearer refreshed"
    };
    let barrier = Arc::new(tokio::sync::Barrier::new(32));
    let results = futures::future::join_all((0..32).map(|_| {
        let provider = Arc::clone(&provider);
        let barrier = Arc::clone(&barrier);
        async move {
            barrier.wait().await;
            provider.apply_auth_headers(HeaderMap::new()).await
        }
    }))
    .await;
    for result in results {
        assert_eq!(
            result.unwrap()[http::header::AUTHORIZATION],
            expected_bearer
        );
    }
}

#[tokio::test]
async fn executor_refresh_survives_cancelled_caller() {
    use std::process::Stdio;
    use tokio::io::AsyncBufReadExt;
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;

    const CHILD_MODE: &str = "CODEX_EXECUTOR_CANCEL_TEST_MODE";
    let Ok(mode) = std::env::var(CHILD_MODE) else {
        // Isolate endpoint overrides; release the response only after the caller
        // is cancelled, with no sleep or race against the HTTP response.
        for mode in ["proactive", "recovery", "logout", "replacement", "timeout"] {
            tokio::time::timeout(Duration::from_secs(30), async {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
                let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "exec_server_auth::tests::executor_refresh_survives_cancelled_caller", "--nocapture"])
                    .env(CHILD_MODE, mode)
                    .env_remove("CODEX_ACCESS_TOKEN")
                    .env("CODEX_REFRESH_TOKEN_URL_OVERRIDE", endpoint)
                    .env("NO_PROXY", "127.0.0.1,localhost")
                    .env("no_proxy", "127.0.0.1,localhost")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true)
                    .spawn().unwrap();
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = tokio::io::BufReader::new(&mut stream);
                let mut content_length = 0;
                loop {
                    let mut line = String::new();
                    assert!(request.read_line(&mut line).await.unwrap() > 0);
                    if line == "\r\n" { break; }
                    if let Some(length) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        content_length = length.trim().parse().unwrap();
                    }
                }
                request.read_exact(&mut vec![0; content_length]).await.unwrap();
                drop(request);
                child.stdin.take().unwrap().write_all(b"cancel\n").await.unwrap();
                let mut stdout = tokio::io::BufReader::new(child.stdout.take().unwrap());
                loop {
                    let mut line = String::new();
                    assert!(stdout.read_line(&mut line).await.unwrap() > 0);
                    if line.contains("CALLER_CANCELLED") { break; }
                }
                let body = r#"{"access_token":"rotated-access","refresh_token":"rotated-refresh"}"#;
                if mode != "timeout" {
                    stream.write_all(format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()
                    ).as_bytes()).await.unwrap();
                }
                // Keep the unanswered timeout connection open until the child finishes.
                let mut remaining = String::new();
                stdout.read_to_string(&mut remaining).await.unwrap();
                let output = child.wait_with_output().await.unwrap();
                assert!(output.status.success(), "{remaining}\n{output:?}");
                assert!(remaining.contains("1 passed"), "{remaining}");
            }).await.expect("cancelled refresh must finish persisting");
        }
        return;
    };

    let home = TempDir::new().unwrap();
    let token = if mode != "recovery" {
        jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &serde_json::json!({"exp": chrono::Utc::now().timestamp() - 60}),
            &jsonwebtoken::EncodingKey::from_secret(b"executor-auth-test"),
        )
        .unwrap()
    } else {
        "initial-access".to_string()
    };
    write_chatgpt_auth(home.path(), &token, "account");
    let provider = stored_chatgpt_provider(home.path()).await;
    let caller = Arc::clone(&provider);
    let rejected = provider.to_auth_headers().unwrap();
    let refresh_mode = mode.clone();
    let task = tokio::spawn(async move {
        if refresh_mode != "recovery" {
            caller.apply_auth_headers(HeaderMap::new()).await.unwrap();
        } else {
            assert!(caller.refresh_after_unauthorized(rejected).await);
        }
    });
    let mut instruction = String::new();
    tokio::io::BufReader::new(tokio::io::stdin())
        .read_line(&mut instruction)
        .await
        .unwrap();
    assert_eq!(instruction, "cancel\n");
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    if mode == "logout" {
        std::fs::remove_file(home.path().join("auth.json")).unwrap();
    } else if mode == "replacement" {
        write_chatgpt_auth(home.path(), "fresh-external-access", "account");
        let path = home.path().join("auth.json");
        let mut stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        stored["tokens"]["refresh_token"] = serde_json::json!("externally-rotated-refresh");
        std::fs::write(path, serde_json::to_vec(&stored).unwrap()).unwrap();
    }
    println!("CALLER_CANCELLED");
    if mode == "timeout" {
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(31)).await;
        tokio::time::resume();
        assert!(matches!(
            provider.apply_auth_headers(HeaderMap::new()).await,
            Err(AuthError::Transient(_))
        ));
        // Repair is usable without releasing the old response or restarting the executor.
        write_chatgpt_auth(home.path(), "fresh-external-access", "account");
    }
    if mode == "logout" {
        assert!(matches!(
            provider.apply_auth_headers(HeaderMap::new()).await,
            Err(AuthError::Build(_))
        ));
        assert!(!home.path().join("auth.json").exists());
        return;
    }
    let repaired = mode == "replacement" || mode == "timeout";
    // This waits on the same permit, proving the detached operation kept it
    // until both persistence and cache update were complete.
    let headers = provider.apply_auth_headers(HeaderMap::new()).await.unwrap();
    assert_eq!(
        headers[http::header::AUTHORIZATION],
        if repaired {
            "Bearer fresh-external-access"
        } else {
            "Bearer rotated-access"
        }
    );
    let stored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.path().join("auth.json")).unwrap()).unwrap();
    assert_eq!(
        stored["tokens"]["access_token"],
        if repaired {
            "fresh-external-access"
        } else {
            "rotated-access"
        }
    );
    assert_eq!(
        stored["tokens"]["refresh_token"],
        match mode.as_str() {
            "replacement" => "externally-rotated-refresh",
            "timeout" => "test-refresh-token",
            _ => "rotated-refresh",
        }
    );
}

