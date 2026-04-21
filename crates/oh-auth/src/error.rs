//! Auth error type.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("keyring error: {0}")]
    Keyring(String),

    #[error("file storage error: {0}")]
    File(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("credential not found for {provider} (label={label:?})")]
    NotFound {
        provider: String,
        label: Option<String>,
    },

    #[error("OAuth error: {0}")]
    OAuth(String),

    #[error("{0}")]
    Other(String),
}
