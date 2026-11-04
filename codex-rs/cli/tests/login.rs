use anyhow::Result;
use codex_protocol::shell_environment::OPENAI_FEDERATION_RULE_ID_ENV_VAR;
use codex_protocol::shell_environment::OPENAI_IDENTITY_TOKEN_FILE_ENV_VAR;
use predicates::str::contains;
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::path::Path;
use tempfile::TempDir;

fn codex_command(codex_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("codex")?);
    cmd.env("CODEX_HOME", codex_home);
    Ok(cmd)
}

fn write_file_auth_config(codex_home: &Path) -> Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        "cli_auth_credentials_store = \"file\"\n",
    )?;
    Ok(())
}

fn read_auth_json(codex_home: &Path) -> Result<Value> {
    let auth_json = std::fs::read_to_string(codex_home.join("auth.json"))?;
    Ok(serde_json::from_str(&auth_json)?)
}

#[test]
fn login_with_api_key_reads_stdin_and_writes_auth_json() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_file_auth_config(codex_home.path())?;

    let mut cmd = codex_command(codex_home.path())?;
    let login = cmd
        .args([
            "-c",
            "forced_login_method=\"api\"",
            "login",
            "--with-api-key",
        ])
        .write_stdin("sk-test\n")
        .assert()
        .success();
    insta::assert_snapshot!(String::from_utf8_lossy(&login.get_output().stderr), @r"
    Reading API key from stdin...
    API key saved. Authentication is checked when you send a request.
    ");

    let auth = read_auth_json(codex_home.path())?;
    assert_eq!(auth["OPENAI_API_KEY"], "sk-test");
    assert!(auth.get("tokens").is_none());
    assert!(auth.get("agent_identity").is_none());

    let status = codex_command(codex_home.path())?
        .env_remove(OPENAI_FEDERATION_RULE_ID_ENV_VAR)
        .env_remove(OPENAI_IDENTITY_TOKEN_FILE_ENV_VAR)
        .args(["login", "status"])
        .assert()
        .success();
    insta::assert_snapshot!(String::from_utf8_lossy(&status.get_output().stderr), @r"
    API key configured - ***
    Authentication is checked when you send a request.
    ");

    codex_command(codex_home.path())?
        .args(["-c", "forced_login_method=\"api\""])
        .arg("login")
        .assert()
        .failure()
        .stderr(contains("API key login is required"));
    assert_eq!(read_auth_json(codex_home.path())?, auth);

    codex_command(codex_home.path())?
        .args(["-c", "forced_login_method=\"api\""])
        .args(["login", "--device-auth"])
        .assert()
        .failure();
    assert_eq!(read_auth_json(codex_home.path())?, auth);

    Ok(())
}

#[test]
fn api_key_auth_commands_work_when_log_dir_is_unavailable() -> Result<()> {
    let codex_home = TempDir::new()?;
    let log_dir = codex_home.path().join("unavailable-logs");
    std::fs::write(&log_dir, "not a directory")?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        format!("cli_auth_credentials_store = \"file\"\nlog_dir = {log_dir:?}\n"),
    )?;

    codex_command(codex_home.path())?
        .args(["login", "--with-api-key"])
        .write_stdin("sk-test\n")
        .assert()
        .success()
        .stderr(contains("Warning: failed to create login log directory"));
    assert_eq!(
        read_auth_json(codex_home.path())?["OPENAI_API_KEY"],
        "sk-test"
    );

    codex_command(codex_home.path())?
        .args(["login", "status"])
        .assert()
        .success()
        .stderr(contains("API key"));

    codex_command(codex_home.path())?
        .arg("logout")
        .assert()
        .success()
        .stderr(contains("Successfully logged out"));
    assert!(!codex_home.path().join("auth.json").exists());

    Ok(())
}

#[test]
fn login_status_reports_auth_storage_errors() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_file_auth_config(codex_home.path())?;
    std::fs::write(codex_home.path().join("auth.json"), "{invalid json")?;

    codex_command(codex_home.path())?
        .args(["login", "status"])
        .assert()
        .failure()
        .stderr(contains("Error checking login status:"));

    Ok(())
}
