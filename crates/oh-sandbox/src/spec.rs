//! Core data types shared by all sandbox backends.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Specification describing the environment for one sandbox session.
#[derive(Debug, Clone)]
pub struct SandboxSpec {
    /// Working directory inside the sandbox.
    pub cwd: PathBuf,

    /// Paths the sandboxed process may **read**.
    pub allow_read: Vec<PathBuf>,

    /// Paths the sandboxed process may **read and write**.
    pub allow_write: Vec<PathBuf>,

    /// Network policy for the sandboxed process.
    pub allow_net: NetworkPolicy,

    /// Environment variables to inject.
    pub env: HashMap<String, String>,

    /// Container image to use (Docker backend only; ignored by Landlock).
    pub image: Option<String>,
}

/// Network access policy.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum NetworkPolicy {
    /// No network access.
    #[default]
    None,
    /// Loopback interface only.
    Localhost,
    /// Outbound connections to specific host names are permitted.
    AllowList(Vec<String>),
    /// Unrestricted network access.
    All,
}

/// An opaque handle returned by [`SandboxBackend::start`] and consumed by
/// [`SandboxBackend::exec`] / [`SandboxBackend::stop`].
#[derive(Debug, Clone)]
pub struct SandboxHandle {
    /// A UUID identifying this session.
    pub id: String,

    /// Backend-specific state attached to the handle.
    pub(crate) inner: HandleInner,
}

#[derive(Debug, Clone)]
pub(crate) enum HandleInner {
    /// Landlock: the child process PID we are tracking.
    #[cfg(target_os = "linux")]
    Landlock { pid: Option<u32> },

    /// Docker: the container ID/name to exec into.
    Docker { container_id: String },

    /// Used in tests / non-Linux builds for Landlock stubs.
    #[allow(dead_code)]
    Noop,
}

/// The result of a command executed inside a sandbox.
#[derive(Debug, Clone)]
pub struct ExecResult {
    /// Process exit code.
    pub status: i32,
    /// Captured standard output.
    pub stdout: Vec<u8>,
    /// Captured standard error.
    pub stderr: Vec<u8>,
}
