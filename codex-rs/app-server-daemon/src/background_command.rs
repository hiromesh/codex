//! Recovers background startup after the launching directory is removed.
//! Preserve process-scoped paths; retain usable Unix cwd defaults for clients.

use std::path::Path;
use std::process::Command;

use anyhow::Result;

pub(crate) fn set_working_directory(command: &mut Command, state_dir: &Path) -> Result<()> {
    #[cfg(unix)]
    let cwd_missing = match std::env::current_dir() {
        Ok(_) => false,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => true,
        Err(err) => return Err(err.into()),
    };
    if let Ok(value) = std::env::var("CODEX_HOME")
        && !value.is_empty()
    {
        let home = codex_utils_home_dir::find_codex_home()?;
        // Remove parent components without discarding a valid configured symlink alias.
        let absolute = std::path::absolute(value)?;
        let alias = home.join(&absolute);
        let preserved = if alias.canonicalize().ok().as_ref() == Some(&home) {
            alias.as_path()
        } else if home.as_path().to_str().is_some() {
            home.as_path()
        } else {
            // The home resolver ignores non-Unicode env values; keep the valid original alias.
            absolute.as_path()
        };
        command.env("CODEX_HOME", preserved);
    }
    // Preserve process-scoped paths before changing cwd; CA names match CUSTOM_CA_ENV_KEYS.
    for name in [
    Ok(())
}
