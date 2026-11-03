#[cfg(target_os = "macos")]
use std::ffi::CStr;
#[cfg(target_os = "macos")]
use std::ffi::OsStr;
#[cfg(target_os = "macos")]
use std::fs;
#[cfg(target_os = "macos")]
use std::os::unix::ffi::OsStrExt;
#[cfg(target_os = "macos")]
use std::path::Path;
#[cfg(target_os = "macos")]
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::process::Stdio;
#[cfg(target_os = "macos")]
use std::time::Duration;

#[cfg(target_os = "macos")]
use serde_json::Value;
#[cfg(target_os = "macos")]
use tokio::process::Command;
#[cfg(target_os = "macos")]
use tokio::time::timeout;

use super::super::CheckStatus;
use super::super::DoctorCheck;

pub(in crate::doctor) struct InstalledApp {
    pub(in crate::doctor) identity: &'static str,
    pub(in crate::doctor) version: String,
    #[cfg(target_os = "macos")]
    pub(in crate::doctor) bundle: PathBuf,
    #[cfg(target_os = "macos")]
    pub(in crate::doctor) build: u64,
}

#[derive(Clone, Copy, Debug)]
pub(in crate::doctor) struct DiscoveryError;

pub(super) async fn installed_app() -> Result<Option<InstalledApp>, DiscoveryError> {
    #[cfg(target_os = "macos")]
    {
        installed_macos_app().await
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(None)
    }
}

#[cfg(target_os = "macos")]
pub(super) fn application_running(application: &InstalledApp, candidate_pid: u32) -> bool {
    process_executable_path(candidate_pid)
        .is_some_and(|path| path.starts_with(application.bundle.join("Contents/MacOS")))
}

#[cfg(target_os = "macos")]
fn process_executable_path(pid: u32) -> Option<PathBuf> {
    let pid = i32::try_from(pid).ok()?;
    let mut buffer = [0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let result =
        unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if result <= 0 {
        return None;
    }
    let executable = CStr::from_bytes_until_nul(&buffer).ok()?;
    Some(PathBuf::from(OsStr::from_bytes(executable.to_bytes())))
}

#[cfg(not(target_os = "macos"))]
pub(super) fn application_running(_application: &InstalledApp, _candidate_pid: u32) -> bool {
    false
}

pub(super) fn desktop_check(
    id: impl Into<String>,
    status: CheckStatus,
    summary: impl Into<String>,
) -> DoctorCheck {
    DoctorCheck::new(id, "desktop", status, summary)
}

#[cfg(target_os = "macos")]
async fn installed_macos_app() -> Result<Option<InstalledApp>, DiscoveryError> {
    let applications = std::iter::once(PathBuf::from("/Applications"))
        .chain(std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Applications")))
        .flat_map(|directory| {
            ["ChatGPT.app", "Codex.app"]
                .into_iter()
                .map(move |application| directory.join(application))
        });

    for bundle in applications {
        match fs::metadata(&bundle) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(DiscoveryError),
        }
        if let Some(application) = inspect_macos_bundle(&bundle).await? {
            return Ok(Some(application));
        }
    }

    Ok(None)
}

#[cfg(target_os = "macos")]
pub(in crate::doctor) async fn inspect_macos_bundle(
    bundle: &Path,
) -> Result<Option<InstalledApp>, DiscoveryError> {
    let mut command = Command::new("/usr/bin/plutil");
    command
        .args(["-convert", "json", "-o", "-"])
        .arg(bundle.join("Contents/Info.plist"))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let output = timeout(Duration::from_secs(5), command.output())
        .await
        .map_err(|_| DiscoveryError)?
        .map_err(|_| DiscoveryError)?;
    if !output.status.success() || output.stdout.len() > 64 * 1024 {
        return Err(DiscoveryError);
    }
    let metadata: Value = serde_json::from_slice(&output.stdout).map_err(|_| DiscoveryError)?;
    if metadata.get("CFBundleIdentifier").and_then(Value::as_str) != Some("com.openai.codex") {
        return Ok(None);
    }
    let version = metadata
        .get("CFBundleShortVersionString")
        .or_else(|| metadata.get("CFBundleVersion"))
        .and_then(Value::as_str)
        .ok_or(DiscoveryError)?;
    let build = metadata
        .get("CFBundleVersion")
        .and_then(Value::as_str)
        .and_then(|value| value.parse().ok())
        .ok_or(DiscoveryError)?;

    Ok(Some(InstalledApp {
        identity: "com.openai.codex",
        version: version.to_string(),
        bundle: bundle.to_path_buf(),
        build,
    }))
}
