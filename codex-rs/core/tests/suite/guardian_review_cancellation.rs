use anyhow::Context;
use anyhow::Result;
use codex_core::TurnInputRequest;
use codex_core::config::Constrained;
use codex_features::Feature;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::GuardianAssessmentStatus;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::SandboxPolicy;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_custom_tool_call;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::sse;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_sandbox;
use core_test_support::test_codex::local_requests;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use serde_json::Value;
use serde_json::json;
use std::time::Duration;
use test_case::test_case;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_tool_aborts_its_guardian_review() -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));

    let server = responses::start_mock_server().await;
    let mut builder = test_codex()
        .with_model("test-gpt-5.1-codex")
        .with_config(move |config| {
            config.permissions.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);
            config.approvals_reviewer = ApprovalsReviewer::AutoReview;
            config
                .set_legacy_sandbox_policy(SandboxPolicy::new_workspace_write_policy())
                .expect("set sandbox policy");
        });
    let test = builder.build_with_auto_env(&server).await?;
    let output_file = test.cwd.path().join("cancelled-guardian-command.txt");
    let output_path = shlex::try_join([output_file.to_string_lossy().as_ref()])?;
    let tool_args = json!({
        "cmd": format!("printf should-not-run > {output_path}"),
        "sandbox_permissions": "require_escalated",
        "justification": "Exercise tool-owned Guardian cancellation.",
    });
    let tool_call = ev_function_call("reviewed-tool", "exec_command", &tool_args.to_string());
    responses::mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("parent-start"),
            tool_call,
            ev_completed("parent-start"),
        ]),
    )
    .await;
    let pending_guardian = responses::mount_response_once_match(
        &server,
        |request: &wiremock::Request| {
            serde_json::from_slice::<Value>(&request.body)
                .expect("Responses request body should be valid JSON")
                .pointer("/client_metadata/x-openai-subagent")
                .and_then(Value::as_str)
                == Some("guardian")
        },
        responses::sse_response(sse(vec![
            ev_response_created("pending-review"),
            ev_assistant_message(
                "review-result",
                &json!({
                    "risk_level": "low",
                    "user_authorization": "high",
                    "outcome": "allow",
                    "rationale": "The test must cancel this review before it completes.",
                })
                .to_string(),
            ),
            ev_completed("pending-review"),
        ]))
        .set_delay(Duration::from_secs(60)),
    )
    .await;

    test.codex
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: "start a Guardian-reviewed command".into(),
                text_elements: Vec::new(),
            }])
            .with_thread_settings(ThreadSettingsOverrides {
                environments: Some(local_requests(test.config.cwd.clone())),
                ..Default::default()
            }),
        )
        .await?;
    tokio::time::timeout(Duration::from_secs(10), async {
        while pending_guardian.requests().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("Guardian review did not start")?;

    test.codex.submit(Op::Interrupt).await?;

    tokio::time::timeout(Duration::from_secs(10), async {
        let mut guardian_aborted = false;
        let mut parent_finished = false;
        while !guardian_aborted || !parent_finished {
            let event = test.codex.next_event().await?;
            match event.msg {
                EventMsg::GuardianAssessment(assessment)
                    if assessment.status == GuardianAssessmentStatus::Aborted =>
                {
                    guardian_aborted = true;
                }
                EventMsg::TurnAborted(_) => {
                    parent_finished = true;
                }
                _ => {}
            }
        }
        anyhow::Ok(())
    })
    .await
    .context("tool cancellation did not abort Guardian")??;
    assert!(!output_file.exists(), "cancelled command executed");
    test.codex.shutdown_and_wait().await?;
    Ok(())
}
