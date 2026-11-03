//! Program resolution for MCP server execution.
//!
//! Unix handles PATH resolution and script execution natively through shebangs,
//! so the program is returned unchanged.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;

/// Resolves a program to its executable path on Unix systems.
///
/// Unix systems handle PATH resolution and script execution natively through
/// the kernel's shebang (`#!`) mechanism, so this function simply returns
/// the program name unchanged.
#[cfg(unix)]
pub fn resolve(
    program: OsString,
    _env: &HashMap<OsString, OsString>,
    _cwd: &Path,
) -> std::io::Result<OsString> {
    Ok(program)
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::create_env_for_mcp_server;
    use anyhow::Result;
    use std::path::Path;
    use tempfile::TempDir;
    use tokio::process::Command;

    /// Unix: Verifies the OS handles script execution without file extensions.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_unix_executes_script_without_extension() -> Result<()> {
        let env = TestExecutableEnv::new()?;
        let output = Command::new(&env.program_name)
            .envs(&env.mcp_env)
            .output()
            .await;

        assert!(
            output.is_ok(),
            "Unix should execute PATH-resolved scripts directly: {output:?}"
        );
        Ok(())
    }

    /// Verifies program resolution enables successful execution on all platforms.
    #[tokio::test]
    async fn test_resolved_program_executes_successfully() -> Result<()> {
        let env = TestExecutableEnv::new()?;
        let program = OsString::from(&env.program_name);

        // Apply platform-specific resolution
        let resolved = resolve(program, &env.mcp_env, std::env::current_dir()?.as_path())?;

        // Verify resolved path executes successfully
        let mut cmd = Command::new(resolved);
        cmd.envs(&env.mcp_env);
        let output = cmd.output().await;

        assert!(
            output.is_ok(),
            "Resolved program should execute successfully"
        );
        Ok(())
    }

    // Test fixture for creating temporary executables in a controlled environment.
    struct TestExecutableEnv {
        // Held to prevent the temporary directory from being deleted.
        _temp_dir: TempDir,
        program_name: String,
        mcp_env: HashMap<OsString, OsString>,
    }

    impl TestExecutableEnv {
        const TEST_PROGRAM: &'static str = "test_mcp_server";

        fn new() -> Result<Self> {
            let temp_dir = TempDir::new()?;
            let dir_path = temp_dir.path();

            Self::create_executable(dir_path)?;

            // Build a clean environment with the temp dir in the PATH.
            let mut extra_env = HashMap::new();
            extra_env.insert(OsString::from("PATH"), Self::build_path_env_var(dir_path));

            let mcp_env = create_env_for_mcp_server(Some(extra_env), &[])?;

            Ok(Self {
                _temp_dir: temp_dir,
                program_name: Self::TEST_PROGRAM.to_string(),
                mcp_env,
            })
        }

        /// Creates a simple, platform-specific executable script.
        fn create_executable(dir: &Path) -> Result<()> {
            #[cfg(unix)]
            {
                let file = dir.join(Self::TEST_PROGRAM);
                codex_utils_cargo_bin::write_executable(&file, "#!/bin/sh\nexit 0")?;
            }

            Ok(())
        }

        /// Prepends the given directory to the system's PATH variable.
        fn build_path_env_var(dir: &Path) -> OsString {
            let mut path = OsString::from(dir.as_os_str());
            if let Some(current) = std::env::var_os("PATH") {
                let sep = ":";
                path.push(sep);
                path.push(current);
            }
            path
        }

    }
}
