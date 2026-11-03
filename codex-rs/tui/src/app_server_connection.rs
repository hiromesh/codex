//! Applies the selected server's socket policy on initial connections and reconnects.

use crate::AppServerTarget;
use crate::connect_remote_app_server;
use codex_app_server_client::AppServerClient;

pub(crate) async fn connect(target: &AppServerTarget) -> color_eyre::Result<AppServerClient> {
    match target {
        AppServerTarget::Embedded => {
            color_eyre::eyre::bail!("embedded sessions have no remote connection")
        }
        AppServerTarget::LocalDaemon { endpoint, .. } | AppServerTarget::Remote { endpoint } => {
            connect_remote_app_server(endpoint.clone()).await
        }
    }
}
