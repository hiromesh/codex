const PERSONAL_ACCESS_TOKEN_PREFIX: &str = "at-";

/// Returns the personal access token when the value uses the supported prefix.
pub(super) fn classify_codex_access_token(access_token: &str) -> Option<&str> {
    access_token
        .starts_with(PERSONAL_ACCESS_TOKEN_PREFIX)
        .then_some(access_token)
}

#[cfg(test)]
#[path = "access_token_tests.rs"]
mod tests;
