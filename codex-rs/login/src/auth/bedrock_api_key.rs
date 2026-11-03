use std::fmt;

use serde::Deserialize;
use serde::Serialize;

/// Managed Amazon Bedrock API key persisted in `auth.json`.
#[derive(Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct BedrockApiKeyAuth {
    pub api_key: String,
    pub region: String,
}

impl fmt::Debug for BedrockApiKeyAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BedrockApiKeyAuth")
            .field("api_key", &"<redacted>")
            .field("region", &self.region)
            .finish()
    }
}
