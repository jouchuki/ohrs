//! Bridge subsystem: spawn child ohrs sessions, capture output, manage work secrets.

use oh_types::bridge::{BridgeConfig, BridgeSessionRecord, WorkSecret};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{oneshot, RwLock};
use tokio::task::JoinHandle;
use uuid::Uuid;

const OUTPUT_BUF_CAP: usize = 1000;

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("session not found: {0}")]
    NotFound(String),
    #[error("spawn failed: {0}")]
    SpawnError(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct BridgeSessionState {
    pub record: BridgeSessionRecord,
    pub output_buf: VecDeque<String>,
    pub child_handle: Option<JoinHandle<()>>,
    pub kill_tx: Option<oneshot::Sender<()>>,
}

pub struct BridgeManager {
    config: BridgeConfig,
    sessions: Arc<RwLock<HashMap<String, BridgeSessionState>>>,
}

impl BridgeManager {
    pub fn new(config: BridgeConfig) -> Self {
        Self {
            config,
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Spawn a new ohrs child session with the given prompt.
    pub async fn spawn_session(
        &self,
        prompt: &str,
        cwd: &Path,
    ) -> Result<BridgeSessionRecord, BridgeError> {
        let ohrs_bin = std::env::current_exe()
            .ok()
            .unwrap_or_else(|| std::path::PathBuf::from("ohrs"));
        self.spawn_command_internal(
            &[ohrs_bin.to_string_lossy().as_ref(), "-p", prompt],
            cwd,
        )
        .await
    }

    /// Testable variant: accepts an explicit argv slice.
    pub(crate) async fn spawn_command(
        &self,
        argv: &[&str],
        cwd: &Path,
    ) -> Result<BridgeSessionRecord, BridgeError> {
        self.spawn_command_internal(argv, cwd).await
    }

    async fn spawn_command_internal(
        &self,
        argv: &[&str],
        cwd: &Path,
    ) -> Result<BridgeSessionRecord, BridgeError> {
        if argv.is_empty() {
            return Err(BridgeError::SpawnError("empty argv".into()));
        }

        let session_id = Uuid::new_v4().to_string();
        let command_str = argv.join(" ");
        let cwd_str = cwd.to_string_lossy().into_owned();

        let mut cmd = tokio::process::Command::new(argv[0]);
        cmd.args(&argv[1..])
            .current_dir(cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| BridgeError::SpawnError(e.to_string()))?;

        let pid = child.id().unwrap_or(0);
        let started_at = now();

        let stdout = child.stdout.take().map(BufReader::new);
        let stderr = child.stderr.take().map(BufReader::new);

        let record = BridgeSessionRecord {
            session_id: session_id.clone(),
            command: command_str,
            cwd: cwd_str,
            pid,
            status: "running".into(),
            started_at,
            output_path: self.output_path(&session_id),
        };

        let (kill_tx, kill_rx) = oneshot::channel::<()>();

        let sessions = Arc::clone(&self.sessions);
        let sid = session_id.clone();

        let handle = tokio::spawn(async move {
            // Collect lines from stdout and stderr concurrently.
            let stdout_lines = async {
                if let Some(rdr) = stdout {
                    let mut lines = rdr.lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        push_line(&sessions, &sid, line).await;
                    }
                }
            };
            let stderr_lines = async {
                if let Some(rdr) = stderr {
                    let mut lines = rdr.lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        push_line(&sessions, &sid, line).await;
                    }
                }
            };

            tokio::select! {
                _ = async { tokio::join!(stdout_lines, stderr_lines) } => {},
                _ = kill_rx => {
                    // kill_tx was dropped or sent: child will be reaped below
                },
            }

            // Wait for child exit, update status.
            match child.wait().await {
                Ok(status) => {
                    let new_status = if status.success() {
                        "completed"
                    } else {
                        "failed"
                    };
                    let mut map = sessions.write().await;
                    if let Some(state) = map.get_mut(&sid) {
                        state.record.status = new_status.into();
                    }
                }
                Err(_) => {
                    let mut map = sessions.write().await;
                    if let Some(state) = map.get_mut(&sid) {
                        state.record.status = "failed".into();
                    }
                }
            }
        });

        let state = BridgeSessionState {
            record: record.clone(),
            output_buf: VecDeque::new(),
            child_handle: Some(handle),
            kill_tx: Some(kill_tx),
        };

        self.sessions
            .write()
            .await
            .insert(session_id, state);

        Ok(record)
    }

    pub async fn list_sessions(&self) -> Vec<BridgeSessionRecord> {
        let map = self.sessions.read().await;
        let mut records: Vec<BridgeSessionRecord> =
            map.values().map(|s| s.record.clone()).collect();
        // Most recent first.
        records.sort_by(|a, b| b.started_at.partial_cmp(&a.started_at).unwrap());
        records
    }

    pub async fn get_output(
        &self,
        id: &str,
        max_lines: usize,
    ) -> Result<Vec<String>, BridgeError> {
        let map = self.sessions.read().await;
        let state = map.get(id).ok_or_else(|| BridgeError::NotFound(id.into()))?;
        let buf = &state.output_buf;
        let skip = buf.len().saturating_sub(max_lines);
        Ok(buf.iter().skip(skip).cloned().collect())
    }

    pub async fn stop_session(&self, id: &str) -> Result<(), BridgeError> {
        let mut map = self.sessions.write().await;
        let state = map
            .get_mut(id)
            .ok_or_else(|| BridgeError::NotFound(id.into()))?;

        // Signal the watcher task to stop.
        if let Some(tx) = state.kill_tx.take() {
            let _ = tx.send(());
        }
        // Abort the watcher task.
        if let Some(handle) = state.child_handle.take() {
            handle.abort();
        }
        state.record.status = "killed".into();
        Ok(())
    }

    fn output_path(&self, session_id: &str) -> String {
        format!("{}/bridge/{}.log", self.config.dir, session_id)
    }
}

/// Push a line into the session's ring buffer.
async fn push_line(
    sessions: &Arc<RwLock<HashMap<String, BridgeSessionState>>>,
    sid: &str,
    line: String,
) {
    let mut map = sessions.write().await;
    if let Some(state) = map.get_mut(sid) {
        if state.output_buf.len() >= OUTPUT_BUF_CAP {
            state.output_buf.pop_front();
        }
        state.output_buf.push_back(line);
    }
}

// ── Work secret ───────────────────────────────────────────────────────────────

/// Generate a work-secret token: 32 random bytes encoded as 64 lowercase hex chars.
pub fn generate_work_secret() -> WorkSecret {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("getrandom failed");
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    WorkSecret {
        version: 1,
        session_ingress_token: token,
        api_base_url: String::new(),
    }
}

/// Constant-time comparison: true iff provided matches expected token.
pub fn validate_work_secret(provided: &str, expected: &WorkSecret) -> bool {
    let a = provided.as_bytes();
    let b = expected.session_ingress_token.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    // XOR every byte; accumulate differences.
    let diff = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    diff == 0
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_config() -> BridgeConfig {
        BridgeConfig {
            dir: std::env::temp_dir().to_string_lossy().into_owned(),
            machine_name: "test".into(),
            max_sessions: 4,
            verbose: false,
            session_timeout_ms: oh_types::bridge::DEFAULT_SESSION_TIMEOUT_MS,
        }
    }

    #[tokio::test]
    async fn test_spawn_echo_and_get_output() {
        let mgr = BridgeManager::new(test_config());
        let cwd = PathBuf::from(std::env::temp_dir());

        let record = mgr
            .spawn_command(&["echo", "hello bridge"], &cwd)
            .await
            .expect("spawn failed");

        assert_eq!(record.status, "running");
        assert!(!record.session_id.is_empty());

        // Wait for the child to finish.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let lines = mgr
            .get_output(&record.session_id, 100)
            .await
            .expect("get_output failed");

        assert!(
            lines.iter().any(|l| l.contains("hello bridge")),
            "expected 'hello bridge' in output, got: {:?}",
            lines
        );
    }

    #[tokio::test]
    async fn test_list_sessions() {
        let mgr = BridgeManager::new(test_config());
        let cwd = PathBuf::from(std::env::temp_dir());

        mgr.spawn_command(&["echo", "a"], &cwd)
            .await
            .expect("spawn a");
        mgr.spawn_command(&["echo", "b"], &cwd)
            .await
            .expect("spawn b");

        let sessions = mgr.list_sessions().await;
        assert_eq!(sessions.len(), 2);
    }

    #[tokio::test]
    async fn test_stop_session() {
        let mgr = BridgeManager::new(test_config());
        let cwd = PathBuf::from(std::env::temp_dir());

        // Use sleep so the process is still alive when we stop it.
        let record = mgr
            .spawn_command(&["sleep", "30"], &cwd)
            .await
            .expect("spawn sleep");

        mgr.stop_session(&record.session_id)
            .await
            .expect("stop_session failed");

        let sessions = mgr.list_sessions().await;
        let stopped = sessions
            .iter()
            .find(|r| r.session_id == record.session_id)
            .expect("session missing");
        assert_eq!(stopped.status, "killed");
    }

    #[tokio::test]
    async fn test_stop_unknown_session() {
        let mgr = BridgeManager::new(test_config());
        let err = mgr.stop_session("does-not-exist").await;
        assert!(matches!(err, Err(BridgeError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_get_output_unknown_session() {
        let mgr = BridgeManager::new(test_config());
        let err = mgr.get_output("missing", 10).await;
        assert!(matches!(err, Err(BridgeError::NotFound(_))));
    }

    #[test]
    fn test_generate_work_secret_length() {
        let ws = generate_work_secret();
        assert_eq!(ws.session_ingress_token.len(), 64);
        // All chars must be hex.
        assert!(ws.session_ingress_token.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_generate_work_secret_unique() {
        let a = generate_work_secret();
        let b = generate_work_secret();
        assert_ne!(a.session_ingress_token, b.session_ingress_token);
    }

    #[test]
    fn test_validate_work_secret_match() {
        let ws = generate_work_secret();
        assert!(validate_work_secret(&ws.session_ingress_token, &ws));
    }

    #[test]
    fn test_validate_work_secret_mismatch() {
        let ws = generate_work_secret();
        assert!(!validate_work_secret("wrong_token", &ws));
    }

    #[test]
    fn test_validate_work_secret_same_length_mismatch() {
        let ws = generate_work_secret();
        // Flip last char to get a same-length but different token.
        let mut bad = ws.session_ingress_token.clone();
        let last = bad.pop().unwrap();
        bad.push(if last == 'a' { 'b' } else { 'a' });
        assert!(!validate_work_secret(&bad, &ws));
    }
}
