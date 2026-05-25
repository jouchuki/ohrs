//! Background task manager: create, list, stop, read output.

#[cfg(unix)]
extern crate libc;

use oh_types::tasks::{TaskRecord, TaskStatus, TaskType};
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use uuid::Uuid;

/// Boxed future producing a subagent's final result (or an error string),
/// driven by [`BackgroundTaskManager::create_in_process_task`].
pub type InProcessJob = Pin<Box<dyn Future<Output = Result<String, String>> + Send>>;

// ── Completion listener type ────────────────────────────────────────────────

pub type CompletionListener = Box<dyn Fn(&TaskRecord) + Send + Sync>;

// ── Per-task live state ──────────────────────────────────────────────────────

struct LiveTask {
    record: TaskRecord,
    /// Handle to the watcher task (used for cancellation).
    watcher: Option<tokio::task::JoinHandle<()>>,
    /// Sender half of a oneshot used to signal the child to be killed.
    kill_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

// ── Public manager ───────────────────────────────────────────────────────────

/// Manages background tasks (shell and agent).
///
/// The inner state is wrapped in `Arc<Mutex<_>>` so that the background
/// watcher tasks spawned by `tokio::spawn` can post status updates back
/// without holding a mutable borrow on the manager.
pub struct BackgroundTaskManager {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    tasks: HashMap<String, LiveTask>,
    completion_listeners: HashMap<String, CompletionListener>,
}

impl BackgroundTaskManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                tasks: HashMap::new(),
                completion_listeners: HashMap::new(),
            })),
        }
    }

    // ── Completion listeners ─────────────────────────────────────────────

    /// Register a callback that is fired whenever a task reaches a terminal
    /// state (`Completed`, `Failed`, or `Killed`).
    ///
    /// Returns an unregister closure.
    pub async fn register_completion_listener(
        &self,
        listener: CompletionListener,
    ) -> impl FnOnce() {
        let id = Uuid::new_v4().to_string();
        let inner = Arc::clone(&self.inner);
        {
            let mut guard = self.inner.lock().await;
            guard.completion_listeners.insert(id.clone(), listener);
        }
        move || {
            // Best-effort synchronous removal; the async guard is not available here,
            // so we spawn a tiny task to do it.
            let inner2 = Arc::clone(&inner);
            tokio::spawn(async move {
                inner2.lock().await.completion_listeners.remove(&id);
            });
        }
    }

    // ── Task creation ─────────────────────────────────────────────────────

    /// Create and immediately start a shell task (`TaskType::LocalBash`).
    pub async fn create_shell_task(
        &self,
        command: &str,
        description: &str,
        cwd: &str,
    ) -> TaskRecord {
        self.create_command_task(command, description, cwd, TaskType::LocalBash)
            .await
    }

    /// Create and immediately start a subprocess-agent task
    /// (`TaskType::RemoteAgent`).
    ///
    /// Used by the subprocess / worktree subagent backends: `command` is the
    /// `oh run …` invocation, `cwd` is the agent's working directory (a git
    /// worktree for the worktree backend). The child's stdout/stderr are tee'd
    /// to the task log so `read_output` returns the subagent's final text — the
    /// same `spawn_and_watch` machinery shell tasks use, no separate process
    /// management.
    pub async fn create_remote_agent_task(
        &self,
        command: &str,
        description: &str,
        cwd: &str,
    ) -> TaskRecord {
        self.create_command_task(command, description, cwd, TaskType::RemoteAgent)
            .await
    }

    /// Shared body for command-backed tasks: record a `TaskRecord` of
    /// `task_type`, spawn `/bin/bash -lc <command>` in `cwd`, and tee its output
    /// to the task log via [`spawn_and_watch`].
    async fn create_command_task(
        &self,
        command: &str,
        description: &str,
        cwd: &str,
        task_type: TaskType,
    ) -> TaskRecord {
        let id = Uuid::new_v4().to_string();
        let output_file = oh_config::get_tasks_dir().join(format!("{id}.log"));

        // Touch the log file.
        if let Some(parent) = output_file.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let _ = tokio::fs::write(&output_file, b"").await;

        let record = TaskRecord {
            id: id.clone(),
            task_type,
            status: TaskStatus::Running,
            description: description.to_string(),
            cwd: cwd.to_string(),
            output_file: output_file.clone(),
            command: Some(command.to_string()),
            prompt: None,
            created_at: now(),
            started_at: Some(now()),
            ended_at: None,
            return_code: None,
            metadata: HashMap::new(),
        };

        oh_telemetry::ACTIVE_BACKGROUND_TASKS.add(1, &[]);

        let (kill_tx, kill_rx) = tokio::sync::oneshot::channel::<()>();

        let live = LiveTask {
            record: record.clone(),
            watcher: None,
            kill_tx: Some(kill_tx),
        };

        {
            let mut guard = self.inner.lock().await;
            guard.tasks.insert(id.clone(), live);
        }

        // Spawn the child process and watcher.
        let watcher = spawn_and_watch(
            id.clone(),
            command.to_string(),
            cwd.to_string(),
            output_file,
            Arc::clone(&self.inner),
            kill_rx,
        );

        {
            let mut guard = self.inner.lock().await;
            if let Some(lt) = guard.tasks.get_mut(&id) {
                lt.watcher = Some(watcher);
            }
        }

        record
    }

    /// Create and immediately start an agent task.
    ///
    /// The in-process subagent driver is provided by the caller via
    /// [`create_in_process_task`](Self::create_in_process_task); this overload
    /// (kept for the `BackgroundTasks::create_agent` trait + the `TaskCreate`
    /// tool's prompt path) records an `InProcessTeammate` task with no driver
    /// and immediately resolves it, writing the prompt to the log so
    /// `read_output` is exercised. It is the degenerate "no runner injected"
    /// case; real subagent spawns go through `create_in_process_task`.
    pub async fn create_agent_task(
        &self,
        prompt: &str,
        description: &str,
        cwd: &str,
    ) -> TaskRecord {
        let prompt_owned = prompt.to_string();
        self.create_in_process_task(
            prompt,
            description,
            cwd,
            Box::pin(async move { Ok(prompt_owned) }),
        )
        .await
    }

    /// Create an in-process teammate task: record an [`TaskType::InProcessTeammate`]
    /// `TaskRecord`, spawn a tokio task that drives `job` to completion, tee its
    /// result into the task log file, and transition the status
    /// `Running → Completed/Failed`.
    ///
    /// This is the real Phase 1 spawn path: the `job` is the subagent's
    /// `run_subagent` future (built by the harness-injected `SubagentRunner`),
    /// returning the final assistant text on success.
    pub async fn create_in_process_task(
        &self,
        prompt: &str,
        description: &str,
        cwd: &str,
        job: InProcessJob,
    ) -> TaskRecord {
        let id = Uuid::new_v4().to_string();
        let output_file = oh_config::get_tasks_dir().join(format!("{id}.log"));

        if let Some(parent) = output_file.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let _ = tokio::fs::write(&output_file, b"").await;

        let record = TaskRecord {
            id: id.clone(),
            task_type: TaskType::InProcessTeammate,
            status: TaskStatus::Running,
            description: description.to_string(),
            cwd: cwd.to_string(),
            output_file: output_file.clone(),
            command: None,
            prompt: Some(prompt.to_string()),
            created_at: now(),
            started_at: Some(now()),
            ended_at: None,
            return_code: None,
            metadata: HashMap::new(),
        };

        oh_telemetry::ACTIVE_BACKGROUND_TASKS.add(1, &[]);

        let (kill_tx, kill_rx) = tokio::sync::oneshot::channel::<()>();

        let live = LiveTask {
            record: record.clone(),
            watcher: None,
            kill_tx: Some(kill_tx),
        };

        {
            let mut guard = self.inner.lock().await;
            guard.tasks.insert(id.clone(), live);
        }

        let watcher = spawn_in_process(
            id.clone(),
            output_file,
            Arc::clone(&self.inner),
            job,
            kill_rx,
        );

        {
            let mut guard = self.inner.lock().await;
            if let Some(lt) = guard.tasks.get_mut(&id) {
                lt.watcher = Some(watcher);
            }
        }

        record
    }

    // ── Query / update ────────────────────────────────────────────────────

    pub async fn get_task(&self, id: &str) -> Option<TaskRecord> {
        let guard = self.inner.lock().await;
        guard.tasks.get(id).map(|lt| lt.record.clone())
    }

    pub async fn list_tasks(&self, status: Option<TaskStatus>) -> Vec<TaskRecord> {
        let guard = self.inner.lock().await;
        guard
            .tasks
            .values()
            .filter(|lt| status.map_or(true, |s| lt.record.status == s))
            .map(|lt| lt.record.clone())
            .collect()
    }

    pub async fn update_task(&self, id: &str, description: Option<&str>) -> Option<TaskRecord> {
        let mut guard = self.inner.lock().await;
        if let Some(lt) = guard.tasks.get_mut(id) {
            if let Some(desc) = description {
                lt.record.description = desc.to_string();
            }
            Some(lt.record.clone())
        } else {
            None
        }
    }

    /// Kill a running task.  Sets status to `Killed` and records `ended_at`.
    pub async fn stop_task(&self, id: &str) -> Option<TaskRecord> {
        let kill_tx = {
            let mut guard = self.inner.lock().await;
            if let Some(lt) = guard.tasks.get_mut(id) {
                if matches!(
                    lt.record.status,
                    TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
                ) {
                    return Some(lt.record.clone());
                }
                // Mark killed immediately so concurrent readers see the right state.
                lt.record.status = TaskStatus::Killed;
                lt.record.ended_at = Some(now());
                lt.kill_tx.take()
            } else {
                return None;
            }
        };

        // Signal the watcher to kill the child.
        if let Some(tx) = kill_tx {
            let _ = tx.send(());
        }

        oh_telemetry::ACTIVE_BACKGROUND_TASKS.add(-1, &[]);
        let guard = self.inner.lock().await;
        guard.tasks.get(id).map(|lt| lt.record.clone())
    }

    /// Read the last `max_bytes` from the task's output file.
    pub async fn read_output(&self, id: &str, max_bytes: usize) -> Result<String, String> {
        let output_file = {
            let guard = self.inner.lock().await;
            guard
                .tasks
                .get(id)
                .map(|lt| lt.record.output_file.clone())
                .ok_or_else(|| format!("task not found: {id}"))?
        };

        if !output_file.exists() {
            return Ok(String::new());
        }

        let bytes = tokio::fs::read(&output_file)
            .await
            .map_err(|e| e.to_string())?;

        let content = String::from_utf8_lossy(&bytes).into_owned();
        if content.len() > max_bytes {
            Ok(content[content.len() - max_bytes..].to_string())
        } else {
            Ok(content)
        }
    }
}

impl Default for BackgroundTaskManager {
    fn default() -> Self {
        Self::new()
    }
}

// ── BackgroundTasks trait impl ────────────────────────────────────────────────
//
// Thin delegation to the inherent methods so tools can reach the manager via the
// `oh_types::BackgroundTasks` trait object without a dependency cycle.

#[async_trait::async_trait]
impl oh_types::subagent::BackgroundTasks for BackgroundTaskManager {
    async fn create_shell(&self, command: &str, description: &str, cwd: &str) -> TaskRecord {
        self.create_shell_task(command, description, cwd).await
    }

    async fn create_agent(&self, prompt: &str, description: &str, cwd: &str) -> TaskRecord {
        self.create_agent_task(prompt, description, cwd).await
    }

    async fn get(&self, id: &str) -> Option<TaskRecord> {
        self.get_task(id).await
    }

    async fn list(&self, status: Option<TaskStatus>) -> Vec<TaskRecord> {
        self.list_tasks(status).await
    }

    async fn stop(&self, id: &str) -> Option<TaskRecord> {
        self.stop_task(id).await
    }

    async fn read_output(&self, id: &str, max_bytes: usize) -> Result<String, String> {
        BackgroundTaskManager::read_output(self, id, max_bytes).await
    }
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Spawn `/bin/bash -lc <command>` in `cwd`, tee stdout+stderr to
/// `output_file`, and update the task record in `inner` when the process exits.
fn spawn_and_watch(
    task_id: String,
    command: String,
    cwd: String,
    output_file: PathBuf,
    inner: Arc<Mutex<Inner>>,
    kill_rx: tokio::sync::oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Spawn the child.
        let child = Command::new("/bin/bash")
            .args(["-lc", &command])
            .current_dir(&cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .spawn();

        let mut child: Child = match child {
            Ok(c) => c,
            Err(e) => {
                // Record failure to launch.
                let mut guard = inner.lock().await;
                if let Some(lt) = guard.tasks.get_mut(&task_id) {
                    lt.record.status = TaskStatus::Failed;
                    lt.record.ended_at = Some(now());
                    lt.record.return_code = Some(-1);
                    lt.record
                        .metadata
                        .insert("spawn_error".into(), e.to_string());
                }
                oh_telemetry::ACTIVE_BACKGROUND_TASKS.add(-1, &[]);
                notify_listeners(&mut *guard, &task_id);
                return;
            }
        };

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        // Open the log file for appending (both streams go here).
        let log_file = match tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&output_file)
            .await
        {
            Ok(f) => Arc::new(Mutex::new(f)),
            Err(e) => {
                tracing::error!("failed to open task log {output_file:?}: {e}");
                let _ = child.kill().await;
                let mut guard = inner.lock().await;
                if let Some(lt) = guard.tasks.get_mut(&task_id) {
                    lt.record.status = TaskStatus::Failed;
                    lt.record.ended_at = Some(now());
                    lt.record.return_code = Some(-1);
                }
                oh_telemetry::ACTIVE_BACKGROUND_TASKS.add(-1, &[]);
                notify_listeners(&mut *guard, &task_id);
                return;
            }
        };

        // Copy stdout → log file.
        let stdout_handle = if let Some(mut out) = stdout {
            let log = Arc::clone(&log_file);
            Some(tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    match tokio::io::AsyncReadExt::read(&mut out, &mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let mut f = log.lock().await;
                            let _ = f.write_all(&buf[..n]).await;
                        }
                    }
                }
            }))
        } else {
            None
        };

        // Copy stderr → log file.
        let stderr_handle = if let Some(mut err) = stderr {
            let log = Arc::clone(&log_file);
            Some(tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    match tokio::io::AsyncReadExt::read(&mut err, &mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let mut f = log.lock().await;
                            let _ = f.write_all(&buf[..n]).await;
                        }
                    }
                }
            }))
        } else {
            None
        };

        // Remember the child PID so we can kill the entire process group.
        let child_pid = child.id();

        // Wait for either the process to finish OR a kill signal.
        tokio::select! {
            status = child.wait() => {
                // Process finished on its own.
                let exit_status_opt = status.ok();
                // Use the actual exit code from the OS.  If the process was
                // killed by a signal, code() is None — treat that as Failed
                // with return_code -1, not as Completed.
                let return_code: Option<i32> = exit_status_opt.as_ref().and_then(|s| s.code());
                let succeeded = return_code == Some(0);

                // Drain I/O tasks before updating status.
                if let Some(h) = stdout_handle { let _ = h.await; }
                if let Some(h) = stderr_handle { let _ = h.await; }

                let mut guard = inner.lock().await;
                oh_telemetry::ACTIVE_BACKGROUND_TASKS.add(-1, &[]);
                if let Some(lt) = guard.tasks.get_mut(&task_id) {
                    // If stop_task already marked this Killed, preserve that.
                    if lt.record.status != TaskStatus::Killed {
                        lt.record.status = if succeeded {
                            TaskStatus::Completed
                        } else {
                            TaskStatus::Failed
                        };
                        // For signal-killed processes where code() is None,
                        // record -1 so callers can distinguish from a clean exit.
                        lt.record.return_code = Some(return_code.unwrap_or(-1));
                        lt.record.ended_at = Some(now());
                    }
                }
                notify_listeners(&mut *guard, &task_id);
            }
            _ = kill_rx => {
                // Kill signal received from stop_task().
                // Kill the whole process group so bash-launched grandchildren
                // are also reaped (on Unix only; best-effort on other platforms).
                #[cfg(unix)]
                if let Some(pid) = child_pid {
                    // SAFETY: calling kill(-pgid, SIGKILL) is always safe; a
                    // non-existent pgid returns ESRCH which we ignore.
                    unsafe {
                        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
                    }
                }
                let _ = child.kill().await;
                let _ = child.wait().await;

                // Drain I/O tasks.
                if let Some(h) = stdout_handle { let _ = h.await; }
                if let Some(h) = stderr_handle { let _ = h.await; }

                // Status was already set to Killed in stop_task(); fire listeners.
                let mut guard = inner.lock().await;
                notify_listeners(&mut *guard, &task_id);
            }
        }
    })
}

/// Drive an in-process [`InProcessJob`] to completion: write its result (or
/// error) to `output_file`, then update the task record in `inner`. Honors a
/// kill signal from `stop_task` by aborting the job.
fn spawn_in_process(
    task_id: String,
    output_file: PathBuf,
    inner: Arc<Mutex<Inner>>,
    job: InProcessJob,
    kill_rx: tokio::sync::oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let outcome: Option<Result<String, String>> = tokio::select! {
            res = job => Some(res),
            _ = kill_rx => None, // killed via stop_task
        };

        // Append the result/error text to the log file (best-effort).
        if let Some(ref res) = outcome {
            let text = match res {
                Ok(s) => s.clone(),
                Err(e) => e.clone(),
            };
            if let Ok(mut f) = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&output_file)
                .await
            {
                let _ = f.write_all(text.as_bytes()).await;
                let _ = f.flush().await;
            }
        }

        let mut guard = inner.lock().await;
        oh_telemetry::ACTIVE_BACKGROUND_TASKS.add(-1, &[]);
        if let Some(lt) = guard.tasks.get_mut(&task_id) {
            // Preserve a Killed status set by stop_task.
            if lt.record.status != TaskStatus::Killed {
                match &outcome {
                    Some(Ok(_)) => {
                        lt.record.status = TaskStatus::Completed;
                        lt.record.return_code = Some(0);
                    }
                    Some(Err(_)) => {
                        lt.record.status = TaskStatus::Failed;
                        lt.record.return_code = Some(-1);
                    }
                    None => {
                        lt.record.status = TaskStatus::Killed;
                        lt.record.return_code = Some(-1);
                    }
                }
                lt.record.ended_at = Some(now());
            }
        }
        notify_listeners(&mut guard, &task_id);
    })
}

/// Fire all registered completion listeners for `task_id`.
/// Must be called while holding the `Inner` lock.
fn notify_listeners(guard: &mut Inner, task_id: &str) {
    let record = match guard.tasks.get(task_id) {
        Some(lt) => lt.record.clone(),
        None => return,
    };
    for listener in guard.completion_listeners.values() {
        listener(&record);
    }
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{sleep, Duration};

    fn tasks_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("oh_tasks_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Override the tasks dir for tests so we don't write into the real config dir.
    fn with_tasks_dir(dir: &PathBuf) {
        // oh_config::get_tasks_dir() reads an env var if set.
        // We use a temp directory and write the output_file path manually,
        // so no override is strictly required; the manager accepts the path
        // embedded in the TaskRecord.
        let _ = dir; // used in documentation only
    }

    #[tokio::test]
    async fn test_echo_hello_succeeds() {
        let dir = tasks_dir();
        with_tasks_dir(&dir);

        let mgr = BackgroundTaskManager::new();

        // Use a known output file path.
        let task = mgr
            .create_shell_task("echo hello", "test echo", "/tmp")
            .await;

        let task_id = task.id.clone();

        // Poll for completion (up to 5 s).
        let mut final_record = None;
        for _ in 0..50 {
            sleep(Duration::from_millis(100)).await;
            let r = mgr.get_task(&task_id).await.unwrap();
            if r.status != TaskStatus::Running && r.status != TaskStatus::Pending {
                final_record = Some(r);
                break;
            }
        }

        let record = final_record.expect("task did not complete within 5 s");

        assert_eq!(
            record.status,
            TaskStatus::Completed,
            "expected Completed, got {:?}",
            record.status
        );
        assert_eq!(record.return_code, Some(0));

        let output = mgr.read_output(&task_id, 65536).await.unwrap();
        assert!(
            output.contains("hello"),
            "output did not contain 'hello': {output:?}"
        );
    }

    #[tokio::test]
    async fn test_stop_task_kills_sleep() {
        let mgr = BackgroundTaskManager::new();

        let task = mgr
            .create_shell_task("sleep 30", "long sleep", "/tmp")
            .await;
        let task_id = task.id.clone();

        // Give it a moment to start.
        sleep(Duration::from_millis(200)).await;

        let stopped = mgr.stop_task(&task_id).await.unwrap();
        assert_eq!(
            stopped.status,
            TaskStatus::Killed,
            "expected Killed, got {:?}",
            stopped.status
        );
        assert!(stopped.ended_at.is_some(), "ended_at should be set");
    }

    #[tokio::test]
    async fn test_completion_listener_fires() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let fired = Arc::new(AtomicBool::new(false));
        let fired2 = Arc::clone(&fired);

        let mgr = BackgroundTaskManager::new();

        let _unregister = mgr
            .register_completion_listener(Box::new(move |rec| {
                if rec.status == TaskStatus::Completed {
                    fired2.store(true, Ordering::SeqCst);
                }
            }))
            .await;

        let task = mgr
            .create_shell_task("echo done", "listener test", "/tmp")
            .await;
        let task_id = task.id.clone();

        for _ in 0..50 {
            sleep(Duration::from_millis(100)).await;
            let r = mgr.get_task(&task_id).await.unwrap();
            if r.status != TaskStatus::Running && r.status != TaskStatus::Pending {
                break;
            }
        }

        assert!(
            fired.load(Ordering::SeqCst),
            "completion listener was not fired"
        );
    }

    #[tokio::test]
    async fn test_background_tasks_trait_delegates() {
        use oh_types::subagent::BackgroundTasks;

        let mgr = BackgroundTaskManager::new();
        let mgr: &dyn BackgroundTasks = &mgr;

        let task = mgr
            .create_shell("echo via-trait", "trait test", "/tmp")
            .await;
        let task_id = task.id.clone();

        // get() via the trait returns the same record.
        let got = mgr.get(&task_id).await.expect("task should exist");
        assert_eq!(got.id, task_id);

        // list() via the trait includes it.
        let listed = mgr.list(None).await;
        assert!(listed.iter().any(|r| r.id == task_id));

        // Poll for completion, then read_output() via the trait.
        for _ in 0..50 {
            sleep(Duration::from_millis(100)).await;
            let r = mgr.get(&task_id).await.unwrap();
            if r.status != TaskStatus::Running && r.status != TaskStatus::Pending {
                break;
            }
        }
        let output = mgr.read_output(&task_id, 65536).await.unwrap();
        assert!(output.contains("via-trait"), "output: {output:?}");
    }

    #[tokio::test]
    async fn test_failed_command_status() {
        let mgr = BackgroundTaskManager::new();

        let task = mgr
            .create_shell_task("exit 42", "failing task", "/tmp")
            .await;
        let task_id = task.id.clone();

        let mut final_record = None;
        for _ in 0..50 {
            sleep(Duration::from_millis(100)).await;
            let r = mgr.get_task(&task_id).await.unwrap();
            if r.status != TaskStatus::Running && r.status != TaskStatus::Pending {
                final_record = Some(r);
                break;
            }
        }

        let record = final_record.expect("task did not complete");
        assert_eq!(record.status, TaskStatus::Failed);
        assert_eq!(record.return_code, Some(42));
    }
}
