//! Linux Landlock-based filesystem sandbox backend.
//!
//! [`LandlockBackend`] restricts a freshly-spawned child process to only the
//! paths listed in [`SandboxSpec::allow_read`] and
//! [`SandboxSpec::allow_write`] by applying a Landlock ruleset before
//! `execve`.
//!
//! # How it works
//!
//! 1. [`LandlockBackend::start`] validates the requested paths, stores the
//!    spec in a [`SandboxHandle`], and returns immediately.
//!
//! 2. [`LandlockBackend::exec`] builds the Landlock ruleset and applies it via
//!    `pre_exec` (run in the child process after `fork`, before `execve`).
//!    This means the restriction applies only to the child and never leaks
//!    into the parent or the Tokio runtime threads.
//!
//! 3. [`LandlockBackend::stop`] removes the session.  Landlock restrictions
//!    are scoped to the child process lifetime.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::{debug, warn};
use uuid::Uuid;

use landlock::{
    path_beneath_rules, Access, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
    ABI,
};

use crate::{
    path_validator::validate_mount_paths,
    spec::{ExecResult, HandleInner, SandboxHandle, SandboxSpec},
    SandboxBackend, SandboxError,
};

/// Stored state for an active Landlock sandbox session.
#[derive(Debug, Clone)]
struct LandlockSession {
    spec: SandboxSpec,
}

/// Linux-native filesystem sandboxing via the `landlock` kernel feature.
///
/// This backend is a no-op on kernel versions older than 5.13 (where Landlock
/// was introduced) and will return [`SandboxError::Unavailable`] in that case.
#[derive(Debug, Default)]
pub struct LandlockBackend {
    sessions: Arc<Mutex<HashMap<String, LandlockSession>>>,
}

impl LandlockBackend {
    /// Create a new [`LandlockBackend`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Check whether Landlock is usable on the current kernel.
    ///
    /// Returns `Ok(ABI)` when the kernel supports at least Landlock ABI v1,
    /// or `Err(SandboxError::Unavailable)` otherwise.
    pub fn check_availability() -> Result<ABI, SandboxError> {
        let abi = ABI::V1;
        match Ruleset::default()
            .handle_access(AccessFs::from_all(abi))
            .map_err(|e| format!("{e}"))
        {
            Ok(_) => Ok(abi),
            Err(msg) => Err(SandboxError::Unavailable(format!(
                "landlock not supported on this kernel: {msg}"
            ))),
        }
    }
}

#[async_trait]
impl SandboxBackend for LandlockBackend {
    async fn start(&self, spec: SandboxSpec) -> Result<SandboxHandle, SandboxError> {
        // Fail fast if the kernel does not support Landlock.
        LandlockBackend::check_availability()?;

        // Validate that no sensitive paths are being exposed, including cwd.
        crate::path_validator::validate_mount_path(&spec.cwd)?;
        validate_mount_paths(&spec.allow_read)?;
        validate_mount_paths(&spec.allow_write)?;

        let id = Uuid::new_v4().to_string();
        let session = LandlockSession { spec };

        self.sessions.lock().await.insert(id.clone(), session);

        debug!(sandbox_id = %id, "Landlock sandbox session started");

        Ok(SandboxHandle {
            id,
            inner: HandleInner::Landlock { pid: None },
        })
    }

    async fn exec(
        &self,
        handle: &SandboxHandle,
        command: &[&str],
        input: Option<&[u8]>,
    ) -> Result<ExecResult, SandboxError> {
        let spec = {
            let sessions = self.sessions.lock().await;
            sessions
                .get(&handle.id)
                .ok_or_else(|| SandboxError::InvalidHandle(handle.id.clone()))?
                .spec
                .clone()
        };

        if command.is_empty() {
            return Err(SandboxError::Exec("command must not be empty".into()));
        }

        let allow_read: Vec<PathBuf> = spec.allow_read.clone();
        let allow_write: Vec<PathBuf> = spec.allow_write.clone();

        // Build the ruleset *before* forking so we can validate it.
        // We then apply it in the child via `pre_exec`.
        let abi = ABI::V1;
        let read_access = AccessFs::from_read(abi);
        let write_access = AccessFs::from_write(abi);
        let rw_access = read_access | write_access;

        // Verify the ruleset can be created (may fail on old kernels).
        {
            let mut test_ruleset = Ruleset::default()
                .handle_access(AccessFs::from_all(abi))
                .map_err(|e| {
                    SandboxError::Unavailable(format!("landlock unavailable: {e}"))
                })?
                .create()
                .map_err(|e| {
                    SandboxError::Unavailable(format!("landlock ruleset create failed: {e}"))
                })?;

            if !allow_read.is_empty() {
                test_ruleset = test_ruleset
                    .add_rules(path_beneath_rules(&allow_read, read_access))
                    .map_err(|e| {
                        SandboxError::PathValidation(format!("read rule error: {e}"))
                    })?;
            }
            if !allow_write.is_empty() {
                test_ruleset = test_ruleset
                    .add_rules(path_beneath_rules(&allow_write, rw_access))
                    .map_err(|e| {
                        SandboxError::PathValidation(format!("write rule error: {e}"))
                    })?;
            }
            // Don't restrict_self here; just validate, then drop.
            drop(test_ruleset);
        }

        // Build the Command, applying Landlock in the child via `pre_exec`.
        let mut cmd = Command::new(command[0]);
        cmd.args(&command[1..])
            .current_dir(&spec.cwd)
            .envs(&spec.env)
            .stdin(if input.is_some() {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            })
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // SAFETY: `pre_exec` runs in the child after `fork`, before `execve`.
        // We must not allocate or call async code here.
        // Cloning Vecs is acceptable because this runs synchronously in the child.
        let pre_allow_read = allow_read.clone();
        let pre_allow_write = allow_write.clone();

        unsafe {
            cmd.pre_exec(move || {
                apply_landlock_restriction(&pre_allow_read, &pre_allow_write, abi)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::PermissionDenied, e))
            });
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| SandboxError::Exec(format!("failed to spawn '{}': {e}", command[0])))?;

        // Feed stdin if provided.
        if let (Some(mut stdin_handle), Some(data)) = (child.stdin.take(), input) {
            stdin_handle
                .write_all(data)
                .await
                .map_err(|e| SandboxError::Exec(format!("stdin write failed: {e}")))?;
        }

        let output = child
            .wait_with_output()
            .await
            .map_err(|e| SandboxError::Exec(format!("wait_with_output failed: {e}")))?;

        Ok(ExecResult {
            status: output.status.code().unwrap_or(-1),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    async fn stop(&self, handle: SandboxHandle) -> Result<(), SandboxError> {
        let removed = self.sessions.lock().await.remove(&handle.id);
        if removed.is_none() {
            warn!(sandbox_id = %handle.id, "LandlockBackend::stop called for unknown handle");
        }
        Ok(())
    }
}

/// Apply the Landlock ruleset to the current process.
///
/// This function is called inside the child process after `fork`, via
/// `pre_exec`.  It must not allocate heap memory or call async code.
fn apply_landlock_restriction(
    allow_read: &[PathBuf],
    allow_write: &[PathBuf],
    abi: ABI,
) -> Result<(), String> {
    let read_access = AccessFs::from_read(abi);
    let write_access = AccessFs::from_write(abi);
    let rw_access = read_access | write_access;

    let mut ruleset = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))
        .map_err(|e| format!("handle_access: {e}"))?
        .create()
        .map_err(|e| format!("create: {e}"))?;

    if !allow_read.is_empty() {
        ruleset = ruleset
            .add_rules(path_beneath_rules(allow_read, read_access))
            .map_err(|e| format!("add read rules: {e}"))?;
    }

    if !allow_write.is_empty() {
        ruleset = ruleset
            .add_rules(path_beneath_rules(allow_write, rw_access))
            .map_err(|e| format!("add write rules: {e}"))?;
    }

    let status = ruleset
        .restrict_self()
        .map_err(|e| format!("restrict_self: {e}"))?;

    match status.ruleset {
        RulesetStatus::FullyEnforced | RulesetStatus::PartiallyEnforced => Ok(()),
        RulesetStatus::NotEnforced => {
            Err("landlock not supported on this kernel".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_spec_for(allow_read: Vec<PathBuf>, allow_write: Vec<PathBuf>) -> SandboxSpec {
        SandboxSpec {
            cwd: std::path::PathBuf::from("/tmp"),
            allow_read,
            allow_write,
            allow_net: crate::spec::NetworkPolicy::None,
            env: Default::default(),
            image: None,
        }
    }

    fn landlock_available() -> bool {
        LandlockBackend::check_availability().is_ok()
    }

    /// Paths required to exec any dynamically-linked binary (e.g. `cat`).
    fn system_read_paths() -> Vec<PathBuf> {
        let mut paths = vec![
            PathBuf::from("/usr"),
            PathBuf::from("/lib"),
            PathBuf::from("/lib64"),
        ];
        // Some distributions symlink /bin -> /usr/bin; add /bin for completeness.
        if std::path::Path::new("/bin").exists() {
            paths.push(PathBuf::from("/bin"));
        }
        paths
    }

    #[tokio::test]
    async fn start_returns_handle() {
        if !landlock_available() {
            eprintln!("Skipping: Landlock not available");
            return;
        }
        let backend = LandlockBackend::new();
        let spec = make_spec_for(vec![PathBuf::from("/tmp")], vec![]);
        let handle = backend.start(spec).await.unwrap();
        assert!(!handle.id.is_empty());
        backend.stop(handle).await.unwrap();
    }

    #[tokio::test]
    async fn exec_allowed_read_succeeds() {
        if !landlock_available() {
            eprintln!("Skipping: Landlock not available");
            return;
        }
        // Create a known temp file that we are explicitly allowed to read.
        let mut f = tempfile::NamedTempFile::new_in("/tmp").unwrap();
        f.write_all(b"hello landlock\n").unwrap();
        let file_path = f.path().to_path_buf();

        // Allow /tmp + system paths required to exec `cat`.
        let mut allow_read = system_read_paths();
        allow_read.push(PathBuf::from("/tmp"));

        let backend = LandlockBackend::new();
        let spec = make_spec_for(allow_read, vec![]);
        let handle = backend.start(spec).await.unwrap();

        let file_str = file_path.to_string_lossy().to_string();
        let result = backend
            .exec(&handle, &["cat", &file_str], None)
            .await
            .unwrap();

        assert_eq!(result.status, 0, "cat of allowed file should succeed");
        assert_eq!(result.stdout, b"hello landlock\n");

        backend.stop(handle).await.unwrap();
    }

    #[tokio::test]
    async fn exec_denied_path_fails() {
        if !landlock_available() {
            eprintln!("Skipping: Landlock not available");
            return;
        }
        // Allow /tmp + system paths but NOT /etc.
        let mut allow_read = system_read_paths();
        allow_read.push(PathBuf::from("/tmp"));

        let backend = LandlockBackend::new();
        let spec = make_spec_for(allow_read, vec![]);
        let handle = backend.start(spec).await.unwrap();

        let result = backend
            .exec(&handle, &["cat", "/etc/hostname"], None)
            .await
            .unwrap();

        // cat should exit non-zero when Landlock denies access to /etc/hostname.
        assert_ne!(result.status, 0, "reading /etc/hostname should be denied");

        backend.stop(handle).await.unwrap();
    }

    #[tokio::test]
    async fn start_rejects_sensitive_mount_path() {
        let backend = LandlockBackend::new();
        let spec = make_spec_for(vec![PathBuf::from("/etc")], vec![]);
        let result = backend.start(spec).await;
        assert!(
            matches!(result, Err(SandboxError::PathValidation(_))),
            "expected PathValidation error, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn start_rejects_sensitive_cwd() {
        let backend = LandlockBackend::new();
        let spec = SandboxSpec {
            cwd: PathBuf::from("/etc"),
            allow_read: vec![],
            allow_write: vec![],
            allow_net: crate::spec::NetworkPolicy::None,
            env: Default::default(),
            image: None,
        };
        let result = backend.start(spec).await;
        assert!(
            matches!(result, Err(SandboxError::PathValidation(_))),
            "expected PathValidation error for sensitive cwd, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn stop_unknown_handle_is_ok() {
        let backend = LandlockBackend::new();
        let handle = SandboxHandle {
            id: "nonexistent-id".into(),
            inner: HandleInner::Landlock { pid: None },
        };
        let _ = backend.stop(handle).await;
    }
}
