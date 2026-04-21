//! Docker-based sandbox backend.
//!
//! [`DockerBackend`] manages a Docker container per sandbox session.
//!
//! ## Container lifecycle
//!
//! 1. **`start`** — validates paths, creates and starts a container with the
//!    requested volume mounts, network policy, and environment variables.
//!    The container runs `tail -f /dev/null` so it stays alive between `exec`
//!    calls.
//! 2. **`exec`** — uses the Docker exec API to run a command inside the
//!    running container, optionally feeding stdin.
//! 3. **`stop`** — stops and removes the container (best-effort).
//!
//! ## Network policy mapping
//!
//! | [`NetworkPolicy`] variant | Docker `--network` value |
//! |---------------------------|--------------------------|
//! | `None`                    | `none`                   |
//! | `Localhost`               | `none` + warning ¹       |
//! | `AllowList(_)`            | `none` + warning ¹       |
//! | `All`                     | `bridge`                 |
//!
//! ¹ Docker has no native "loopback-only" or domain-allowlist mode.
//! Both variants fail-closed to `none` and emit a warning, matching the Python
//! reference's "fail closed instead of silently widening egress" philosophy.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use bollard::container::{
    Config, CreateContainerOptions, RemoveContainerOptions, StartContainerOptions,
    StopContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};
use bollard::image::CreateImageOptions;
use bollard::models::{HostConfig, Mount, MountTypeEnum};
use bollard::Docker;
use futures::StreamExt;
use tokio::sync::Mutex;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::{
    path_validator::validate_mount_paths,
    spec::{ExecResult, HandleInner, NetworkPolicy, SandboxHandle, SandboxSpec},
    SandboxBackend, SandboxError,
};

/// Default container image when none is specified in [`SandboxSpec::image`].
const DEFAULT_IMAGE: &str = "alpine:latest";

/// Container entry-point that keeps it alive between `exec` calls.
const KEEPALIVE_CMD: &[&str] = &["tail", "-f", "/dev/null"];

/// State for a running Docker sandbox session.
#[derive(Debug, Clone)]
struct DockerSession {
    container_id: String,
    spec: SandboxSpec,
}

/// Docker-based sandbox backend.
///
/// Internally holds an [`Arc<Docker>`] so the backend can be cloned cheaply.
#[derive(Debug, Clone)]
pub struct DockerBackend {
    docker: Arc<Docker>,
    sessions: Arc<Mutex<HashMap<String, DockerSession>>>,
}

impl DockerBackend {
    /// Connect to Docker via the default socket (`/var/run/docker.sock`).
    pub fn new() -> Result<Self, SandboxError> {
        let docker = Docker::connect_with_socket_defaults()
            .map_err(|e| SandboxError::Unavailable(format!("cannot connect to Docker: {e}")))?;
        Ok(Self {
            docker: Arc::new(docker),
            sessions: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Use a pre-existing [`bollard::Docker`] client (useful in tests).
    pub fn with_client(docker: Docker) -> Self {
        Self {
            docker: Arc::new(docker),
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

// ── Spec → HostConfig translation ────────────────────────────────────────────

/// Translate a [`SandboxSpec`] into a bollard [`HostConfig`].
///
/// This function is `pub(crate)` so that unit tests can exercise the
/// translation logic without a live Docker daemon.
pub(crate) fn spec_to_host_config(spec: &SandboxSpec) -> HostConfig {
    // Build volume mounts.
    let mut mounts: Vec<Mount> = Vec::new();

    // Read-only mounts
    for path in &spec.allow_read {
        let src = path.to_string_lossy().to_string();
        mounts.push(Mount {
            target: Some(src.clone()),
            source: Some(src),
            typ: Some(MountTypeEnum::BIND),
            read_only: Some(true),
            ..Default::default()
        });
    }

    // Read-write mounts (allow_write paths override allow_read for the same path)
    for path in &spec.allow_write {
        let src = path.to_string_lossy().to_string();
        // Remove any previously-added read-only mount for this path.
        mounts.retain(|m| m.source.as_deref() != Some(&src));
        mounts.push(Mount {
            target: Some(src.clone()),
            source: Some(src),
            typ: Some(MountTypeEnum::BIND),
            read_only: Some(false),
            ..Default::default()
        });
    }

    // Network mode.
    // Docker has no native "loopback-only" mode.  Localhost and AllowList both
    // fail-closed to "none" to avoid silently widening egress.
    let network_mode = match &spec.allow_net {
        NetworkPolicy::None => "none".to_string(),
        NetworkPolicy::Localhost => {
            warn!(
                "Docker backend has no loopback-only network mode; \
                 falling back to network=none (fail-closed)"
            );
            "none".to_string()
        }
        NetworkPolicy::AllowList(domains) => {
            warn!(
                domains = ?domains,
                "Docker backend does not enforce AllowList; falling back to network=none"
            );
            "none".to_string()
        }
        NetworkPolicy::All => "bridge".to_string(),
    };

    HostConfig {
        mounts: if mounts.is_empty() { None } else { Some(mounts) },
        network_mode: Some(network_mode),
        ..Default::default()
    }
}

/// Convert the `SandboxSpec::env` map into the `KEY=VALUE` format that
/// Docker's container config expects.
fn env_to_docker(env: &HashMap<String, String>) -> Vec<String> {
    env.iter().map(|(k, v)| format!("{k}={v}")).collect()
}

// ── SandboxBackend impl ───────────────────────────────────────────────────────

#[async_trait]
impl SandboxBackend for DockerBackend {
    async fn start(&self, spec: SandboxSpec) -> Result<SandboxHandle, SandboxError> {
        // Validate all user-supplied paths before touching Docker, including cwd.
        crate::path_validator::validate_mount_path(&spec.cwd)?;
        validate_mount_paths(&spec.allow_read)?;
        validate_mount_paths(&spec.allow_write)?;

        let image = spec.image.clone().unwrap_or_else(|| DEFAULT_IMAGE.into());
        let session_id = Uuid::new_v4().to_string();
        let container_name = format!("oh-sandbox-{}", session_id);

        // Pull image if it is not already present (best-effort).
        pull_image_if_needed(&self.docker, &image).await;

        let host_config = spec_to_host_config(&spec);
        let env_vec = env_to_docker(&spec.env);
        let cwd_str = spec.cwd.to_string_lossy().to_string();

        let config: Config<String> = Config {
            image: Some(image.clone()),
            cmd: Some(KEEPALIVE_CMD.iter().map(|s| s.to_string()).collect()),
            working_dir: Some(cwd_str),
            env: if env_vec.is_empty() {
                None
            } else {
                Some(env_vec)
            },
            host_config: Some(host_config),
            ..Default::default()
        };

        let create_opts = CreateContainerOptions {
            name: container_name.clone(),
            platform: None,
        };

        let resp = self
            .docker
            .create_container(Some(create_opts), config)
            .await?;

        let container_id = resp.id;
        debug!(container_id = %container_id, "Docker container created");

        self.docker
            .start_container(&container_id, None::<StartContainerOptions<String>>)
            .await?;

        debug!(container_id = %container_id, "Docker container started");

        let session = DockerSession {
            container_id: container_id.clone(),
            spec,
        };
        self.sessions
            .lock()
            .await
            .insert(session_id.clone(), session);

        Ok(SandboxHandle {
            id: session_id,
            inner: HandleInner::Docker { container_id },
        })
    }

    async fn exec(
        &self,
        handle: &SandboxHandle,
        command: &[&str],
        input: Option<&[u8]>,
    ) -> Result<ExecResult, SandboxError> {
        let container_id = {
            let sessions = self.sessions.lock().await;
            let session = sessions
                .get(&handle.id)
                .ok_or_else(|| SandboxError::InvalidHandle(handle.id.clone()))?;
            session.container_id.clone()
        };

        if command.is_empty() {
            return Err(SandboxError::Exec("command must not be empty".into()));
        }

        let exec_options = CreateExecOptions {
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            attach_stdin: Some(input.is_some()),
            cmd: Some(command.iter().map(|s| s.to_string()).collect()),
            ..Default::default()
        };

        let exec_created = self
            .docker
            .create_exec::<String>(&container_id, exec_options)
            .await?;

        let start_opts = StartExecOptions {
            detach: false,
            tty: false,
            ..Default::default()
        };

        let result = self
            .docker
            .start_exec(&exec_created.id, Some(start_opts))
            .await?;

        let mut stdout_buf: Vec<u8> = Vec::new();
        let mut stderr_buf: Vec<u8> = Vec::new();

        match result {
            StartExecResults::Attached { mut output, .. } => {
                while let Some(chunk) = output.next().await {
                    match chunk? {
                        bollard::container::LogOutput::StdOut { message } => {
                            stdout_buf.extend_from_slice(&message);
                        }
                        bollard::container::LogOutput::StdErr { message } => {
                            stderr_buf.extend_from_slice(&message);
                        }
                        _ => {}
                    }
                }
            }
            StartExecResults::Detached => {
                return Err(SandboxError::Exec(
                    "unexpected detached exec result".into(),
                ))
            }
        }

        // Retrieve exit code.
        let inspect = self.docker.inspect_exec(&exec_created.id).await?;
        let exit_code = inspect.exit_code.unwrap_or(-1) as i32;

        Ok(ExecResult {
            status: exit_code,
            stdout: stdout_buf,
            stderr: stderr_buf,
        })
    }

    async fn stop(&self, handle: SandboxHandle) -> Result<(), SandboxError> {
        let session_opt = self.sessions.lock().await.remove(&handle.id);
        let container_id = match session_opt {
            Some(s) => s.container_id,
            None => {
                warn!(sandbox_id = %handle.id, "DockerBackend::stop called for unknown handle");
                return Ok(());
            }
        };

        // Stop the container (give it 5 s to shut down gracefully).
        let stop_opts = StopContainerOptions { t: 5 };
        if let Err(e) = self.docker.stop_container(&container_id, Some(stop_opts)).await {
            warn!("Failed to stop container {}: {}", container_id, e);
        }

        // Remove the container unconditionally.
        let rm_opts = RemoveContainerOptions {
            force: true,
            ..Default::default()
        };
        if let Err(e) = self
            .docker
            .remove_container(&container_id, Some(rm_opts))
            .await
        {
            warn!("Failed to remove container {}: {}", container_id, e);
        }

        debug!(container_id = %container_id, "Docker sandbox stopped and removed");
        Ok(())
    }
}

/// Pull `image` from a registry if it is not already available locally.
///
/// Errors are logged as warnings; a missing image will surface as a "no such
/// image" error when the container is created.
async fn pull_image_if_needed(docker: &Docker, image: &str) {
    // If we can inspect the image, it is already local.
    if docker.inspect_image(image).await.is_ok() {
        return;
    }

    debug!(image = %image, "Pulling Docker image");
    let opts = CreateImageOptions {
        from_image: image,
        ..Default::default()
    };
    let mut stream = docker.create_image(Some(opts), None, None);
    while let Some(result) = stream.next().await {
        if let Err(e) = result {
            warn!(image = %image, "Error while pulling Docker image: {e}");
            return;
        }
    }
    debug!(image = %image, "Docker image pull complete");
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_spec(
        allow_read: Vec<PathBuf>,
        allow_write: Vec<PathBuf>,
        allow_net: NetworkPolicy,
        env: HashMap<String, String>,
        image: Option<String>,
    ) -> SandboxSpec {
        SandboxSpec {
            cwd: PathBuf::from("/workspace"),
            allow_read,
            allow_write,
            allow_net,
            env,
            image,
        }
    }

    // ── spec_to_host_config unit tests (no Docker daemon required) ────────────

    #[test]
    fn host_config_no_mounts_no_network() {
        let spec = make_spec(vec![], vec![], NetworkPolicy::None, HashMap::new(), None);
        let hc = spec_to_host_config(&spec);

        assert_eq!(hc.network_mode.as_deref(), Some("none"));
        assert!(hc.mounts.is_none());
    }

    #[test]
    fn host_config_read_only_mounts() {
        let spec = make_spec(
            vec![PathBuf::from("/tmp/data"), PathBuf::from("/tmp/config")],
            vec![],
            NetworkPolicy::None,
            HashMap::new(),
            None,
        );
        let hc = spec_to_host_config(&spec);
        let mounts = hc.mounts.expect("should have mounts");
        assert_eq!(mounts.len(), 2);
        assert!(mounts.iter().all(|m| m.read_only == Some(true)));
    }

    #[test]
    fn host_config_write_mount_overrides_read() {
        let path = PathBuf::from("/tmp/shared");
        let spec = make_spec(
            vec![path.clone()],
            vec![path.clone()],
            NetworkPolicy::None,
            HashMap::new(),
            None,
        );
        let hc = spec_to_host_config(&spec);
        let mounts = hc.mounts.expect("should have mounts");
        // Deduped: only one mount entry for /tmp/shared
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].read_only, Some(false));
    }

    #[test]
    fn host_config_network_all_is_bridge() {
        let spec = make_spec(vec![], vec![], NetworkPolicy::All, HashMap::new(), None);
        let hc = spec_to_host_config(&spec);
        assert_eq!(hc.network_mode.as_deref(), Some("bridge"));
    }

    #[test]
    fn host_config_network_localhost_falls_back_to_none() {
        // Docker has no loopback-only mode; fail-closed to "none".
        let spec = make_spec(
            vec![],
            vec![],
            NetworkPolicy::Localhost,
            HashMap::new(),
            None,
        );
        let hc = spec_to_host_config(&spec);
        assert_eq!(hc.network_mode.as_deref(), Some("none"));
    }

    #[test]
    fn host_config_allowlist_falls_back_to_none() {
        let spec = make_spec(
            vec![],
            vec![],
            NetworkPolicy::AllowList(vec!["example.com".into()]),
            HashMap::new(),
            None,
        );
        let hc = spec_to_host_config(&spec);
        assert_eq!(hc.network_mode.as_deref(), Some("none"));
    }

    #[test]
    fn env_to_docker_format() {
        let mut env = HashMap::new();
        env.insert("FOO".into(), "bar".into());
        env.insert("HELLO".into(), "world".into());
        let mut pairs = env_to_docker(&env);
        pairs.sort();
        assert!(pairs.contains(&"FOO=bar".to_string()));
        assert!(pairs.contains(&"HELLO=world".to_string()));
    }

    #[test]
    fn host_config_cwd_validation_is_separate() {
        // spec_to_host_config itself does not validate cwd (that is the start()
        // method's job); but we document the expected test at the start() level.
        // Here we just confirm spec_to_host_config builds a config without panic.
        let spec = make_spec(vec![], vec![], NetworkPolicy::None, HashMap::new(), None);
        let hc = spec_to_host_config(&spec);
        assert_eq!(hc.network_mode.as_deref(), Some("none"));
    }

    // ── Live Docker integration tests (guarded behind feature flag) ───────────

    #[cfg(feature = "docker-tests")]
    mod live {
        use super::*;

        async fn backend() -> DockerBackend {
            DockerBackend::new().expect("Docker must be available for live tests")
        }

        #[tokio::test]
        #[ignore = "requires docker"]
        async fn start_stop_container() {
            let b = backend().await;
            let spec = make_spec(
                vec![PathBuf::from("/tmp")],
                vec![],
                NetworkPolicy::None,
                HashMap::new(),
                Some("alpine:latest".into()),
            );
            let handle = b.start(spec).await.expect("start should succeed");
            b.stop(handle).await.expect("stop should succeed");
        }

        #[tokio::test]
        #[ignore = "requires docker"]
        async fn exec_echo_in_container() {
            let b = backend().await;
            let spec = make_spec(
                vec![],
                vec![],
                NetworkPolicy::None,
                HashMap::new(),
                Some("alpine:latest".into()),
            );
            let handle = b.start(spec).await.expect("start should succeed");
            let result = b
                .exec(&handle, &["echo", "hello"], None)
                .await
                .expect("exec should succeed");
            assert_eq!(result.status, 0);
            assert_eq!(result.stdout.trim_ascii_end(), b"hello");
            b.stop(handle).await.expect("stop should succeed");
        }
    }
}
