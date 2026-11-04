//! Optional account-security reminders for the local, ChatGPT-authenticated CLI.
//! The server owns eligibility, rollout and cutoff copy. Reads never block a turn;
//! no reminder analytics are sent. Unknown or unavailable state produces no notice.
//!
//! ChatGPT authentication is no longer supported, so prefetching is disabled; the
//! data types remain for the chat widget's notice rendering.

use crate::app_event_sender::AppEventSender;
use crate::app_server_session::AppServerSession;
use crate::legacy_core::config::Config;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Notice {
    pub title: String,
    pub description: String,
    pub action: Action,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Action {
    pub label: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    pub account: String,
    pub user: String,
}

impl Notice {
    pub(crate) fn valid(&self) -> bool {
        !self.title.trim().is_empty()
            && self.title.len() <= 256
            && self.description.len() <= 2048
            && !self.title.chars().any(char::is_control)
            && !self.description.chars().any(char::is_control)
            && !self.action.label.trim().is_empty()
            && self.action.label.len() <= 128
            && !self.action.label.chars().any(char::is_control)
            && url::Url::parse(&self.action.url).is_ok_and(|url| {
                url.scheme() == "https"
                    && url.host_str() == Some("chatgpt.com")
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.port().is_none()
            })
    }
}

pub(crate) fn prefetch(
    _config: &Config,
    _server: &AppServerSession,
    _tx: AppEventSender,
    _request_id: Uuid,
) {
}
