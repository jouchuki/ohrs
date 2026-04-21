use thiserror::Error;

#[derive(Debug, Error)]
pub enum AutopilotError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Card not found: {0}")]
    NotFound(String),

    #[error("Invalid state transition: {0}")]
    InvalidTransition(String),

    #[error("Task execution failed: {0}")]
    Execution(String),
}
