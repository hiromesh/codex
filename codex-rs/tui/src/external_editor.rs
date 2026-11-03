use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;

use codex_protocol::permissions::FileSystemSandboxPolicy;
use color_eyre::eyre::Report;
use color_eyre::eyre::Result;
use tempfile::Builder;
use thiserror::Error;
use tokio::process::Command;

#[derive(Debug, Error)]
pub(crate) enum EditorError {
    #[error("neither VISUAL nor EDITOR is set")]
    MissingEditor,
    #[error("failed to parse editor command")]
    ParseFailed,
    #[error("editor command is empty")]
    EmptyCommand,
}

/// Resolve the editor command from environment variables.
/// Prefers `VISUAL` over `EDITOR`.
pub(crate) fn resolve_editor_command() -> std::result::Result<Vec<String>, EditorError> {
    let raw = env::var("VISUAL")
        .or_else(|_| env::var("EDITOR"))
        .map_err(|_| EditorError::MissingEditor)?;
    let parts = {
        {
            shlex::split(&raw).ok_or(EditorError::ParseFailed)?
        }
    };
    if parts.is_empty() {
        return Err(EditorError::EmptyCommand);
    }
    Ok(parts)
}

pub(super) fn editor_directory(
    candidate_homes: &[&Path],
    file_system_policy: &FileSystemSandboxPolicy,
    cwd: &Path,
) -> Result<PathBuf> {
    let writable_roots = file_system_policy.get_writable_roots_with_cwd(cwd);
    let mut error = Report::msg("editor directory must not be writable");
    let mut rejected_writable = false;

    for candidate_home in candidate_homes {
        let canonical_home = match dunce::canonicalize(candidate_home) {
            Ok(path) => path,
            Err(canonicalize_error)
                if canonicalize_error.kind() == std::io::ErrorKind::NotFound =>
            {
                let Some(parent) = candidate_home.parent() else {
                    error = Report::msg("editor directory has no parent");
                    continue;
                };
                let Some(name) = candidate_home.file_name() else {
                    error = Report::msg("editor directory has no parent");
                    continue;
                };
                match dunce::canonicalize(parent) {
                    Ok(parent) => parent.join(name),
                    Err(canonicalize_error) => {
                        error = canonicalize_error.into();
                        continue;
                    }
                }
            }
            Err(canonicalize_error) => {
                error = canonicalize_error.into();
                continue;
            }
        };
        let editor_directory = canonical_home.join("editor");
        let logical_editor_directory = candidate_home.join("editor");

        if !file_system_policy.has_full_disk_write_access()
            && [&logical_editor_directory, &editor_directory]
                .into_iter()
                .any(|directory| {
                    let Some(parent) = directory.parent() else {
                        return true;
                    };
                    let is_writable = file_system_policy
                        .can_write_local_path_with_cwd(directory, cwd)
                        || file_system_policy.can_write_local_path_with_cwd(parent, cwd)
                        || writable_roots.iter().any(|root| {
                            root.is_path_writable(directory)
                                || root.root.as_path().starts_with(directory)
                        });
                    is_writable
                })
        {
            error = Report::msg("editor directory must not be writable");
            rejected_writable = true;
            continue;
        }

        if let Err(create_error) = fs::create_dir_all(&editor_directory) {
            error = create_error.into();
            continue;
        }
        match dunce::canonicalize(&editor_directory) {
            Ok(path) if path == editor_directory => return Ok(editor_directory),
            Ok(_) => {
                error = Report::msg("editor directory must not contain symbolic links");
            }
            Err(canonicalize_error) => {
                error = canonicalize_error.into();
            }
        }
    }

    if rejected_writable {
        Err(Report::msg("editor directory must not be writable"))
    } else {
        Err(error)
    }
}

/// Write `seed` to a temp file, launch the editor command, and return the updated content.
pub(crate) async fn run_editor(
    seed: &str,
    editor_cmd: &[String],
    codex_home: &Path,
    file_system_policy: &FileSystemSandboxPolicy,
    cwd: &Path,
) -> Result<String> {
    if editor_cmd.is_empty() {
        return Err(Report::msg("editor command is empty"));
    }

    let default_codex_home = dirs::home_dir().map(|home| home.join(".codex"));
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let project_codex_home = cwd.join(".codex");
    let mut candidate_homes = vec![codex_home];
    if let Some(default_codex_home) = default_codex_home.as_deref() {
        candidate_homes.push(default_codex_home);
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    candidate_homes.push(&project_codex_home);
    let editor_directory = editor_directory(&candidate_homes, file_system_policy, cwd)?;
    // Convert to TempPath immediately so no file handle stays open on Windows.
    let temp_path = Builder::new()
        .suffix(".md")
        .tempfile_in(editor_directory)?
        .into_temp_path();
    fs::write(&temp_path, seed)?;

    let mut cmd = {
        {
            Command::new(&editor_cmd[0])
        }
    };
    if editor_cmd.len() > 1 {
        cmd.args(&editor_cmd[1..]);
    }
    let status = cmd
        .arg(&temp_path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await?;

    if !status.success() {
        return Err(Report::msg(format!("editor exited with status {status}")));
    }

    let contents = fs::read_to_string(&temp_path)?;
    Ok(contents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serial_test::serial;
    #[cfg(unix)]
    use tempfile::tempdir;

    struct EnvGuard {
        visual: Option<String>,
        editor: Option<String>,
    }

    impl EnvGuard {
        fn new() -> Self {
            Self {
                visual: env::var("VISUAL").ok(),
                editor: env::var("EDITOR").ok(),
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            restore_env("VISUAL", self.visual.take());
            restore_env("EDITOR", self.editor.take());
        }
    }

    fn restore_env(key: &str, value: Option<String>) {
        match value {
            Some(val) => unsafe { env::set_var(key, val) },
            None => unsafe { env::remove_var(key) },
        }
    }

    #[test]
    #[serial]
    fn resolve_editor_prefers_visual() {
        let _guard = EnvGuard::new();
        unsafe {
            env::set_var("VISUAL", "vis");
            env::set_var("EDITOR", "ed");
        }
        let cmd = resolve_editor_command().unwrap();
        assert_eq!(cmd, vec!["vis".to_string()]);
    }

    #[test]
    #[serial]
    fn resolve_editor_errors_when_unset() {
        let _guard = EnvGuard::new();
        unsafe {
            env::remove_var("VISUAL");
            env::remove_var("EDITOR");
        }
        assert!(matches!(
            resolve_editor_command(),
            Err(EditorError::MissingEditor)
        ));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn run_editor_returns_updated_content() {
        let dir = tempdir().unwrap();
        let script_path = dir.path().join("edit.sh");
        codex_utils_cargo_bin::write_executable(
            &script_path,
            "#!/bin/sh\nprintf \"edited\" > \"$1\"\n",
        )
        .unwrap();

        let cmd = vec![script_path.to_string_lossy().to_string()];
        let policy = FileSystemSandboxPolicy::read_only();
        let result = run_editor("seed", &cmd, dir.path(), &policy, dir.path())
            .await
            .unwrap();
        assert_eq!(result, "edited".to_string());
    }
}

#[cfg(test)]
#[path = "external_editor_tests.rs"]
mod buffer_tests;
