//! Command construction for background processes.
//! Callers retain control of environment, stdio, and process lifetime.

use std::ffi::OsStr;
use std::process::Command;

/// Construct a background command.
/// Convert with `tokio::process::Command::from` for asynchronous execution.
pub fn background_command(program: impl AsRef<OsStr>) -> Command {
    Command::new(program)
}
