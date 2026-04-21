//! Sandbox backends for isolated tool execution in OpenHarness.
//!
//! Two backends are provided:
//! - [`LandlockBackend`] — Linux-native filesystem sandboxing via the `landlock` crate.
//! - [`DockerBackend`] — Container isolation via the `bollard` Docker API client.
//!
//! Both implement the [`SandboxBackend`] async trait.

pub mod error;
pub mod path_validator;
pub mod spec;
pub mod docker;

#[cfg(target_os = "linux")]
pub mod landlock;

pub use error::SandboxError;
pub use spec::{ExecResult, NetworkPolicy, SandboxHandle, SandboxSpec};
pub use docker::DockerBackend;

#[cfg(target_os = "linux")]
pub use landlock::LandlockBackend;

use async_trait::async_trait;

/// Common interface implemented by every sandbox backend.
#[async_trait]
pub trait SandboxBackend: Send + Sync {
    /// Start a new sandbox session according to `spec`.
    ///
    /// Returns a [`SandboxHandle`] that identifies the running session.
    async fn start(&self, spec: SandboxSpec) -> Result<SandboxHandle, SandboxError>;

    /// Execute `command` inside the sandbox identified by `handle`.
    ///
    /// `input` is optionally written to the command's stdin.
    async fn exec(
        &self,
        handle: &SandboxHandle,
        command: &[&str],
        input: Option<&[u8]>,
    ) -> Result<ExecResult, SandboxError>;

    /// Stop (and clean up) the sandbox identified by `handle`.
    async fn stop(&self, handle: SandboxHandle) -> Result<(), SandboxError>;
}
