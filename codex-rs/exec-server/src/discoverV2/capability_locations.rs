//! Prewarms fixed standalone skill locations before discovery.

mod candidates;
use candidates::CandidateCapabilityLocations;

use std::io;

use crate::LocalFileSystem;
use codex_file_watcher::WatchPath;
use codex_utils_path_uri::PathUri;
use serde::Deserialize;
use serde::Serialize;

/// Fixed paths are supplied by the executor, not read from the sandbox helper's sanitized env.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CapabilityLocationRequest {
    pub codex_home: PathUri,
    pub user_home: Option<PathUri>,
    pub system_skills_root: Option<PathUri>,
}

const MAX_LOCATIONS: usize = 512;
const AGENTS_SKILLS_SUFFIX: &str = ".agents/skills";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CapabilityLocation {
    pub root: PathUri,
    pub standalone_root_id: Option<String>,
}

/// Selected locations; membership does not authorize capability reads.
#[derive(Clone, Default)]
pub(crate) struct CapabilityLocations {
    pub locations: Vec<CapabilityLocation>,
    pub watches: Vec<WatchPath>,
    pub warnings: Vec<String>,
}

/// Resolves fixed home roots on the executor without reading configuration or using a cwd.
pub(crate) async fn prewarm_locations(
    fs: &LocalFileSystem,
    request: &CapabilityLocationRequest,
) -> io::Result<CapabilityLocations> {
    let codex_home = request.codex_home.to_abs_path()?;
    let user_home = request
        .user_home
        .as_ref()
        .map(PathUri::to_abs_path)
        .transpose()?;
    // Fixed standalone roots extend Flora's V1 root; project skill discovery is deferred.
    let mut skill_roots = vec![("codex-home-skills", codex_home.join("skills"))];
    if let Some(home) = user_home {
        skill_roots.push(("user-agent-skills", home.join(AGENTS_SKILLS_SUFFIX)));
    }
    if let Some(system_root) = &request.system_skills_root {
        skill_roots.push(("system-skills", system_root.to_abs_path()?));
    }
    CandidateCapabilityLocations::resolve(
        fs,
        skill_roots
            .into_iter()
            .map(|(id, root)| crate::SkillDiscoveryRoot {
                id: id.to_string(),
                path: PathUri::from_abs_path(&root),
            })
            .collect(),
    )
    .await
}

#[cfg(test)]
#[path = "tests/capability_locations_tests.rs"]
pub(crate) mod tests;
