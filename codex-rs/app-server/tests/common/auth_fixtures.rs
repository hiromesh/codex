use std::path::Path;

use anyhow::Context;
use anyhow::Result;
use serde_json::json;

pub async fn mount_workspace_routing(server: &wiremock::MockServer) {
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path_regex(
            "^/(backend-api/wham|api/codex)/accounts/check$",
        ))
        .respond_with(|request: &wiremock::Request| {
            let account_id = request
                .headers
                .get("chatgpt-account-id")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("account-123");
            wiremock::ResponseTemplate::new(200).set_body_json(json!({
                "accounts": [{"id": account_id, "workspace_backend_origin": "https://chatgpt.com",
                    "account_routing_override": "NO_CONSTRAINT"}],
            }))
        })
        .mount(server)
        .await;
}

/// Writes a stored OpenAI API key into `auth.json` without going through the
/// login crate, so test fixtures stay independent of login internals.
pub fn write_api_key_auth(codex_home: &Path, api_key: impl AsRef<str>) -> Result<()> {
    write_auth_json(
        codex_home,
        &json!({
            "auth_mode": "apikey",
            "OPENAI_API_KEY": api_key.as_ref(),
        }),
    )
}

fn write_auth_json(codex_home: &Path, auth: &serde_json::Value) -> Result<()> {
    std::fs::create_dir_all(codex_home).context("create codex home")?;
    let auth_file = codex_home.join("auth.json");
    std::fs::write(&auth_file, serde_json::to_string_pretty(auth)?)
        .with_context(|| format!("write {}", auth_file.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&auth_file, std::fs::Permissions::from_mode(0o600))
            .context("set auth.json permissions")?;
    }
    Ok(())
}
