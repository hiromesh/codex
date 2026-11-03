use codex_arg0::Arg0DispatchPaths;
use codex_core::config::ConfigBuilder;
use pretty_assertions::assert_eq;

use super::CheckStatus;
use super::sandbox_check;

#[tokio::test]
async fn reports_missing_linux_sandbox_helper() {
    let home = tempfile::tempdir().expect("create Codex home");
    let config = ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await
        .expect("load sandbox config");
    let arg0_paths = Arg0DispatchPaths {
        codex_linux_sandbox_exe: Some(home.path().join("missing-linux-helper")),
        ..Default::default()
    };

    let check = sandbox_check(&config, &arg0_paths);

    assert_eq!(check.status, CheckStatus::Warning);
    assert_eq!(check.summary, "Linux sandbox helper path does not exist");
}

