//! Compare credential sources for local runtime reuse, not server authentication.

use super::CodexAuth;

pub(super) fn same_source(previous: Option<&CodexAuth>, current: Option<&CodexAuth>) -> bool {
    let (Some(previous), Some(current)) = (previous, current) else {
        return false;
    };
    previous.api_key() == current.api_key()
}
