//! Error type for sandbox operations.

use thiserror::Error;

/// All errors that can be returned by sandbox backends.
#[derive(Debug, Error)]
pub enum SandboxError {
    /// The backend is not available on this platform or kernel.
    #[error("sandbox unavailable: {0}")]
    Unavailable(String),

    /// A path failed validation (e.g. mounts into `/etc`, `/root/.ssh`, …).
    #[error("path validation failed: {0}")]
    PathValidation(String),

    /// The underlying Docker API returned an error.
    #[error("docker error: {0}")]
    Docker(#[from] bollard::errors::Error),

    /// The sandbox handle references an unknown or already-stopped session.
    #[error("invalid sandbox handle: {0}")]
    InvalidHandle(String),

    /// A command spawned inside the sandbox could not be started or waited for.
    #[error("exec error: {0}")]
    Exec(String),

    /// Generic I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Some other internal error.
    #[error("internal error: {0}")]
    Internal(String),
}
