//! Pipe I/O and process-tree ownership on top of the shared local child launcher.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::AtomicBool;

use crate::ChildStdin;
use crate::Command;
#[cfg(unix)]
use crate::DescriptorPolicy;
#[cfg(unix)]
use crate::ProcessMode;
use crate::child_command::ChildDropPolicy;
use anyhow::Result;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::process::ChildTerminator;
use crate::process::ProcessHandle;
use crate::process::ProcessSignal;
use crate::process::SpawnedProcess;
use crate::process::exit_code_from_status;

struct PipeChildTerminator {
    #[cfg(unix)]
    process_group_id: u32,
}

impl ChildTerminator for PipeChildTerminator {
    fn signal(&mut self, signal: ProcessSignal) -> io::Result<()> {
        match signal {
            ProcessSignal::Interrupt => {
                #[cfg(unix)]
                {
                    crate::process_group::interrupt_process_group(self.process_group_id)
                }
            }
        }
    }

    fn kill(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        {
            crate::process_group::kill_process_group(self.process_group_id)
        }
    }
}


async fn read_output_stream<R>(mut reader: R, output_tx: mpsc::Sender<Vec<u8>>)
where
    R: AsyncRead + Unpin,
{
    let mut buf = vec![0u8; 8_192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let _ = output_tx.send(buf[..n].to_vec()).await;
            }
            Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

#[derive(Clone, Copy)]
enum PipeStdinMode {
    Piped,
    Null,
}

async fn spawn_process_with_stdin_mode(
    program: &OsStr,
    args: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
    arg0: &Option<String>,
    stdin_mode: PipeStdinMode,
    inherited_fds: &[i32],
) -> Result<SpawnedProcess> {
    if program.is_empty() {
        anyhow::bail!("missing program for pipe spawn");
    }

    let mut command = Command::new(program);
    #[cfg(unix)]
    if let Some(arg0) = arg0 {
        command.arg0(arg0);
    }
    #[cfg(unix)]
    command
        .process_mode(ProcessMode::NewSession)
        .descriptor_policy(DescriptorPolicy::Explicit)
        .inherit_fds(inherited_fds);
    #[cfg(target_os = "linux")]
    command.terminate_on_parent_death();
    command
        .current_dir(cwd)
        .envs(env)
        .args(args)
        .drop_policy(ChildDropPolicy::ReapOnly)
        .stdin(match stdin_mode {
            PipeStdinMode::Piped => ChildStdin::Piped,
            PipeStdinMode::Null => ChildStdin::Null,
        });

    #[cfg(target_os = "linux")]
    let mut child =
        match crate::spawn_helper::spawn(&command, crate::spawn_helper::Setup::Pipe).await? {
            Some(child) => child,
            None => command.spawn()?,
        };
    #[cfg(not(target_os = "linux"))]
    let mut child = command.spawn()?;
    #[cfg(unix)]
    let process_group_id = child
        .id()
        .ok_or_else(|| io::Error::other("missing child pid"))?;

    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let (writer_tx, mut writer_rx) = mpsc::channel::<Vec<u8>>(128);
    let (stdout_tx, stdout_rx) = mpsc::channel::<Vec<u8>>(128);
    let (stderr_tx, stderr_rx) = mpsc::channel::<Vec<u8>>(128);
    let writer_handle = if let Some(stdin) = stdin {
        tokio::spawn(async move {
            let mut writer = stdin;
            while let Some(bytes) = writer_rx.recv().await {
                let _ = writer.write_all(&bytes).await;
                let _ = writer.flush().await;
            }
        })
    } else {
        drop(writer_rx);
        tokio::spawn(async {})
    };

    let stdout_handle = stdout.map(|stdout| {
        let stdout_tx = stdout_tx.clone();
        tokio::spawn(async move {
            read_output_stream(BufReader::new(stdout), stdout_tx).await;
        })
    });
    let stderr_handle = stderr.map(|stderr| {
        let stderr_tx = stderr_tx.clone();
        tokio::spawn(async move {
            read_output_stream(BufReader::new(stderr), stderr_tx).await;
        })
    });

    let (exit_tx, exit_rx) = oneshot::channel::<i32>();
    let exit_status = Arc::new(AtomicBool::new(false));
    let wait_exit_status = Arc::clone(&exit_status);
    let exit_code = Arc::new(StdMutex::new(None));
    let wait_exit_code = Arc::clone(&exit_code);
    let wait_handle: JoinHandle<()> = tokio::spawn(async move {
        let code = match child.wait().await {
            Ok(status) => {
                exit_code_from_status(status)
            }
            Err(_) => -1,
        };
        wait_exit_status.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut guard) = wait_exit_code.lock() {
            *guard = Some(code);
        }
        let _ = exit_tx.send(code);
    });

    let handle = ProcessHandle::new(
        writer_tx,
        Box::new(PipeChildTerminator {
            #[cfg(unix)]
            process_group_id,
        }),
        stdout_handle,
        stderr_handle,
        writer_handle,
        wait_handle,
        exit_status,
        exit_code,
        /*pty_handles*/ None,
        /*resizer*/ None,
    );

    Ok(SpawnedProcess {
        session: handle,
        stdout_rx,
        stderr_rx,
        exit_rx,
    })
}

/// Spawn a process using regular pipes and preserve selected inherited file
/// descriptors across exec on Unix. The executable path retains its native encoding.
pub async fn spawn_process(
    program: impl AsRef<OsStr>,
    args: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
    arg0: &Option<String>,
    inherited_fds: &[i32],
) -> Result<SpawnedProcess> {
    spawn_process_with_stdin_mode(
        program.as_ref(),
        args,
        cwd,
        env,
        arg0,
        PipeStdinMode::Piped,
        inherited_fds,
    )
    .await
}

/// Spawn a process using regular pipes, close stdin immediately, and preserve
/// selected inherited file descriptors across exec on Unix. The executable path
/// retains its native encoding.
pub async fn spawn_process_no_stdin(
    program: impl AsRef<OsStr>,
    args: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
    arg0: &Option<String>,
    inherited_fds: &[i32],
) -> Result<SpawnedProcess> {
    spawn_process_with_stdin_mode(
        program.as_ref(),
        args,
        cwd,
        env,
        arg0,
        PipeStdinMode::Null,
        inherited_fds,
    )
    .await
}

#[cfg(all(test, unix))]
#[path = "pipe_unix_tests.rs"]
mod unix_tests;
