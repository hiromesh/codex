//! Agent-message wakes preserve stop, Plan-mode, and turn-completion boundaries.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Result;
use codex_core::NotSubmittedReason;
use codex_core::StartIfIdleSubmission;
use codex_core::TurnInputRequest;
use codex_core::TurnInputSubmission;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_features::Feature;
use codex_history::RolloutItem;
use codex_protocol::AgentPath;
use codex_protocol::config_types::ModeKind;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::user_input::UserInput;
use core_test_support::ThreadIdle;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use test_case::test_case;
use tokio::sync::Notify;
use tokio::sync::oneshot;
use tokio::time::timeout;

#[derive(Default)]
struct PauseOnce {
    gate: Mutex<Option<oneshot::Receiver<()>>>,
    entered: Notify,
}

impl PauseOnce {
    fn arm(&self) -> oneshot::Sender<()> {
        let (release, gate) = oneshot::channel();
        assert!(
            self.gate
                .lock()
                .expect("pause lock")
                .replace(gate)
                .is_none()
        );
        release
    }

    async fn pause(&self) {
        let gate = self.gate.lock().expect("pause lock").take();
        if let Some(gate) = gate {
            self.entered.notify_one();
            gate.await.expect("test should release the pause");
        }
    }
}

#[derive(Default)]
struct WakeBarriers {
    completion: PauseOnce,
}

impl codex_extension_api::TurnLifecycleContributor for WakeBarriers {
    fn on_turn_stop<'a>(
        &'a self,
        _input: codex_extension_api::TurnStopInput<'a>,
    ) -> codex_extension_api::ExtensionFuture<'a, ()> {
        Box::pin(self.completion.pause())
    }
}

#[derive(Clone, Copy)]
enum AgentMessageWake {
    Enabled,
    Disabled,
}

#[test_case(AgentMessageWake::Enabled; "enabled")]
#[test_case(AgentMessageWake::Disabled; "disabled")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_during_completion_preserves_running_status(
    setting: AgentMessageWake,
) -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let response = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_assistant_message("answer", "Finished."),
            responses::ev_completed("response"),
        ]),
    )
    .await;
    let barriers = Arc::new(WakeBarriers::default());
    let release_completion = barriers.completion.arm();
    let mut extensions = ExtensionRegistryBuilder::new();
    extensions.turn_lifecycle_contributor(barriers.clone());
    let test = test_codex()
        .with_extensions(Arc::new(extensions.build()))
        .with_config(move |config| {
            match setting {
                AgentMessageWake::Enabled => config.features.enable(Feature::WakeOnAgentMessages),
                AgentMessageWake::Disabled => config.features.disable(Feature::WakeOnAgentMessages),
            }
            .expect("configure agent-message wakes");
        })
        .build_with_auto_env(&server)
        .await?;
    let TurnInputSubmission::Started { turn_id, .. } = test
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "Finish the work.".to_owned(),
            text_elements: Vec::new(),
        }]))
        .await?
    else {
        anyhow::bail!("the initial input must start a turn");
    };
    // The stop callback runs after the task is detached, before TurnComplete
    // is persisted. This is a finishing turn, not an unstarted wake reservation.
    timeout(
        Duration::from_secs(10),
        barriers.completion.entered.notified(),
    )
    .await?;
    assert_eq!(test.codex.agent_status().await, AgentStatus::Running);
    test.codex.submit(Op::Interrupt).await?;
    test.codex
        .update_thread_settings(ThreadSettingsOverrides::default())
        .await?;
    let status_while_finishing = test.codex.agent_status().await;

    release_completion.send(()).expect("completion is paused");
    let completed = wait_for_event(
        &test.codex,
        |event| matches!(event, EventMsg::TurnComplete(done) if done.turn_id == turn_id),
    )
    .await;
    let rollout_path = test.codex.rollout_path().expect("rollout path");
    test.codex.shutdown_and_wait().await?;

    // Interrupted would make a child eligible for capacity eviction while its
    // completion callback still owns the rollout writer.
    assert_eq!(status_while_finishing, AgentStatus::Running);
    let terminal_events = std::fs::read_to_string(rollout_path)?
        .lines()
        .map(codex_rollout::parse_rollout_line)
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .filter_map(|line| match line.item {
            RolloutItem::EventMsg(
                event @ (EventMsg::TurnComplete(_) | EventMsg::TurnAborted(_)),
            ) => Some(event),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        serde_json::to_value(terminal_events)?,
        serde_json::to_value(vec![completed])?
    );
    response.single_request();
    Ok(())
}
