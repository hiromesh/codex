//! Tracks credential changes separately from ownership changes, before notifications coalesce.

use super::CodexAuth;

/// Opaque revisions local to one auth manager. Consumers must reset on reconnect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AuthChangeState {
    /// Advances on cached credential changes and explicit invalidation of pending reads.
    pub generation: u64,
    /// Advances on login, logout, or changes of user, workspace, or auth mode.
    /// A verified legacy-to-OAuth credential migration preserves the same owner.
    /// Credential changes with incomplete owner identity also advance this revision.
    /// Losing login metadata alone preserves the account owner.
    pub owner_generation: u64,
    /// Advances on an underlying actor credential/source change, including a new OAuth login.
    /// Ordinary refresh within an identifiable OAuth session preserves this revision.
    pub source_generation: u64,
    /// Preserves strict login ownership for EMA, including loss of login metadata.
    pub login_generation: u64,
}

/// Whether request preparation can use the credentials it selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestAuthChange {
    Unchanged,
    CredentialsChanged,
    OwnerChanged,
}

impl AuthChangeState {
    /// Compare a completed attempt with its original owner and starting credential revision.
    pub fn compare_for_request(&self, expected_owner: u64, prepared: Self) -> RequestAuthChange {
        if self.owner_generation != expected_owner {
            RequestAuthChange::OwnerChanged
        } else if self.generation != prepared.generation {
            RequestAuthChange::CredentialsChanged
        } else {
            RequestAuthChange::Unchanged
        }
    }
}

pub(super) fn same_owner(previous: Option<&CodexAuth>, current: Option<&CodexAuth>) -> bool {
    let (Some(previous), Some(current)) = (previous, current) else {
        return false;
    };
    previous.api_key() == current.api_key()
}

pub(super) fn same_login(previous: Option<&CodexAuth>, current: Option<&CodexAuth>) -> bool {
    same_owner(previous, current)
}
