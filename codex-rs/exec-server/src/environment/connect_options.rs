//! Host-owned connection options retained for API compatibility.

use std::collections::HashMap;
use std::time::Duration;

/// Host-owned connection settings for a named remote execution environment.
///
/// Remote execution environments have been removed. The type remains so
/// downstream callers can be migrated without changing their request types.
#[derive(Clone, Eq, PartialEq)]
pub struct RemoteEnvironmentOptions {
    /// Direct `ws://` or `wss://` endpoint for the remote environment.
    pub exec_server_url: String,
    /// Optional connection timeout; the standard exec-server default applies when omitted.
    pub connect_timeout: Option<Duration>,
    /// Additional trusted headers for every physical WebSocket connection.
    pub http_headers: HashMap<String, String>,
}

impl std::fmt::Debug for RemoteEnvironmentOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteEnvironmentOptions")
            .field("exec_server_url", &self.exec_server_url)
            .field("connect_timeout", &self.connect_timeout)
            .field("http_headers", &"<redacted>")
            .finish()
    }
}
