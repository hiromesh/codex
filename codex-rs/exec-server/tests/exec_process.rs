mod common;
#[cfg(unix)]
#[path = "exec_process/shell_snapshot.rs"]
mod shell_snapshot;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use anyhow::Result;
use codex_exec_server::Environment;
use codex_exec_server::ExecBackend;
#[cfg(unix)]
use codex_exec_server::ExecEnvPolicy;
use codex_exec_server::ExecMetadata;
use codex_exec_server::ExecOutputStream;
use codex_exec_server::ExecParams;
use codex_exec_server::ExecProcess;
use codex_exec_server::ExecProcessEvent;
#[cfg(unix)]
use codex_exec_server::FileSystemSandboxContext;
use codex_exec_server::ProcessId;
use codex_exec_server::ProcessSignal;
use codex_exec_server::ReadResponse;
#[cfg(unix)]
use codex_exec_server::ShellInfo;
#[cfg(unix)]
use codex_exec_server::ShellSnapshotRequest;
use codex_exec_server::StartedExecProcess;
use codex_exec_server::WriteStatus;
#[cfg(unix)]
use codex_protocol::config_types::ShellEnvironmentPolicyInherit;
#[cfg(unix)]
use codex_protocol::models::PermissionProfile;
#[cfg(unix)]
use codex_protocol::permissions::FileSystemAccessMode;
#[cfg(unix)]
use codex_protocol::permissions::FileSystemSandboxEntry;
#[cfg(unix)]
use codex_protocol::permissions::FileSystemSandboxPolicy;
#[cfg(unix)]
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use test_case::test_case;
use tokio::sync::watch;
use tokio::time::Duration;
use tokio::time::sleep;
use tokio::time::timeout;

use common::DELAYED_OUTPUT_AFTER_EXIT_PARENT_ARG;
use common::current_test_binary_helper_paths;

struct ProcessContext {
    backend: Arc<dyn ExecBackend>,
}

#[derive(Debug, PartialEq, Eq)]
enum ProcessEventSnapshot {
    Output {
        seq: u64,
        stream: ExecOutputStream,
        text: String,
    },
    Exited {
        seq: u64,
        exit_code: i32,
    },
    Closed {
        seq: u64,
    },
}

async fn create_process_context() -> Result<ProcessContext> {
    let environment = Environment::create_for_tests(/*exec_server_url*/ None)?;
    Ok(ProcessContext {
        backend: environment.get_exec_backend(),
    })
}

#[cfg(unix)]
#[test_case(false, false, "bash"; "local_pipe")]
#[test_case(true, false, "bash"; "local_tty")]
#[test_case(false, false, "sh"; "local_sh_pipe")]
#[test_case(false, false, "bash-sh"; "local_bash_backed_sh")]
#[test_case(false, true, "bash"; "local_bash_env")]
#[cfg_attr(
    target_os = "macos",
    test_case(false, false, "zsh"; "local_zsh_pipe")
)]
#[cfg_attr(
    target_os = "macos",
    test_case(false, true, "zsh"; "local_zshenv")
)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// Serialize tests that launch a real exec-server process through the full CLI.
#[serial_test::serial(exec_server_process)]
async fn shell_snapshot_v2_filters_profile_exports_and_stays_in_memory(
    tty: bool,
    automatic_startup: bool,
    shell_name: &str,
) -> Result<()> {
    let context = create_process_context().await?;
    let home = TempDir::new()?;
    let cwd = PathUri::from_host_native_path(home.path())?;
    let (shell_path, profile_name) = match shell_name {
        "bash" if automatic_startup => ("/bin/bash", ".bash-env"),
        "bash" => ("/bin/bash", ".bashrc"),
        "sh" => ("/bin/sh", ".snapshot-env"),
        "bash-sh" => ("/bin/bash", ".snapshot-env"),
        "zsh" if automatic_startup => ("/bin/zsh", ".zshenv"),
        "zsh" => ("/bin/zsh", ".zshrc"),
        name => anyhow::bail!("unsupported test shell {name}"),
    };
    let profile_path = home.path().join(profile_name);
    let profile_path_entry = home.path().join("profile-bin");
    let runtime_path_entry = home.path().join("runtime-bin");
    std::fs::create_dir(&profile_path_entry)?;
    let wc = profile_path_entry.join("wc");
    codex_utils_cargo_bin::write_executable(
        &wc,
        "#!/bin/sh\nprintf x >> \"$HOME/tool-captures\"\nexec /usr/bin/wc \"$@\"\n",
    )?;
    let posix_shell = matches!(shell_name, "sh" | "bash-sh");
    let padding = if !tty && shell_name == "bash" {
        format!(
            "snapshot_padding() {{ printf '%s' '{}'; }}\n",
            "🦀".repeat(200_000)
        )
    } else {
        String::new()
    };
    let shadowed_builtins = if posix_shell {
        ""
    } else {
        "unset() { exit 41; }\nbuiltin() { exit 41; }\nexec() { exit 41; }\n"
    };
    std::fs::write(
        &profile_path,
        format!(
            "printf x >> \"$HOME/captures\"\nexport PATH=\"$HOME/profile-bin:/usr/bin:/bin\"\nexport PROFILE_ALLOWED=profile\nexport PROFILE_SECRET=secret\nexport PROFILE_DENIED=denied\nprofile_helper() {{ printf helper; }}\nif [ -n \"${{BASH_VERSION-}}\" ]; then\n  shopt -s extglob nocasematch\n  eval 'profile_helper() {{ case $1 in @(foo|bar)*) printf helper ;; *) return 1 ;; esac; }}'\nfi\nset -u\n{shadowed_builtins}{padding}"
        ),
    )?;
    if shell_name == "zsh" && automatic_startup {
        std::fs::write(
            home.path().join(".zshrc"),
            "export PATH=\"$HOME/profile-bin:/usr/bin:/bin\"\n",
        )?;
    }
    let mut configured_environment = HashMap::from([(
        "HOME".to_string(),
        home.path().to_string_lossy().into_owned(),
    )]);
    if posix_shell {
        configured_environment.insert(
            "ENV".to_string(),
            "${XDG_CONFIG_HOME:-$HOME}/.snapshot-env".to_string(),
        );
        // Keep coverage for large values alongside the many-small-entry case below.
        for index in 0..3 {
            configured_environment.insert(format!("PROFILE_SDK_{index}"), "x".repeat(60 * 1024));
        }
    }
    if shell_name == "bash" && automatic_startup {
        configured_environment.insert(
            "BASH_ENV".to_string(),
            profile_path.to_string_lossy().into_owned(),
        );
    }
    // Many small entries exercise capture overhead separately from the byte limit above.
    let many_entries = !tty && !automatic_startup;
    if many_entries {
        configured_environment.extend(
            (0..1_000).map(|index| (format!("PROFILE_ENTRY_{index}"), format!("value-{index}"))),
        );
    }
    let policy = ExecEnvPolicy {
        inherit: ShellEnvironmentPolicyInherit::All,
        ignore_default_excludes: false,
        exclude: vec!["PROFILE_DENIED".to_string()],
        r#set: configured_environment,
        include_only: vec![
            "BASH_ENV".to_string(),
            "ENV".to_string(),
            "HOME".to_string(),
            "PATH".to_string(),
            "PROFILE_*".to_string(),
        ],
    };
    let (command_prefix, expected_prefix) = if shell_name == "sh" {
        ("", "")
    } else {
        ("profile_helper FOObar; ", "helper")
    };
    let entry_check = if many_entries {
        "[ \"${PROFILE_ENTRY_999-missing}\" = value-999 ] || exit 43; "
    } else {
        ""
    };
    let command = format!(
        "case $- in *u*) ;; *) exit 42 ;; esac; {entry_check}export PATH='{}':\"$PATH\"; {command_prefix}printf '|%s|%s|%s|%s|%s|%s' \"$PROFILE_ALLOWED\" \"${{PROFILE_SECRET-missing}}\" \"${{PROFILE_DENIED-missing}}\" \"$PATH\" \"${{__CODEX_SHELL_SNAPSHOT_STATE_0-missing}}\" \"${{__CODEX_SHELL_SNAPSHOT_STATE_1-missing}}\"",
        runtime_path_entry.display(),
    );
    let expected_stdout = format!(
        "{expected_prefix}|profile|missing|missing|{}:{}:/usr/bin:/bin|missing|missing",
        runtime_path_entry.display(),
        profile_path_entry.display(),
    );

    for attempt in 0..2 {
        let started = context
            .backend
            .start(ExecParams {
                metadata: Default::default(),
                process_id: ProcessId::from(format!("snapshot-{attempt}")),
                argv: vec![shell_path.to_string(), "-lc".to_string(), command.clone()],
                cwd: cwd.clone(),
                env_policy: Some(policy.clone()),
                shell_snapshot: Some(ShellSnapshotRequest {
                    scope_id: "attachment-1".to_string(),
                    shell: ShellInfo {
                        name: if posix_shell { "sh" } else { shell_name }.to_string(),
                        path: shell_path.to_string(),
                    },
                }),
                env: HashMap::new(),
                tty,
                pipe_stdin: false,
                arg0: (shell_name == "bash-sh").then(|| "sh".to_string()),
                sandbox: None,
                enforce_managed_network: false,
                managed_network: None,
                network_proxy: None,
            })
            .await?;
        let (stdout, stderr, status, closed) =
            collect_process_output_from_events(started.process).await?;
        assert_eq!(
            (stdout, stderr, status, closed),
            (expected_stdout.clone(), String::new(), Some(0), true,)
        );
    }

    assert_eq!(std::fs::read_to_string(home.path().join("captures"))?, "x");
    assert!(!std::fs::read(home.path().join("tool-captures"))?.is_empty());
    Ok(())
}

#[cfg(unix)]
#[test_case(false; "local_pipe")]
#[test_case(true; "local_tty")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(exec_server_process)]
async fn exec_metadata_overrides_policy_and_cached_snapshot(tty: bool) -> Result<()> {
    let context = create_process_context().await?;
    let home = TempDir::new()?;
    let cwd = PathUri::from_host_native_path(home.path())?;
    std::fs::write(
        home.path().join(".bashrc"),
        "printf '%s|%s\n' \"${CODEX_THREAD_ID-unset}\" \"${CODEX_TOOL_CALL_ID-unset}\" >> \"$HOME/capture-env\"\nexport CODEX_THREAD_ID=snapshot-thread\nexport CODEX_TOOL_CALL_ID=snapshot-call\n",
    )?;
    let policy = ExecEnvPolicy {
        inherit: ShellEnvironmentPolicyInherit::None,
        ignore_default_excludes: true,
        exclude: Vec::new(),
        r#set: HashMap::from([
            ("HOME".into(), home.path().to_string_lossy().into_owned()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("CODEX_THREAD_ID".into(), "policy-thread".into()),
            ("CODEX_TOOL_CALL_ID".into(), "policy-call".into()),
        ]),
        include_only: Vec::new(),
    };
    let thread_id = codex_protocol::ThreadId::from_string("019d25f0-6728-7ce2-908f-c3c187323e5b")?;
    let later_thread_id =
        codex_protocol::ThreadId::from_string("019d25f0-6728-7ce2-908f-c3c187323e5c")?;

    for (attempt, call_id, expected) in [
        (0, Some(""), ""),
        (1, Some("call-λ-'\";$()"), "call-λ-'\";$()"),
        (2, None, "unset"),
        (3, Some("exec-invalid\0call"), "unset"),
    ] {
        let execution_thread_id = if attempt == 1 {
            later_thread_id
        } else {
            thread_id
        };
        let started = context
            .backend
            .start(ExecParams {
                metadata: Some(ExecMetadata {
                    thread_id: Some(execution_thread_id),
                    tool_call_id: call_id.map(str::to_string),
                }),
                process_id: ProcessId::from(format!("metadata-snapshot-{attempt}")),
                argv: vec![
                    "/bin/bash".into(),
                    "-lc".into(),
                    "printf '%s|%s' \"$CODEX_THREAD_ID\" \"${CODEX_TOOL_CALL_ID-unset}\""
                        .to_string(),
                ],
                cwd: cwd.clone(),
                env_policy: Some(policy.clone()),
                shell_snapshot: Some(ShellSnapshotRequest {
                    scope_id: "metadata-attachment".into(),
                    shell: ShellInfo {
                        name: "bash".into(),
                        path: "/bin/bash".into(),
                    },
                }),
                env: HashMap::from([
                    ("CODEX_THREAD_ID".into(), "overlay-thread".into()),
                    ("CODEX_TOOL_CALL_ID".into(), "overlay-call".into()),
                ]),
                tty,
                pipe_stdin: false,
                arg0: None,
                sandbox: None,
                enforce_managed_network: false,
                managed_network: None,
                network_proxy: None,
            })
            .await?;
        let (stdout, stderr, status, closed) =
            collect_process_output_from_events(started.process).await?;
        assert_eq!(
            (stdout, stderr, status, closed),
            (
                format!("{execution_thread_id}|{expected}"),
                String::new(),
                Some(0),
                true,
            )
        );
    }

    assert_eq!(
        std::fs::read_to_string(home.path().join("capture-env"))?,
        "019d25f0-6728-7ce2-908f-c3c187323e5b|unset\n"
    );
    Ok(())
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum CaptureFailure {
    Exit(i32),
    Cancel,
    Timeout,
}

#[cfg(unix)]
#[test_case(false, "bash", 1, CaptureFailure::Exit(7); "local_pipe_recovery")]
#[test_case(true, "bash", 1, CaptureFailure::Exit(7); "local_tty_recovery")]
#[test_case(false, "bash", 3, CaptureFailure::Exit(7); "local_retry_budget_exhausted")]
#[test_case(false, "bash", 1, CaptureFailure::Exit(0); "local_invalid_output_recovery")]
#[cfg_attr(target_os = "macos", test_case(false, "zsh", 1, CaptureFailure::Exit(0); "local_zsh_invalid_output_recovery"))]
#[cfg_attr(target_os = "macos", test_case(false, "zsh", 1, CaptureFailure::Exit(7); "local_zsh_recovery"))]
#[test_case(false, "bash", 1, CaptureFailure::Cancel; "cancel_capture")]
#[test_case(false, "bash", 1, CaptureFailure::Timeout; "timeout_capture")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(exec_server_process)]
async fn shell_snapshot_v2_capture_failure_falls_back_and_retries(
    tty: bool,
    shell_name: &str,
    failures_before_repair: usize,
    failure: CaptureFailure,
) -> Result<()> {
    let context = create_process_context().await?;
    let home = TempDir::new()?;
    let cwd = PathUri::from_host_native_path(home.path())?;
    let (shell_path, profile_name) = match shell_name {
        "bash" => ("/bin/bash", ".bashrc"),
        "zsh" => ("/bin/zsh", ".zshrc"),
        name => anyhow::bail!("unsupported test shell {name}"),
    };
    let child_pids = home.path().join("startup-pids");
    let _cleanup = shell_snapshot::StartupProcesses(child_pids.clone());
    let startup_child =
        "/bin/sleep 30 >/dev/null 2>&1 &\nprintf '%s\\n' \"$!\" >> \"$HOME/startup-pids\"\n";
    let finish = match failure {
        CaptureFailure::Exit(code) => format!("exit {code}"),
        CaptureFailure::Cancel | CaptureFailure::Timeout => "wait".to_string(),
    };
    std::fs::write(
        home.path().join(profile_name),
        format!("printf x >> \"$HOME/captures\"\n{startup_child}{finish}\n"),
    )?;
    let policy = ExecEnvPolicy {
        inherit: ShellEnvironmentPolicyInherit::All,
        ignore_default_excludes: false,
        exclude: Vec::new(),
        r#set: HashMap::from([(
            "HOME".to_string(),
            home.path().to_string_lossy().into_owned(),
        )]),
        include_only: vec!["HOME".to_string(), "PATH".to_string()],
    };
    let mut params = ExecParams {
        metadata: Default::default(),
        process_id: ProcessId::from("snapshot-first"),
        argv: vec![
            shell_path.to_string(),
            "-lc".to_string(),
            "if command -v profile_helper >/dev/null; then profile_helper; else printf original; fi".to_string(),
        ],
        cwd: cwd.clone(),
        env_policy: Some(policy),
        shell_snapshot: Some(ShellSnapshotRequest {
            scope_id: "attachment-1".to_string(),
            shell: ShellInfo {
                name: shell_name.to_string(),
                path: shell_path.to_string(),
            },
        }),
        env: HashMap::new(),
        tty,
        pipe_stdin: false,
        arg0: None,
        sandbox: None,
        enforce_managed_network: false,
        managed_network: None,
        network_proxy: None,
    };

    for attempt in 0..failures_before_repair {
        params.process_id = ProcessId::from(format!("snapshot-fallback-{attempt}"));
        let mut start = context.backend.start(params.clone());
        if matches!(failure, CaptureFailure::Cancel) {
            tokio::select! {
                result = &mut start => anyhow::bail!("capture finished before cancellation: {}", result.is_ok()),
                _ = async {
                    while std::fs::read_to_string(&child_pids).unwrap_or_default().trim().is_empty() {
                        sleep(Duration::from_millis(20)).await;
                    }
                } => {}
            }
            drop(start);
        } else {
            let fallback = start.await?;
            assert_eq!(
                collect_process_output_from_events(fallback.process).await?,
                ("original".to_string(), String::new(), Some(0), true)
            );
        }
        {
            let pids = std::fs::read_to_string(&child_pids)?;
            let pid = pids
                .split_whitespace()
                .last()
                .context("startup child PID")?;
            timeout(Duration::from_secs(5), async {
                loop {
                    let output = std::process::Command::new("/bin/ps")
                        .args(["-o", "stat=", "-p", pid])
                        .output()?;
                    let state = String::from_utf8(output.stdout)?;
                    if state.trim().is_empty() || state.trim().starts_with('Z') {
                        // Forget the completed child before retries can reuse its PID.
                        std::fs::remove_file(&child_pids)?;
                        return Ok::<_, anyhow::Error>(());
                    }
                    sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .context("failed capture left its startup child running")??;
        }
        sleep(Duration::from_millis(1100)).await;
    }
    assert_eq!(
        std::fs::read_to_string(home.path().join("captures"))?,
        "x".repeat(failures_before_repair)
    );

    std::fs::write(
        home.path().join(profile_name),
        "printf x >> \"$HOME/captures\"\nprofile_helper() { printf recovered; }\n",
    )?;
    let (expected_output, expected_captures) = if failures_before_repair == 3 {
        ("original", "xxx")
    } else {
        ("recovered", "xx")
    };
    for attempt in 0..2 {
        params.process_id = ProcessId::from(format!("snapshot-after-repair-{attempt}"));
        let started = context.backend.start(params.clone()).await?;
        assert_eq!(
            collect_process_output_from_events(started.process).await?,
            (expected_output.to_string(), String::new(), Some(0), true)
        );
    }
    assert_eq!(
        std::fs::read_to_string(home.path().join("captures"))?,
        expected_captures
    );
    Ok(())
}

async fn assert_exec_process_starts_and_exits() -> Result<()> {
    let context = create_process_context().await?;
    let thread_id = codex_protocol::ThreadId::from_string("019d25f0-6728-7ce2-908f-c3c187323e5b")?;
    let call_id = "call-λ-'\";$()";
    #[cfg(unix)]
    let argv = vec![
        "/bin/sh".into(),
        "-c".into(),
        "printf '%s|%s|%s' \"$CODEX_THREAD_ID\" \"$CODEX_TOOL_CALL_ID\" \"$CUSTOM_ENV_VAR\""
            .to_string(),
    ];
    let mut env = std::env::vars().collect::<HashMap<_, _>>();
    env.extend([
        ("CODEX_THREAD_ID".into(), "stale-thread".into()),
        ("Codex_Thread_Id".into(), "mixed-case-thread".into()),
        ("CODEX_TOOL_CALL_ID".into(), "stale-call".into()),
        ("Codex_Tool_Call_Id".into(), "mixed-case-call".into()),
        ("CUSTOM_ENV_VAR".into(), "caller-owned".into()),
    ]);
    let session = context
        .backend
        .start(ExecParams {
            metadata: Some(ExecMetadata {
                thread_id: Some(thread_id),
                tool_call_id: Some(call_id.to_string()),
            }),
            process_id: ProcessId::from("proc-1"),
            argv,
            cwd: PathUri::from_host_native_path(std::env::current_dir()?)?,
            shell_snapshot: None,
            env_policy: /*env_policy*/ None,
            env,
            tty: false,
            pipe_stdin: false,
            arg0: None,
            sandbox: None,
            enforce_managed_network: false,
            managed_network: None,
            network_proxy: None,
        })
        .await?;
    assert_eq!(session.process.process_id().as_str(), "proc-1");
    let wake_rx = session.process.subscribe_wake();
    let (stdout, stderr, status, closed) =
        collect_process_output_from_events(Arc::clone(&session.process)).await?;
    assert_eq!(
        (stdout, stderr, status, closed),
        (
            format!("{thread_id}|{call_id}|caller-owned"),
            String::new(),
            Some(0),
            true,
        )
    );
    let (_, exit_code, closed) =
        collect_process_output_from_reads(session.process, wake_rx).await?;

    assert_eq!(exit_code, Some(0));
    assert!(closed);
    Ok(())
}

async fn read_process_until_change(
    session: Arc<dyn ExecProcess>,
    wake_rx: &mut watch::Receiver<u64>,
    after_seq: Option<u64>,
) -> Result<ReadResponse> {
    let response = session
        .read(after_seq, /*max_bytes*/ None, /*wait_ms*/ Some(0))
        .await?;
    if !response.chunks.is_empty() || response.closed || response.failure.is_some() {
        return Ok(response);
    }

    timeout(Duration::from_secs(2), wake_rx.changed()).await??;
    session
        .read(after_seq, /*max_bytes*/ None, /*wait_ms*/ Some(0))
        .await
        .map_err(Into::into)
}

async fn collect_process_output_from_reads(
    session: Arc<dyn ExecProcess>,
    mut wake_rx: watch::Receiver<u64>,
) -> Result<(String, Option<i32>, bool)> {
    let mut output = String::new();
    let mut exit_code = None;
    let mut after_seq = None;
    loop {
        let response =
            read_process_until_change(Arc::clone(&session), &mut wake_rx, after_seq).await?;
        if let Some(message) = response.failure {
            anyhow::bail!("process failed before closed state: {message}");
        }
        for chunk in response.chunks {
            output.push_str(&String::from_utf8_lossy(&chunk.chunk.into_inner()));
            after_seq = Some(chunk.seq);
        }
        if response.exited {
            exit_code = response.exit_code;
        }
        if response.closed {
            break;
        }
        after_seq = response.next_seq.checked_sub(1).or(after_seq);
    }
    drop(session);
    Ok((output, exit_code, true))
}

async fn collect_process_output_from_events(
    session: Arc<dyn ExecProcess>,
) -> Result<(String, String, Option<i32>, bool)> {
    collect_process_output_from_events_with_timeout(session, Duration::from_secs(2)).await
}

async fn collect_process_output_from_events_with_timeout(
    session: Arc<dyn ExecProcess>,
    event_timeout: Duration,
) -> Result<(String, String, Option<i32>, bool)> {
    let mut events = session.subscribe_events();
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut exit_code = None;
    loop {
        match timeout(event_timeout, events.recv()).await?? {
            ExecProcessEvent::Output(chunk) => match chunk.stream {
                ExecOutputStream::Stdout | ExecOutputStream::Pty => {
                    stdout.push_str(&String::from_utf8_lossy(&chunk.chunk.into_inner()));
                }
                ExecOutputStream::Stderr => {
                    stderr.push_str(&String::from_utf8_lossy(&chunk.chunk.into_inner()));
                }
            },
            ExecProcessEvent::Exited {
                seq: _,
                exit_code: code,
                ..
            } => {
                exit_code = Some(code);
            }
            ExecProcessEvent::Closed { seq: _ } => {
                drop(session);
                return Ok((stdout, stderr, exit_code, true));
            }
            ExecProcessEvent::Failed(message) => {
                anyhow::bail!("process failed before closed state: {message}");
            }
        }
    }
}

async fn collect_process_event_snapshots(
    session: Arc<dyn ExecProcess>,
) -> Result<Vec<ProcessEventSnapshot>> {
    let mut events = session.subscribe_events();
    let mut snapshots = Vec::new();
    loop {
        let snapshot = match timeout(Duration::from_secs(2), events.recv()).await?? {
            ExecProcessEvent::Output(chunk) => ProcessEventSnapshot::Output {
                seq: chunk.seq,
                stream: chunk.stream,
                text: String::from_utf8_lossy(&chunk.chunk.into_inner()).into_owned(),
            },
            ExecProcessEvent::Exited { seq, exit_code, .. } => {
                ProcessEventSnapshot::Exited { seq, exit_code }
            }
            ExecProcessEvent::Closed { seq } => ProcessEventSnapshot::Closed { seq },
            ExecProcessEvent::Failed(message) => {
                anyhow::bail!("process failed before closed state: {message}");
            }
        };
        let closed = matches!(snapshot, ProcessEventSnapshot::Closed { .. });
        snapshots.push(snapshot);
        if closed {
            drop(session);
            return Ok(snapshots);
        }
    }
}

async fn assert_exec_process_streams_output() -> Result<()> {
    let context = create_process_context().await?;
    let process_id = "proc-stream".to_string();
    let session = context
        .backend
        .start(ExecParams {
            metadata: Default::default(),
            process_id: process_id.clone().into(),
            argv: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "sleep 0.05; printf 'session output\\n'".to_string(),
            ],
            cwd: PathUri::from_host_native_path(std::env::current_dir()?)?,
            shell_snapshot: None,
            env_policy: /*env_policy*/ None,
            env: Default::default(),
            tty: false,
            pipe_stdin: false,
            arg0: None,
            sandbox: None,
            enforce_managed_network: false,
            managed_network: None,
            network_proxy: None,
        })
        .await?;
    assert_eq!(session.process.process_id().as_str(), process_id);

    let StartedExecProcess { process, .. } = session;
    let wake_rx = process.subscribe_wake();
    let (output, exit_code, closed) = collect_process_output_from_reads(process, wake_rx).await?;
    assert_eq!(output, "session output\n");
    assert_eq!(exit_code, Some(0));
    assert!(closed);
    Ok(())
}

async fn assert_exec_process_pushes_events() -> Result<()> {
    let context = create_process_context().await?;
    let process_id = "proc-events".to_string();
    let session = context
        .backend
        .start(ExecParams {
            metadata: Default::default(),
            process_id: process_id.clone().into(),
            argv: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "printf 'event output\\n'; sleep 0.1; printf 'event err\\n' >&2; sleep 0.1; exit 7".to_string(),
            ],
            cwd: PathUri::from_host_native_path(std::env::current_dir()?)?,
            shell_snapshot: None,
            env_policy: /*env_policy*/ None,
            env: Default::default(),
            tty: false,
            pipe_stdin: false,
            arg0: None,
            sandbox: None,
            enforce_managed_network: false,
            managed_network: None,
            network_proxy: None,
        })
        .await?;
    assert_eq!(session.process.process_id().as_str(), process_id);

    let StartedExecProcess { process, .. } = session;
    let actual = collect_process_event_snapshots(process).await?;
    assert_eq!(
        actual,
        vec![
            ProcessEventSnapshot::Output {
                seq: 1,
                stream: ExecOutputStream::Stdout,
                text: "event output\n".to_string(),
            },
            ProcessEventSnapshot::Output {
                seq: 2,
                stream: ExecOutputStream::Stderr,
                text: "event err\n".to_string(),
            },
            ProcessEventSnapshot::Exited {
                seq: 3,
                exit_code: 7,
            },
            ProcessEventSnapshot::Closed { seq: 4 },
        ]
    );
    Ok(())
}

async fn assert_exec_process_replays_events_after_close() -> Result<()> {
    let context = create_process_context().await?;
    let process_id = "proc-events-late".to_string();
    let session = context
        .backend
        .start(ExecParams {
            metadata: Default::default(),
            process_id: process_id.clone().into(),
            argv: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "printf 'late one\\n'; printf 'late two\\n'".to_string(),
            ],
            cwd: PathUri::from_host_native_path(std::env::current_dir()?)?,
            shell_snapshot: None,
            env_policy: /*env_policy*/ None,
            env: Default::default(),
            tty: false,
            pipe_stdin: false,
            arg0: None,
            sandbox: None,
            enforce_managed_network: false,
            managed_network: None,
            network_proxy: None,
        })
        .await?;
    assert_eq!(session.process.process_id().as_str(), process_id);

    let StartedExecProcess { process, .. } = session;
    let wake_rx = process.subscribe_wake();
    let read_result = collect_process_output_from_reads(Arc::clone(&process), wake_rx).await?;
    assert_eq!(
        read_result,
        ("late one\nlate two\n".to_string(), Some(0), true)
    );

    let event_result = collect_process_output_from_events(process).await?;
    assert_eq!(
        event_result,
        (
            "late one\nlate two\n".to_string(),
            String::new(),
            Some(0),
            true
        )
    );
    Ok(())
}

async fn assert_exec_process_retains_output_after_exit_until_streams_close() -> Result<()> {
    let context = create_process_context().await?;
    let (helper_binary, _) = current_test_binary_helper_paths()?;
    let release_dir = TempDir::new()?;
    let release_path = release_dir.path().join("release-delayed-output");
    let process_id = "proc-output-after-exit".to_string();
    let session = context
        .backend
        .start(ExecParams {
            metadata: Default::default(),
            process_id: process_id.clone().into(),
            argv: vec![
                helper_binary.to_string_lossy().into_owned(),
                DELAYED_OUTPUT_AFTER_EXIT_PARENT_ARG.to_string(),
                release_path.to_string_lossy().into_owned(),
            ],
            cwd: PathUri::from_host_native_path(std::env::current_dir()?)?,
            shell_snapshot: None,
            env_policy: /*env_policy*/ None,
            env: Default::default(),
            tty: false,
            pipe_stdin: false,
            arg0: None,
            sandbox: None,
            enforce_managed_network: false,
            managed_network: None,
            network_proxy: None,
        })
        .await?;
    assert_eq!(session.process.process_id().as_str(), process_id);

    let StartedExecProcess { process, .. } = session;

    let exit_response = timeout(
        Duration::from_secs(2),
        process.read(
            /*after_seq*/ None,
            /*max_bytes*/ None,
            /*wait_ms*/ Some(2_000),
        ),
    )
    .await??;
    assert!(
        exit_response.chunks.is_empty(),
        "parent should exit before child writes delayed output"
    );
    assert_eq!(exit_response.exit_code, Some(0));
    assert!(!exit_response.closed);
    let exit_seq = exit_response
        .next_seq
        .checked_sub(1)
        .context("exit response should advance next_seq")?;
    std::fs::write(&release_path, b"go")?;

    let late_response = timeout(
        Duration::from_secs(2),
        process.read(
            /*after_seq*/ Some(exit_seq),
            /*max_bytes*/ None,
            /*wait_ms*/ Some(2_000),
        ),
    )
    .await??;
    let mut late_output = String::new();
    for chunk in late_response.chunks {
        assert_eq!(chunk.stream, ExecOutputStream::Stdout);
        late_output.push_str(&String::from_utf8_lossy(&chunk.chunk.into_inner()));
    }
    assert_eq!(late_output, "late output after exit\n");

    let wake_rx = process.subscribe_wake();
    let actual = collect_process_output_from_reads(process, wake_rx).await?;
    assert_eq!(
        actual,
        ("late output after exit\n".to_string(), Some(0), true)
    );
    Ok(())
}

async fn assert_exec_process_write_then_read() -> Result<()> {
    let context = create_process_context().await?;
    let process_id = "proc-stdin".to_string();
    let session = context
        .backend
        .start(ExecParams {
            metadata: Default::default(),
            process_id: process_id.clone().into(),
            argv: vec![
                // Use `/bin/sh` instead of Python so this stdin round-trip test
                // stays portable across Bazel and non-macOS runners where
                // `/usr/bin/python3` is not guaranteed to exist.
                "/bin/sh".to_string(),
                "-c".to_string(),
                "IFS= read line; printf 'from-stdin:%s\\n' \"$line\"".to_string(),
            ],
            cwd: PathUri::from_host_native_path(std::env::current_dir()?)?,
            shell_snapshot: None,
            env_policy: /*env_policy*/ None,
            env: Default::default(),
            tty: true,
            pipe_stdin: false,
            arg0: None,
            sandbox: None,
            enforce_managed_network: false,
            managed_network: None,
            network_proxy: None,
        })
        .await?;
    assert_eq!(session.process.process_id().as_str(), process_id);

    tokio::time::sleep(Duration::from_millis(200)).await;
    session.process.write(b"hello\n".to_vec()).await?;
    let StartedExecProcess { process, .. } = session;
    let wake_rx = process.subscribe_wake();
    let (output, exit_code, closed) = collect_process_output_from_reads(process, wake_rx).await?;

    assert!(
        output.contains("from-stdin:hello"),
        "unexpected output: {output:?}"
    );
    assert_eq!(exit_code, Some(0));
    assert!(closed);
    Ok(())
}

async fn assert_exec_process_write_then_read_without_tty() -> Result<()> {
    let context = create_process_context().await?;
    let process_id = "proc-stdin-pipe".to_string();
    let session = context
        .backend
        .start(ExecParams {
            metadata: Default::default(),
            process_id: process_id.clone().into(),
            argv: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "IFS= read line; printf 'from-stdin:%s\\n' \"$line\"".to_string(),
            ],
            cwd: PathUri::from_host_native_path(std::env::current_dir()?)?,
            shell_snapshot: None,
            env_policy: /*env_policy*/ None,
            env: Default::default(),
            tty: false,
            pipe_stdin: true,
            arg0: None,
            sandbox: None,
            enforce_managed_network: false,
            managed_network: None,
            network_proxy: None,
        })
        .await?;
    assert_eq!(session.process.process_id().as_str(), process_id);

    tokio::time::sleep(Duration::from_millis(200)).await;
    let write_response = session.process.write(b"hello\n".to_vec()).await?;
    assert_eq!(write_response.status, WriteStatus::Accepted);
    let StartedExecProcess { process, .. } = session;
    let wake_rx = process.subscribe_wake();
    let actual = collect_process_output_from_reads(process, wake_rx).await?;

    assert_eq!(actual, ("from-stdin:hello\n".to_string(), Some(0), true));
    Ok(())
}

async fn assert_exec_process_rejects_write_without_pipe_stdin() -> Result<()> {
    let context = create_process_context().await?;
    let process_id = "proc-stdin-closed".to_string();
    let session = context
        .backend
        .start(ExecParams {
            metadata: Default::default(),
            process_id: process_id.clone().into(),
            argv: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "sleep 0.3; if IFS= read -r line; then printf 'read:%s\\n' \"$line\"; else printf 'eof\\n'; fi".to_string(),
            ],
            cwd: PathUri::from_host_native_path(std::env::current_dir()?)?,
            shell_snapshot: None,
            env_policy: /*env_policy*/ None,
            env: Default::default(),
            tty: false,
            pipe_stdin: false,
            arg0: None,
            sandbox: None,
            enforce_managed_network: false,
            managed_network: None,
            network_proxy: None,
        })
        .await?;
    assert_eq!(session.process.process_id().as_str(), process_id);

    let write_response = session.process.write(b"ignored\n".to_vec()).await?;
    assert_eq!(write_response.status, WriteStatus::StdinClosed);
    let StartedExecProcess { process, .. } = session;
    let wake_rx = process.subscribe_wake();
    let (output, exit_code, closed) = collect_process_output_from_reads(process, wake_rx).await?;

    assert_eq!(output, "eof\n");
    assert_eq!(exit_code, Some(0));
    assert!(closed);
    Ok(())
}

async fn assert_exec_process_signal_interrupts_process() -> Result<()> {
    let context = create_process_context().await?;
    let process_id = "proc-signal".to_string();
    let session = context
        .backend
        .start(ExecParams {
            metadata: Default::default(),
            process_id: process_id.clone().into(),
            argv: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "trap 'printf \"signal:2\\n\"; exit 7' INT; printf 'ready\\n'; while :; do :; done".to_string(),
            ],
            cwd: PathUri::from_host_native_path(std::env::current_dir()?)?,
            shell_snapshot: None,
            env_policy: /*env_policy*/ None,
            env: Default::default(),
            tty: false,
            pipe_stdin: false,
            arg0: None,
            sandbox: None,
            enforce_managed_network: false,
            managed_network: None,
            network_proxy: None,
        })
        .await?;
    assert_eq!(session.process.process_id().as_str(), process_id);

    let StartedExecProcess { process, .. } = session;
    let mut wake_rx = process.subscribe_wake();
    let mut ready_output = String::new();
    let mut after_seq = None;
    loop {
        let response =
            read_process_until_change(Arc::clone(&process), &mut wake_rx, after_seq).await?;
        for chunk in response.chunks {
            ready_output.push_str(&String::from_utf8_lossy(&chunk.chunk.into_inner()));
            after_seq = Some(chunk.seq);
        }
        if ready_output.contains("ready\n") {
            break;
        }
        if response.closed {
            anyhow::bail!("process closed before readiness marker: {ready_output:?}");
        }
        after_seq = response.next_seq.checked_sub(1).or(after_seq);
    }

    process.signal(ProcessSignal::Interrupt).await?;
    let (output, exit_code, closed) = collect_process_output_from_reads(process, wake_rx).await?;

    assert!(
        output.contains("signal:2"),
        "expected signal handler output, got {output:?}"
    );
    assert_eq!(exit_code, Some(7));
    assert!(closed);
    Ok(())
}

async fn assert_exec_process_preserves_queued_events_before_subscribe() -> Result<()> {
    let context = create_process_context().await?;
    let session = context
        .backend
        .start(ExecParams {
            metadata: Default::default(),
            process_id: ProcessId::from("proc-queued"),
            argv: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "printf 'queued output\\n'".to_string(),
            ],
            cwd: PathUri::from_host_native_path(std::env::current_dir()?)?,
            shell_snapshot: None,
            env_policy: /*env_policy*/ None,
            env: Default::default(),
            tty: false,
            pipe_stdin: false,
            arg0: None,
            sandbox: None,
            enforce_managed_network: false,
            managed_network: None,
            network_proxy: None,
        })
        .await?;

    tokio::time::sleep(Duration::from_millis(200)).await;

    let StartedExecProcess { process, .. } = session;
    let wake_rx = process.subscribe_wake();
    let (output, exit_code, closed) = collect_process_output_from_reads(process, wake_rx).await?;
    assert_eq!(output, "queued output\n");
    assert_eq!(exit_code, Some(0));
    assert!(closed);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// Serialize tests that launch a real exec-server process through the full CLI.
#[serial_test::serial(exec_server_process)]
async fn exec_process_starts_and_exits() -> Result<()> {
    assert_exec_process_starts_and_exits().await
}

#[cfg_attr(not(unix), ignore = "Unix-only exec-server process test")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// Serialize tests that launch a real exec-server process through the full CLI.
#[serial_test::serial(exec_server_process)]
async fn exec_process_streams_output() -> Result<()> {
    assert_exec_process_streams_output().await
}

#[cfg_attr(not(unix), ignore = "Unix-only exec-server process test")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// Serialize tests that launch a real exec-server process through the full CLI.
#[serial_test::serial(exec_server_process)]
async fn exec_process_pushes_events() -> Result<()> {
    assert_exec_process_pushes_events().await
}

#[cfg_attr(not(unix), ignore = "Unix-only exec-server process test")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// Serialize tests that launch a real exec-server process through the full CLI.
#[serial_test::serial(exec_server_process)]
async fn exec_process_replays_events_after_close() -> Result<()> {
    assert_exec_process_replays_events_after_close().await
}

#[cfg_attr(not(unix), ignore = "Unix-only exec-server process test")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// Serialize tests that launch a real exec-server process through the full CLI.
#[serial_test::serial(exec_server_process)]
async fn exec_process_retains_output_after_exit_until_streams_close() -> Result<()> {
    assert_exec_process_retains_output_after_exit_until_streams_close().await
}

#[cfg_attr(not(unix), ignore = "Unix-only exec-server process test")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// Serialize tests that launch a real exec-server process through the full CLI.
#[serial_test::serial(exec_server_process)]
async fn exec_process_write_then_read() -> Result<()> {
    assert_exec_process_write_then_read().await
}

#[cfg_attr(not(unix), ignore = "Unix-only exec-server process test")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// Serialize tests that launch a real exec-server process through the full CLI.
#[serial_test::serial(exec_server_process)]
async fn exec_process_write_then_read_without_tty() -> Result<()> {
    assert_exec_process_write_then_read_without_tty().await
}

#[cfg_attr(not(unix), ignore = "Unix-only exec-server process test")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// Serialize tests that launch a real exec-server process through the full CLI.
#[serial_test::serial(exec_server_process)]
async fn exec_process_rejects_write_without_pipe_stdin() -> Result<()> {
    assert_exec_process_rejects_write_without_pipe_stdin().await
}

#[cfg_attr(not(unix), ignore = "Unix-only exec-server process test")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// Serialize tests that launch a real exec-server process through the full CLI.
#[serial_test::serial(exec_server_process)]
async fn exec_process_signal_interrupts_process() -> Result<()> {
    assert_exec_process_signal_interrupts_process().await
}

#[cfg_attr(not(unix), ignore = "Unix-only exec-server process test")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// Serialize tests that launch a real exec-server process through the full CLI.
#[serial_test::serial(exec_server_process)]
async fn exec_process_preserves_queued_events_before_subscribe() -> Result<()> {
    assert_exec_process_preserves_queued_events_before_subscribe().await
}
