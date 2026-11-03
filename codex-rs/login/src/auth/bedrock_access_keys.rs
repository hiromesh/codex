use serde::Deserialize;
use serde::Serialize;

/// Managed Amazon Bedrock AWS access keys persisted in auth storage.
#[derive(Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct BedrockAccessKeysAuth {
    pub access_key_id: String,
    pub secret_access_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_token: Option<String>,
}

impl std::fmt::Debug for BedrockAccessKeysAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BedrockAccessKeysAuth")
            .field("access_key_id", &"<redacted>")
            .field("secret_access_key", &"<redacted>")
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

