use anyhow::Context;
use anyhow::Result;
use codex_core::StartThreadOptions;
use codex_core::TurnInputRequest;
use codex_core::shell::ShellType;
use codex_core::shell::get_shell;
use codex_features::Feature;
use codex_protocol::protocol::EnvironmentConfigState;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::HookRunStatus;
use codex_protocol::protocol::TurnEnvironmentSelection;
use codex_protocol::user_input::UserInput;
use codex_utils_path_uri::LegacyAppPathString;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_response_once;
use core_test_support::responses::sse;
use core_test_support::responses::sse_response;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::environment_config_for_selection;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use core_test_support::wait_for_event_match;
use pretty_assertions::assert_eq;
use serde_json::json;
use test_case::test_case;
use wiremock::ResponseTemplate;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn environment_requires_loadable_managed_hooks_at_startup_and_readiness() -> Result<()> {
    let server = start_mock_server().await;
    let test = test_codex()
        .with_config(|config| {
            config
                .features
                .enable(Feature::CodexHooks)
                .expect("enable hooks");
            config
                .features
                .enable(Feature::DeferredExecutor)
                .expect("allow the environment to start with pending configuration");
        })
        .build_with_auto_env(&server)
        .await?;
    let mut selection = test.executor_environment().selection().clone();
    let mut config = environment_config_for_selection(&test.config, &selection);
    config.environment_hooks = toml::from_str(
        r#"
        [[PreToolUse]]
        matcher = "["
        [[PreToolUse.hooks]]
        type = "command"
        command = "echo managed-hook"
        "#,
    )?;
    selection.config = EnvironmentConfigState::Ready(config.clone());
    let start = |selection: TurnEnvironmentSelection| {
        test.thread_manager.start_thread(StartThreadOptions {
            environments: Some(vec![selection.into_request()]),
            ..StartThreadOptions::new(test.config.clone())
        })
    };
    let assert_invalid_matcher = |error: String| {
        assert!(error.contains("invalid matcher"), "{error}");
        assert!(error.contains(&selection.environment_id), "{error}");
    };
    let error = start(selection.clone())
        .await
        .err()
        .context("startup accepted an invalid managed hook")?;
    assert_invalid_matcher(error.to_string());

    selection.config = EnvironmentConfigState::Pending;
    let thread = start(selection.clone()).await?.thread;
    let error = thread
        .environment_ready(&selection, config.clone())
        .await
        .expect_err("an invalid managed hook must prevent readiness");
    assert_invalid_matcher(error.to_string());
    assert_eq!(
        thread.environment_selections().await,
        vec![selection.clone()]
    );

    config.environment_hooks.pre_tool_use[0].matcher = Some("^Bash$".to_string());
    thread.environment_ready(&selection, config).await?;
    assert!(
        thread
            .config_snapshot()
            .await
            .is_primary_environment_configured()
    );
    thread.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thread_lists_the_environment_hook_that_blocks_a_turn() -> Result<()> {
    let server = start_mock_server().await;
    let test = test_codex()
        .with_config(|config| {
            config
                .features
                .enable(Feature::CodexHooks)
                .expect("enable hooks");
        })
        .build_with_auto_env(&server)
        .await?;
    let thread = &test.codex;
    let initial = thread.list_hooks();
    let selection = thread
        .environment_selections()
        .await
        .into_iter()
        .next()
        .context("thread should have a selected environment")?;
    let mut config = environment_config_for_selection(&test.config, &selection);
    config.environment_hooks = toml::from_str(
        r#"
        [[UserPromptSubmit]]
        [[UserPromptSubmit.hooks]]
        type = "command"
        command = "echo blocked-by-listed-hook >&2; exit 2"
    "#,
    )?;
    thread.environment_ready(&selection, config).await?;

    let listed = thread.list_hooks();
    let entry = listed
        .hooks
        .iter()
        .find(|hook| hook.environment_id.as_deref() == Some(selection.environment_id.as_str()))
        .context("list should include this environment's hook")?;
    thread
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "this turn should be blocked".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let run = wait_for_event_match(thread, |event| match event {
        EventMsg::HookCompleted(event)
            if event.run.source_path == LegacyAppPathString::from_abs_path(&entry.source_path) =>
        {
            Some(event.run.clone())
        }
        _ => None,
    })
    .await;
    assert_eq!(run.status, HookRunStatus::Blocked);
    assert_eq!(run.event_name, entry.event_name);
    assert_eq!(run.display_order, entry.display_order);

    wait_for_event(thread, |event| matches!(event, EventMsg::TurnComplete(_))).await;
    thread
        .environment_failed(&selection, "executor disconnected".into())
        .await?;
    assert_eq!(thread.list_hooks(), initial);
    Ok(())
}

const HOOK_INPUT_PROMPT: &str = "environment-hook-prompt";

#[test_case(
    ShellType::PowerShell,
    "& (Get-Process -Id $PID).Path -NoProfile -Command '[Console]::Error.WriteLine(([Console]::In.ReadLine() | ConvertFrom-Json).prompt); exit 2'",
    HookRunStatus::Blocked, HOOK_INPUT_PROMPT;
    "PowerShell preserves a native hook's blocking exit status"
)]
#[test_case(
    ShellType::PowerShell, "ForEach-Object { ($_ | ConvertFrom-Json).prompt }",
    HookRunStatus::Completed, HOOK_INPUT_PROMPT;
    "PowerShell delivers the prompt to a successful hook"
)]
#[test_case(
    ShellType::PowerShell, "ForEach-Object { $_ }; Write-Error 'hook failed'",
    HookRunStatus::Failed, "hook exited with code 1";
    "PowerShell reports a cmdlet failure"
)]
#[cfg_attr(unix, test_case(
    ShellType::Bash, "(grep -o environment-hook-prompt)",
    HookRunStatus::Completed, HOOK_INPUT_PROMPT;
    "Bash delivers the prompt and EOF to a hook starting with a subshell"
))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn environment_hook_receives_prompt_and_reports_exit_status(
    shell_type: ShellType,
    command: &str,
    expected_status: HookRunStatus,
    expected_output: &str,
) -> Result<()> {
    let Some(shell) = get_shell(shell_type) else {
        assert!(
            cfg!(unix) && shell_type == ShellType::PowerShell,
            "expected {shell_type:?} to be installed"
        );
        return Ok(());
    };
    let server = start_mock_server().await;
    mount_response_once(&server, completed_turn_response("after-hook")).await;
    let test = test_codex()
        .with_config(|config| {
            config
                .features
                .enable(Feature::CodexHooks)
                .expect("enable hooks");
        })
        .with_user_shell(shell)
        .build(&server)
        .await?;
    let thread = &test.codex;
    let selection = thread
        .environment_selections()
        .await
        .into_iter()
        .next()
        .context("thread should have a selected environment")?;
    let mut config = environment_config_for_selection(&test.config, &selection);
    config.environment_hooks = serde_json::from_value(json!({
        "UserPromptSubmit": [{"hooks": [{"type": "command", "command": command}]}]
    }))?;
    thread.environment_ready(&selection, config).await?;

    thread
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: HOOK_INPUT_PROMPT.to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let run = wait_for_event_match(thread, |event| match event {
        EventMsg::HookCompleted(event) => Some(event.run.clone()),
        _ => None,
    })
    .await;
    let entries: Vec<_> = run
        .entries
        .iter()
        .map(|entry| entry.text.as_str())
        .collect();
    assert_eq!(
        (run.status, entries),
        (expected_status, vec![expected_output])
    );
    wait_for_event(thread, |event| matches!(event, EventMsg::TurnComplete(_))).await;
    Ok(())
}

fn completed_turn_response(id: &str) -> ResponseTemplate {
    sse_response(sse(vec![
        ev_response_created(id),
        ev_assistant_message(id, "done"),
        ev_completed(id),
    ]))
}
