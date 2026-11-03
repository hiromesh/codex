//! Resolves local sandbox implementations for an exec request.

use codex_file_system::FileSystemSandboxContext;
use codex_protocol::models::PermissionProfile;
use codex_sandboxing::SandboxManager;
use codex_sandboxing::SandboxType;
use codex_sandboxing::SandboxablePreference;

pub(crate) fn select_sandbox(
    manager: &SandboxManager,
    permission_profile: &PermissionProfile,
    _sandbox_context: &FileSystemSandboxContext,
    has_managed_network_requirements: bool,
) -> SandboxType {
    manager.select_initial(
        permission_profile,
        SandboxablePreference::Require,
        has_managed_network_requirements,
    )
}
