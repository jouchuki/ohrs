/// Unified error type for the oh-swarm crate.
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SwarmError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Teammate not found: {0}")]
    TeammateNotFound(String),

    #[error("Team not found: {0}")]
    TeamNotFound(String),

    #[error("Teammate already running: {0}")]
    AlreadyRunning(String),

    #[error("Persist error: {0}")]
    Persist(String),

    #[error("{0}")]
    Other(String),
}
