//! Ordinary refresh resolution without Session construction.

use super::*;
use crate::config::ConfigBuilder;
use codex_config::CONFIG_TOML_FILE;
use codex_config::test_support::CloudConfigBundleFixture;
use pretty_assertions::assert_eq;

async fn base_config() -> (tempfile::TempDir, Config) {
    let home = tempfile::tempdir().unwrap();
    let config = ConfigBuilder::without_managed_config_for_tests()
        .codex_home(home.path().to_path_buf())
        .fallback_cwd(Some(home.path().to_path_buf()))
        .build()
        .await
        .unwrap();
    (home, config)
}

async fn layered_config(
    base: &Config,
    user: &str,
    managed: &str,
    requirements: &str,
    session: &str,
) -> Config {
    std::fs::write(base.codex_home.join(CONFIG_TOML_FILE), user).unwrap();
    let overrides: toml::Table = toml::from_str(session).unwrap();
    ConfigBuilder::without_managed_config_for_tests()
        .codex_home(base.codex_home.to_path_buf())
        .fallback_cwd(Some(base.cwd.to_path_buf()))
        .cli_overrides(overrides.into_iter().collect())
        .cloud_config_bundle(
            CloudConfigBundleFixture::enterprise_config(managed)
                .add_enterprise_requirement(format!("[features]\n{requirements}"))
                .into_loader(),
        )
        .build()
        .await
        .unwrap()
}

#[tokio::test]
async fn ordinary_transport_transitions_use_the_resolved_incoming_map() {
    let (_home, base) = base_config().await;
    let command = "[mcp_servers.ordinary]\ncommand = 'ordinary-server'";
    let url = "[mcp_servers.ordinary]\nurl = 'https://ordinary.example/mcp'";
    for scope in [RuntimeConfigRefresh::User, RuntimeConfigRefresh::Mcp] {
        let (current, incoming) = match scope {
            RuntimeConfigRefresh::User => (
                layered_config(&base, "", "", "", url).await,
                layered_config(&base, command, "", "", "").await,
            ),
            RuntimeConfigRefresh::Mcp => (
                layered_config(&base, command, "", "", "").await,
                layered_config(&base, "", url, "", "").await,
            ),
            RuntimeConfigRefresh::UserFiles => unreachable!(),
        };
        let refreshed = current.resolve_runtime_refresh(&incoming, scope).unwrap();
        assert_eq!(refreshed.mcp_servers.get(), incoming.mcp_servers.get());
    }
}
