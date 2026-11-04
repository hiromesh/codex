use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::CodexErrorInfo;
use codex_app_server_protocol::CyberAccessProgram;
use codex_app_server_protocol::ExperimentalFeatureEnablementSetParams;
use codex_app_server_protocol::ExperimentalFeatureEnablementSetResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::TurnError;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::UserInput;
use codex_login::AuthCredentialsStoreMode;
use codex_login::AuthKeyringBackendKind;
use codex_login::login_with_api_key;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::Path;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[tokio::test]
async fn api_key_cyber_access_program_requires_only_forwarding_feature() -> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let expected_programs = [
        None,
        None,
        None,
        Some(json!({"cyber": "daybreak_blue"})),
        Some(json!({"cyber": "daybreak_red"})),
        Some(json!({"cyber": "standard"})),
        None,
        Some(json!({"cyber": "daybreak_blue"})),
        Some(json!({"cyber": "daybreak_red"})),
        Some(json!({"cyber": "standard"})),
    ];
    let requests = responses::mount_sse_sequence(
        &server,
        expected_programs
            .iter()
            .enumerate()
            .map(|(index, _)| responses::sse_completed(&format!("resp-{index}")))
            .collect(),
    )
    .await;
    let home = TempDir::new()?;
    let mut app = start_api_key_app(home.path(), &server).await?;
    set_api_key_cyber_access_programs(&mut app, /*enabled*/ true).await?;
    for (forwarding_enabled, discovery_enabled) in
        [(false, false), (false, true), (true, false), (true, true)]
    {
        let thread = app
            .start_thread(ThreadStartParams {
                config: Some(HashMap::from([
                    (
                        "features.api_key_cyber_access_programs".to_owned(),
                        json!(forwarding_enabled),
                    ),
                    (
                        "features.api_key_model_discovery".to_owned(),
                        json!(discovery_enabled),
                    ),
                ])),
                ..Default::default()
            })
            .await?
            .thread;
        for program in [
            None,
            Some(CyberAccessProgram::DaybreakBlue),
            Some(CyberAccessProgram::DaybreakRed),
            Some(CyberAccessProgram::Standard),
        ] {
            let completed = app
                .start_turn_and_wait_for_completion(TurnStartParams {
                    thread_id: thread.id.clone(),
                    cyber_access_program: program,
                    input: vec![UserInput::Text {
                        text: "hello".to_owned(),
                        text_elements: Vec::new(),
                    }],
                    ..Default::default()
                })
                .await?;
            if program.is_some() && !forwarding_enabled {
                assert_eq!(completed.turn.status, TurnStatus::Failed);
                assert_eq!(
                    completed.turn.error,
                    Some(TurnError {
                        message: "Cyber access programs are disabled for this API-key session."
                            .to_owned(),
                        codex_error_info: Some(CodexErrorInfo::Other),
                        additional_details: None,
                        misalignment: None,
                    })
                );
            } else {
                assert_eq!(completed.turn.status, TurnStatus::Completed);
            }
        }
    }
    assert_eq!(
        requests
            .requests()
            .iter()
            .map(|request| (
                request.header("authorization"),
                request.body_json()["model"].clone(),
                request.body_json().get("access_programs").cloned(),
            ))
            .collect::<Vec<_>>(),
        expected_programs
            .into_iter()
            .map(|program| (
                Some("Bearer test-key".to_owned()),
                json!("gpt-6-sol"),
                program
            ))
            .collect::<Vec<_>>()
    );
    app.shutdown_gracefully().await?;
    Ok(())
}

#[tokio::test]
async fn api_key_cyber_access_program_server_rejection_does_not_fall_back() -> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(/*s*/ 403).set_body_json(json!({
            "error": {
                "type": "invalid_request_error",
                "code": "access_program_not_enabled",
                "message": "Daybreak Blue is not enabled for this project."
            }
        })))
        .expect(/*r*/ 1..)
        .mount(&server)
        .await;
    let home = TempDir::new()?;
    let mut app = start_api_key_app(home.path(), &server).await?;
    set_api_key_cyber_access_programs(&mut app, /*enabled*/ true).await?;
    let thread = app.start_thread(ThreadStartParams::default()).await?.thread;
    let completed = app
        .start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: thread.id,
            cyber_access_program: Some(CyberAccessProgram::DaybreakBlue),
            input: vec![UserInput::Text {
                text: "hello".to_owned(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    assert_eq!(completed.turn.status, TurnStatus::Failed);
    let error = completed.turn.error.expect("server rejection");
    assert_eq!(
        error.codex_error_info,
        Some(CodexErrorInfo::HttpConnectionFailed {
            http_status_code: Some(403),
        })
    );
    assert!(
        error
            .message
            .contains("Daybreak Blue is not enabled for this project.")
    );
    let requests = responses::received_responses_requests(&server).await;
    assert!(!requests.is_empty());
    // Existing transport retries may repeat the request; none may drop the program.
    assert_eq!(
        requests
            .iter()
            .map(|request| (
                request.header("authorization"),
                request.body_json()["access_programs"].clone()
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                Some("Bearer test-key".to_owned()),
                json!({"cyber": "daybreak_blue"})
            );
            requests.len()
        ]
    );
    app.shutdown_gracefully().await?;
    Ok(())
}

async fn start_api_key_app(home: &Path, server: &MockServer) -> Result<TestAppServer> {
    Mock::given(method("GET"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(/*s*/ 426))
        .mount(server)
        .await;
    // The existing inference override disables remote catalog discovery. Forwarding
    // must therefore work without granting programs through a test catalog.
    std::fs::write(
        home.join("config.toml"),
        format!(
            "model = \"gpt-6-sol\"\nopenai_base_url = \"{}/v1\"\n[features]\napi_key_model_discovery = false\n",
            server.uri()
        ),
    )?;
    login_with_api_key(
        home,
        "test-key",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;
    TestAppServer::builder()
        .with_codex_home(home)
        .without_managed_config()
        .with_env_overrides(&[
            ("OPENAI_API_KEY", None),
            ("CODEX_API_KEY", None),
            ("OPENAI_BASE_URL", None),
        ])
        .build_initialized()
        .await
}

async fn set_api_key_cyber_access_programs(app: &mut TestAppServer, enabled: bool) -> Result<()> {
    let enablement = BTreeMap::from([("api_key_cyber_access_programs".to_owned(), enabled)]);
    let response: ExperimentalFeatureEnablementSetResponse = app
        .request(
            |request_id| ClientRequest::ExperimentalFeatureEnablementSet {
                request_id,
                params: ExperimentalFeatureEnablementSetParams {
                    enablement: enablement.clone(),
                },
            },
        )
        .await?;
    assert_eq!(
        response,
        ExperimentalFeatureEnablementSetResponse { enablement }
    );
    Ok(())
}
