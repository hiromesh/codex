//! Fixtures for prewarmed capability locations.

use std::path::Path;

use super::CapabilityLocationRequest;
use codex_utils_path_uri::PathUri;

/// Maps the conventional test roots below a single temporary directory.
pub(crate) fn request(root: &Path) -> anyhow::Result<CapabilityLocationRequest> {
    Ok(CapabilityLocationRequest {
        codex_home: PathUri::from_host_native_path(root.join("codex"))?,
        user_home: Some(PathUri::from_host_native_path(root.join("home"))?),
        system_skills_root: Some(PathUri::from_host_native_path(
            root.join("etc/codex/skills"),
        )?),
    })
}

pub(crate) fn write(root: &Path, relative: &str, contents: &str) -> std::io::Result<()> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)
}
