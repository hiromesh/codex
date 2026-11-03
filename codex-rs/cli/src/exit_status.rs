#[cfg(unix)]
pub(crate) fn handle_exit_status(status: std::process::ExitStatus) -> ! {
    use std::os::unix::process::ExitStatusExt;

    // Use ExitStatus to derive the exit code.
    if let Some(code) = status.code() {
        std::process::exit(code);
    } else if let Some(signal) = status.signal() {
        std::process::exit(128 + signal);
    } else {
        std::process::exit(1);
    }
}

