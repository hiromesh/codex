//! Identifies the sandbox implementation and overrides selected for a command.

use serde::Deserialize;
use serde::Serialize;

/// Controller decision that overrides normal sandbox selection for a command.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SandboxOverride {
    /// Use normal sandbox selection.
    #[default]
    NoOverride,
    /// Broaden approved filesystem access while retaining denied-read enforcement.
    EscalatedSandboxWithRestrictions,
    /// Bypass the sandbox on the first attempt.
    BypassSandboxFirstAttempt,
}

impl SandboxOverride {
    pub fn is_no_override(&self) -> bool {
        *self == Self::NoOverride
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SandboxType {
    None,
    MacosSeatbelt,
    LinuxSeccomp,
}

impl SandboxType {
    pub fn as_metric_tag(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::MacosSeatbelt => "seatbelt",
            Self::LinuxSeccomp => "seccomp",
        }
    }
}
