use super::*;

#[test]
fn classifies_personal_access_tokens_by_prefix() {
    assert_eq!(
        classify_codex_access_token("at-example"),
        Some("at-example")
    );
    assert_eq!(classify_codex_access_token("header.payload.signature"), None);
}

