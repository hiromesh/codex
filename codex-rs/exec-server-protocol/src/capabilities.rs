//! Executor capability inventory; callers apply identity and enablement.

use codex_file_system::FileSystemSandboxContext;
use codex_skills::SkillDependencies;
use codex_skills::SkillPolicy;
use codex_utils_path_uri::PathUri;
use serde::Deserialize;
use serde::Serialize;
use std::fmt;
use std::sync::Arc;

pub const CAPABILITIES_DISCOVER_V2_METHOD: &str = "capabilities/discoverV2";

/// Caller-bound discovery result shared by capability consumers within a model step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutorCapabilityDiscoveryV2Snapshot {
    pub environment_id: String,
    pub result: Result<Arc<DiscoverV2CapabilitiesResponse>, String>,
}

impl ExecutorCapabilityDiscoveryV2Snapshot {
    /// Compares discovery identity, not inventory contents; repeated errors compare by message.
    pub fn same_discovery(&self, other: &Self) -> bool {
        self.environment_id == other.environment_id
            && match (&self.result, &other.result) {
                (Ok(previous), Ok(current)) => Arc::ptr_eq(previous, current),
                (Err(previous), Err(current)) => previous == current,
                (Ok(_), Err(_)) | (Err(_), Ok(_)) => false,
            }
    }
}

/// Discover installed capabilities for a thread, without client-supplied installation paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverV2CapabilitiesRequest {
    pub cwd: PathUri,
    #[serde(default)]
    pub sandbox: Option<FileSystemSandboxContext>,
}

/// Discovered metadata before caller policy is applied.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverV2CapabilitiesResponse {
    /// Skills outside installed packages.
    pub skills: Vec<ExecutorSkill>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Executor-owned search root for a standalone skill.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillDiscoveryRoot {
    pub id: String,
    pub path: PathUri,
}

/// Skill metadata, excluding instructions and resources.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutorSkill {
    /// Final skill name.
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub short_description: Option<String>,
    /// Path to SKILL.md, not its containing directory.
    pub path: PathUri,
    /// Absent for responses from older executors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_root: Option<SkillDiscoveryRoot>,
    pub dependencies: Option<SkillDependencies>,
    pub policy: Option<SkillPolicy>,
}

impl fmt::Debug for ExecutorSkill {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Omit potentially sensitive descriptions and file contents.
        formatter
            .debug_struct("ExecutorSkill")
            .field("name", &self.name)
            .field("path", &self.path)
            .finish()
    }
}

#[cfg(test)]
#[path = "capabilities_tests.rs"]
mod tests;
