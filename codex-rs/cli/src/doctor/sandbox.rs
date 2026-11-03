
use codex_arg0::Arg0DispatchPaths;
use codex_config::RequirementSource;
use codex_core::config::Config;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;

use super::CheckStatus;
use super::DoctorCheck;
use super::push_path_detail;

pub(super) fn sandbox_check(config: &Config, arg0_paths: &Arg0DispatchPaths) -> DoctorCheck {
    let mut details = Vec::new();
    details.push(format!(
        "approval policy: {:?}",
        config.permissions.approval_policy.value()
    ));
    let file_system_sandbox = config.permissions.file_system_sandbox_policy();
    details.push(format!("filesystem sandbox: {}", file_system_sandbox.kind));
    let denied_entries = file_system_sandbox
        .entries
        .iter()
        .filter(|entry| entry.access == FileSystemAccessMode::Deny);
    details.push(format!(
        "denied-read rules: {}",
        denied_entries.clone().count()
    ));
    details.push(format!(
        "denied-read glob rules: {}",
        denied_entries
            .filter(|entry| matches!(entry.path, FileSystemPath::GlobPattern { .. }))
            .count()
    ));
    details.push(format!(
        "glob scan max depth: {}",
        file_system_sandbox
            .glob_scan_max_depth
            .map_or_else(|| "unbounded".to_string(), |depth| depth.to_string())
    ));
    details.push(format!(
        "managed filesystem source: {}",
        match config
            .config_layer_stack
            .requirements()
            .filesystem
            .as_ref()
            .map(|value| &value.source)
        {
            None => "none",
            Some(RequirementSource::Unknown) => "unknown",
            Some(RequirementSource::EnterpriseManaged { .. }) => "cloud",
            Some(RequirementSource::Composite { .. }) => "multiple layers",
            Some(RequirementSource::MdmManagedPreferences { .. }) => "MDM",
            Some(RequirementSource::SystemRequirementsToml { .. }) => "system",
            Some(RequirementSource::LegacyManagedConfigTomlFromFile { .. }) => "legacy file",
            Some(RequirementSource::LegacyManagedConfigTomlFromMdm) => "legacy MDM",
        }
    ));
    details.push(format!(
        "network sandbox: {}",
        config.permissions.network_sandbox_policy()
    ));
    push_path_detail(
        &mut details,
        "codex-linux-sandbox helper",
        arg0_paths.codex_linux_sandbox_exe.as_deref(),
    );

    let mut status = CheckStatus::Ok;
    let mut summary = "sandbox configuration is readable".to_string();
    if let Some(helper) = arg0_paths.codex_linux_sandbox_exe.as_deref()
        && !helper.exists()
    {
        status = CheckStatus::Warning;
        summary = "Linux sandbox helper path does not exist".to_string();
    }

    let check = DoctorCheck::new("sandbox.helpers", "sandbox", status, summary).details(details);

    check
}

#[cfg(test)]
#[path = "sandbox_tests.rs"]
mod tests;
