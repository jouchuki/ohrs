//! Cron scheduler: file-backed jobs with cron-expression or interval schedules.
//!
//! # Design
//! - [`CronStore`] persists jobs as individual JSON files under `<root>/<id>.json`.
//!   Writes are atomic (temp-file + rename).
//! - [`CronScheduler`] spawns one `tokio::task` per enabled job.  Each task sleeps
//!   via `tokio::time::sleep_until` until the next fire time, then calls the
//!   injected [`CronRunner`] and updates the job record on disk.
//! - `pause` aborts the task handle and sets `enabled = false`.
//! - `resume` re-enables the job and spawns a fresh task.
//! - `fire_now` calls the runner immediately without touching the schedule.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};
use thiserror::Error;
use tokio::{
    sync::Mutex,
    task::JoinHandle,
    time::Instant,
};

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum CronError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("invalid cron expression '{0}': {1}")]
    BadExpression(String, String),

    #[error("job not found: {0}")]
    NotFound(String),
}

// ── Types ─────────────────────────────────────────────────────────────────────

/// How often a job should fire.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "value")]
pub enum CronSchedule {
    /// Standard 5-field cron expression: `minute hour dom month dow`.
    Cron(String),
    /// Fixed interval, stored as whole seconds.
    #[serde(with = "duration_secs")]
    Interval(Duration),
}

/// What the job should do when it fires.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum CronAction {
    RunPrompt {
        prompt: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    RunCommand {
        command: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cwd: Option<PathBuf>,
    },
}

/// A scheduled job record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub id: String,
    pub name: String,
    pub schedule: CronSchedule,
    pub action: CronAction,
    pub enabled: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "opt_system_time"
    )]
    pub last_run: Option<SystemTime>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "opt_system_time"
    )]
    pub next_run: Option<SystemTime>,
    #[serde(with = "system_time")]
    pub created_at: SystemTime,
}

// ── Serde helpers ─────────────────────────────────────────────────────────────

mod duration_secs {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        d.as_secs_f64().serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        f64::deserialize(d).map(Duration::from_secs_f64)
    }
}

mod system_time {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    pub fn serialize<S: Serializer>(t: &SystemTime, s: S) -> Result<S::Ok, S::Error> {
        t.duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs_f64()
            .serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SystemTime, D::Error> {
        let secs = f64::deserialize(d)?;
        Ok(UNIX_EPOCH + Duration::from_secs_f64(secs))
    }
}

mod opt_system_time {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    pub fn serialize<S: Serializer>(t: &Option<SystemTime>, s: S) -> Result<S::Ok, S::Error> {
        match t {
            None => s.serialize_none(),
            Some(t) => t
                .duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::ZERO)
                .as_secs_f64()
                .serialize(s),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<SystemTime>, D::Error> {
        let secs = Option::<f64>::deserialize(d)?;
        Ok(secs.map(|s| UNIX_EPOCH + Duration::from_secs_f64(s)))
    }
}

// ── Schedule math ─────────────────────────────────────────────────────────────

/// Compute the next fire time for a schedule, given the most recent run.
pub fn compute_next_run(
    schedule: &CronSchedule,
    last_run: Option<SystemTime>,
) -> Result<SystemTime, CronError> {
    match schedule {
        CronSchedule::Interval(d) => {
            let base = last_run.unwrap_or_else(SystemTime::now);
            Ok(base + *d)
        }
        CronSchedule::Cron(expr) => {
            // The `cron` crate uses 7-field expressions (sec min hour dom month dow year).
            // We accept the conventional 5-field form and expand it.
            let expanded = expand_cron_expr(expr);
            let sched = parse_cron_schedule(&expanded, expr)?;
            let now: DateTime<Utc> = SystemTime::now().into();
            let next = sched
                .upcoming(Utc)
                .find(|dt| *dt > now)
                .ok_or_else(|| {
                    CronError::BadExpression(
                        expr.clone(),
                        "no upcoming fire time found".to_string(),
                    )
                })?;
            Ok(chrono_to_system_time(next))
        }
    }
}

fn expand_cron_expr(expr: &str) -> String {
    // Count fields; if 5, prepend "0 " (seconds=0) and append " *" (year=any).
    let fields: Vec<&str> = expr.split_whitespace().collect();
    match fields.len() {
        5 => format!("0 {} *", expr),
        6 => format!("{} *", expr),
        _ => expr.to_string(),
    }
}

fn parse_cron_schedule(
    expanded: &str,
    original: &str,
) -> Result<cron::Schedule, CronError> {
    use std::str::FromStr;
    cron::Schedule::from_str(expanded)
        .map_err(|e| CronError::BadExpression(original.to_string(), e.to_string()))
}

fn chrono_to_system_time(dt: DateTime<Utc>) -> SystemTime {
    let unix = dt.timestamp() as u64;
    let nanos = dt.timestamp_subsec_nanos();
    SystemTime::UNIX_EPOCH + Duration::new(unix, nanos)
}

fn system_time_to_instant(st: SystemTime) -> Instant {
    let now_st = SystemTime::now();
    if st <= now_st {
        Instant::now()
    } else {
        let diff = st.duration_since(now_st).unwrap_or(Duration::ZERO);
        Instant::now() + diff
    }
}

// ── CronStore ─────────────────────────────────────────────────────────────────

/// Persists `CronJob` records as individual JSON files under `<root>/<id>.json`.
pub struct CronStore {
    root: PathBuf,
}

impl CronStore {
    pub fn new(root: PathBuf) -> Self {
        std::fs::create_dir_all(&root).ok();
        Self { root }
    }

    fn job_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{}.json", id))
    }

    pub async fn list(&self) -> Result<Vec<CronJob>, CronError> {
        let mut jobs = Vec::new();
        let mut rd = tokio::fs::read_dir(&self.root).await?;
        while let Some(entry) = rd.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                let bytes = tokio::fs::read(&path).await?;
                match serde_json::from_slice::<CronJob>(&bytes) {
                    Ok(job) => jobs.push(job),
                    Err(_) => {} // skip corrupted files
                }
            }
        }
        jobs.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(jobs)
    }

    pub async fn get(&self, id: &str) -> Result<Option<CronJob>, CronError> {
        let path = self.job_path(id);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = tokio::fs::read(&path).await?;
        let job = serde_json::from_slice(&bytes)?;
        Ok(Some(job))
    }

    /// Atomically write a job to disk using temp-file + rename.
    ///
    /// Uses a unique temp filename (includes a random suffix) to avoid races
    /// between concurrent writers for the same job.  The temp file is fsynced
    /// before the rename so the data is durable if the rename itself succeeds.
    pub async fn put(&self, job: &CronJob) -> Result<(), CronError> {
        use std::os::unix::io::AsRawFd;

        let target = self.job_path(&job.id);
        let dir = target.parent().unwrap_or(&self.root);

        // Unique temp path: include a random token to prevent concurrent-writer races.
        let rand_suffix: u64 = {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            SystemTime::now().hash(&mut h);
            std::thread::current().id().hash(&mut h);
            h.finish()
        };
        let tmp_path = dir.join(format!(".{}.{:016x}.tmp", job.id, rand_suffix));
        let bytes = serde_json::to_vec_pretty(job)?;

        // Write + fsync the temp file, then rename (atomic visibility + durability).
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&tmp_path)?;
            file.write_all(&bytes)?;
            // fsync ensures data is on-disk before the rename commits.
            unsafe { libc::fsync(file.as_raw_fd()) };
        }
        tokio::fs::rename(&tmp_path, &target).await?;
        Ok(())
    }

    pub async fn delete(&self, id: &str) -> Result<(), CronError> {
        let path = self.job_path(id);
        if path.exists() {
            tokio::fs::remove_file(&path).await?;
        }
        Ok(())
    }
}

// ── CronRunner trait ──────────────────────────────────────────────────────────

#[async_trait]
pub trait CronRunner: Send + Sync {
    async fn fire(&self, job: &CronJob) -> Result<(), CronError>;
}

// ── CronScheduler ─────────────────────────────────────────────────────────────

struct TaskEntry {
    handle: JoinHandle<()>,
}

/// Scheduler that maintains one tokio task per active job.
pub struct CronScheduler {
    store: Arc<CronStore>,
    runner: Arc<dyn CronRunner>,
    handles: Mutex<HashMap<String, TaskEntry>>,
}

impl CronScheduler {
    pub fn new(store: Arc<CronStore>, runner: Arc<dyn CronRunner>) -> Self {
        Self {
            store,
            runner,
            handles: Mutex::new(HashMap::new()),
        }
    }

    /// Start scheduling all currently enabled jobs.
    pub async fn start(&self) -> Result<(), CronError> {
        let jobs = self.store.list().await?;
        for job in jobs {
            if job.enabled {
                self.spawn_job(job).await;
            }
        }
        Ok(())
    }

    /// Abort all running tasks (does not persist changes).
    pub async fn stop(&self) -> Result<(), CronError> {
        let mut handles = self.handles.lock().await;
        for (_, entry) in handles.drain() {
            entry.handle.abort();
        }
        Ok(())
    }

    /// Add a new job: persist it and (if enabled) spawn its task.
    pub async fn add(&self, mut job: CronJob) -> Result<(), CronError> {
        // Compute next_run at add time.
        job.next_run = Some(compute_next_run(&job.schedule, job.last_run)?);
        self.store.put(&job).await?;
        if job.enabled {
            self.spawn_job(job).await;
        }
        Ok(())
    }

    /// Remove a job: cancel its task and delete from disk.
    pub async fn remove(&self, id: &str) -> Result<(), CronError> {
        self.cancel_task(id).await;
        self.store.delete(id).await?;
        Ok(())
    }

    /// Pause: cancel task and set `enabled = false` on disk.
    pub async fn pause(&self, id: &str) -> Result<(), CronError> {
        self.cancel_task(id).await;
        let mut job = self
            .store
            .get(id)
            .await?
            .ok_or_else(|| CronError::NotFound(id.to_string()))?;
        job.enabled = false;
        self.store.put(&job).await?;
        Ok(())
    }

    /// Resume: set `enabled = true`, recompute `next_run`, respawn task.
    pub async fn resume(&self, id: &str) -> Result<(), CronError> {
        let mut job = self
            .store
            .get(id)
            .await?
            .ok_or_else(|| CronError::NotFound(id.to_string()))?;
        job.enabled = true;
        job.next_run = Some(compute_next_run(&job.schedule, job.last_run)?);
        self.store.put(&job).await?;
        self.spawn_job(job).await;
        Ok(())
    }

    /// Fire a job immediately without affecting its schedule.
    pub async fn fire_now(&self, id: &str) -> Result<(), CronError> {
        let job = self
            .store
            .get(id)
            .await?
            .ok_or_else(|| CronError::NotFound(id.to_string()))?;
        self.runner.fire(&job).await?;
        Ok(())
    }

    /// Return `(job_id, next_run)` for all jobs that have a scheduled next run.
    pub async fn next_runs(&self) -> Vec<(String, SystemTime)> {
        match self.store.list().await {
            Ok(jobs) => jobs
                .into_iter()
                .filter_map(|j| j.next_run.map(|t| (j.id, t)))
                .collect(),
            Err(_) => vec![],
        }
    }

    // ── Internal helpers ───────────────────────────────────────────────────

    async fn cancel_task(&self, id: &str) {
        let mut handles = self.handles.lock().await;
        if let Some(entry) = handles.remove(id) {
            entry.handle.abort();
        }
    }

    async fn spawn_job(&self, job: CronJob) {
        let store = Arc::clone(&self.store);
        let runner = Arc::clone(&self.runner);
        let id = job.id.clone();

        let handle = tokio::spawn(async move {
            job_loop(store, runner, job).await;
        });

        let mut handles = self.handles.lock().await;
        // Abort any stale handle for this id.
        if let Some(old) = handles.insert(id, TaskEntry { handle }) {
            old.handle.abort();
        }
    }
}

/// Per-job loop: sleep until next_run, fire, update, repeat.
///
/// For interval-based schedules the next deadline is computed from the
/// *scheduled* deadline (not from wall-clock `now()` after fire completes).
/// This prevents cumulative drift when `runner.fire` takes non-trivial time.
async fn job_loop(store: Arc<CronStore>, runner: Arc<dyn CronRunner>, mut job: CronJob) {
    loop {
        // Determine when to next fire.
        let next = match job.next_run {
            Some(t) => t,
            None => match compute_next_run(&job.schedule, job.last_run) {
                Ok(t) => t,
                Err(_) => return,
            },
        };

        let fire_at = system_time_to_instant(next);
        tokio::time::sleep_until(fire_at).await;

        // Record actual wall-clock time at fire for `last_run`.
        let fired_at = SystemTime::now();

        // Fire the job (ignore runner errors — log in production).
        let _ = runner.fire(&job).await;

        // Update last_run and next_run.
        // For intervals we use the *scheduled deadline* (`next`) as the base
        // so the next fire stays aligned to the original cadence (no drift).
        // For cron expressions the base is ignored; compute_next_run uses Utc::now().
        job.last_run = Some(fired_at);
        job.next_run = match &job.schedule {
            CronSchedule::Interval(d) => {
                // next deadline = scheduled fire time + interval  (non-drifting)
                Some(next + *d)
            }
            CronSchedule::Cron(_) => compute_next_run(&job.schedule, job.last_run).ok(),
        };

        // Reload from store to pick up any external changes (enabled flag, etc.).
        if let Ok(Some(fresh)) = store.get(&job.id).await {
            if !fresh.enabled {
                // Job was paused externally; stop the loop.
                break;
            }
            // Keep schedule/action from disk, update timing fields.
            job = CronJob {
                last_run: job.last_run,
                next_run: job.next_run,
                ..fresh
            };
        }

        // Persist updated timing.
        let _ = store.put(&job).await;
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::time::Duration as TDuration;

    // ── MockCronRunner ────────────────────────────────────────────────────

    struct MockCronRunner {
        fires: Arc<AtomicUsize>,
    }

    impl MockCronRunner {
        fn new() -> (Self, Arc<AtomicUsize>) {
            let counter = Arc::new(AtomicUsize::new(0));
            (Self { fires: Arc::clone(&counter) }, counter)
        }
    }

    #[async_trait]
    impl CronRunner for MockCronRunner {
        async fn fire(&self, _job: &CronJob) -> Result<(), CronError> {
            self.fires.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn make_store(dir: &tempfile::TempDir) -> Arc<CronStore> {
        Arc::new(CronStore::new(dir.path().join("cron")))
    }

    fn make_job(schedule: CronSchedule) -> CronJob {
        CronJob {
            id: uuid::Uuid::new_v4().to_string(),
            name: "test-job".to_string(),
            schedule,
            action: CronAction::RunCommand {
                command: "echo hello".to_string(),
                cwd: None,
            },
            enabled: true,
            last_run: None,
            next_run: None,
            created_at: SystemTime::now(),
        }
    }

    // ── Store tests ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_store_put_get_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(&dir);
        let job = make_job(CronSchedule::Interval(Duration::from_secs(60)));

        store.put(&job).await.unwrap();

        let loaded = store.get(&job.id).await.unwrap().unwrap();
        assert_eq!(loaded.id, job.id);
        assert_eq!(loaded.name, job.name);

        store.delete(&job.id).await.unwrap();
        assert!(store.get(&job.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_store_list() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(&dir);

        let j1 = make_job(CronSchedule::Interval(Duration::from_secs(10)));
        let j2 = make_job(CronSchedule::Interval(Duration::from_secs(20)));
        store.put(&j1).await.unwrap();
        store.put(&j2).await.unwrap();

        let list = store.list().await.unwrap();
        assert_eq!(list.len(), 2);
    }

    #[tokio::test]
    async fn test_store_atomic_write() {
        // Ensure temp file doesn't leak on successful write.
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(&dir);
        let job = make_job(CronSchedule::Interval(Duration::from_secs(60)));
        store.put(&job).await.unwrap();

        let cron_dir = dir.path().join("cron");
        let entries: Vec<_> = std::fs::read_dir(&cron_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        // Only one file (the final JSON), no leftover .tmp
        assert_eq!(entries.len(), 1);
        assert!(entries[0].file_name().to_str().unwrap().ends_with(".json"));
    }

    // ── compute_next_run tests ────────────────────────────────────────────

    #[test]
    fn test_interval_next_run_no_last() {
        let before = SystemTime::now();
        let next = compute_next_run(
            &CronSchedule::Interval(Duration::from_secs(100)),
            None,
        )
        .unwrap();
        let after = SystemTime::now();
        // next should be approximately now + 100s
        let diff = next
            .duration_since(before)
            .unwrap()
            .as_secs();
        assert!(diff <= 100);
        let _ = after; // used
    }

    #[test]
    fn test_interval_next_run_with_last() {
        let last = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let next = compute_next_run(
            &CronSchedule::Interval(Duration::from_secs(300)),
            Some(last),
        )
        .unwrap();
        assert_eq!(
            next.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs(),
            1_000_300
        );
    }

    #[test]
    fn test_cron_expression_next_run() {
        // "* * * * *" => every minute; next should be within 60s
        let next = compute_next_run(&CronSchedule::Cron("* * * * *".to_string()), None).unwrap();
        let now = SystemTime::now();
        assert!(next > now);
        let diff = next.duration_since(now).unwrap().as_secs();
        assert!(diff <= 60, "Expected next within 60s, got {diff}s");
    }

    #[test]
    fn test_bad_cron_expression() {
        let result = compute_next_run(&CronSchedule::Cron("not a cron".to_string()), None);
        assert!(matches!(result, Err(CronError::BadExpression(_, _))));
    }

    // ── Scheduler: interval fires within 200ms ─────────────────────────

    #[tokio::test]
    async fn test_interval_fires_within_200ms() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(&dir);
        let (mock, counter) = MockCronRunner::new();
        let scheduler = CronScheduler::new(Arc::clone(&store), Arc::new(mock));

        let job = make_job(CronSchedule::Interval(Duration::from_millis(100)));
        scheduler.add(job).await.unwrap();

        tokio::time::sleep(TDuration::from_millis(200)).await;

        let fires = counter.load(Ordering::SeqCst);
        assert!(fires >= 1, "Expected at least 1 fire, got {fires}");

        scheduler.stop().await.unwrap();
    }

    // ── Scheduler: pause cancels, resume respawns ──────────────────────

    #[tokio::test]
    async fn test_pause_and_resume() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(&dir);
        let (mock, counter) = MockCronRunner::new();
        let scheduler = CronScheduler::new(Arc::clone(&store), Arc::new(mock));

        // Use a very short interval.
        let job = make_job(CronSchedule::Interval(Duration::from_millis(50)));
        let id = job.id.clone();
        scheduler.add(job).await.unwrap();

        // Let it fire at least once.
        tokio::time::sleep(TDuration::from_millis(120)).await;
        let fires_before_pause = counter.load(Ordering::SeqCst);
        assert!(fires_before_pause >= 1, "Expected fire before pause");

        // Pause.
        scheduler.pause(&id).await.unwrap();

        // Verify enabled = false on disk.
        let persisted = store.get(&id).await.unwrap().unwrap();
        assert!(!persisted.enabled);

        // Wait longer; counter should not grow.
        let snapshot = counter.load(Ordering::SeqCst);
        tokio::time::sleep(TDuration::from_millis(150)).await;
        let fires_while_paused = counter.load(Ordering::SeqCst) - snapshot;
        assert_eq!(fires_while_paused, 0, "Should not fire while paused");

        // Resume.
        scheduler.resume(&id).await.unwrap();
        let persisted = store.get(&id).await.unwrap().unwrap();
        assert!(persisted.enabled);

        // Should fire again after resume.
        tokio::time::sleep(TDuration::from_millis(150)).await;
        let fires_after_resume = counter.load(Ordering::SeqCst) - snapshot - fires_while_paused;
        assert!(fires_after_resume >= 1, "Expected fire after resume");

        scheduler.stop().await.unwrap();
    }

    // ── Scheduler: fire_now is independent of schedule ─────────────────

    #[tokio::test]
    async fn test_fire_now_independent() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(&dir);
        let (mock, counter) = MockCronRunner::new();
        let scheduler = CronScheduler::new(Arc::clone(&store), Arc::new(mock));

        // Long interval so scheduled task won't fire during the test.
        let job = make_job(CronSchedule::Interval(Duration::from_secs(3600)));
        let id = job.id.clone();
        scheduler.add(job).await.unwrap();

        assert_eq!(counter.load(Ordering::SeqCst), 0);

        // fire_now triggers immediately.
        scheduler.fire_now(&id).await.unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        // next_run on disk should remain unchanged (fire_now doesn't update schedule).
        let persisted = store.get(&id).await.unwrap().unwrap();
        let next = persisted.next_run.unwrap();
        let diff = next
            .duration_since(SystemTime::now())
            .unwrap_or(Duration::ZERO)
            .as_secs();
        // next_run should still be ~3600s from now, not near-zero
        assert!(diff > 3000, "Expected next_run still far in future, diff={diff}s");

        scheduler.stop().await.unwrap();
    }

    // ── Scheduler: remove deletes from disk ────────────────────────────

    #[tokio::test]
    async fn test_remove_job() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(&dir);
        let (mock, _) = MockCronRunner::new();
        let scheduler = CronScheduler::new(Arc::clone(&store), Arc::new(mock));

        let job = make_job(CronSchedule::Interval(Duration::from_secs(60)));
        let id = job.id.clone();
        scheduler.add(job).await.unwrap();

        scheduler.remove(&id).await.unwrap();
        assert!(store.get(&id).await.unwrap().is_none());
    }

    // ── Scheduler: next_runs lists scheduled jobs ──────────────────────

    #[tokio::test]
    async fn test_next_runs() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(&dir);
        let (mock, _) = MockCronRunner::new();
        let scheduler = CronScheduler::new(Arc::clone(&store), Arc::new(mock));

        let j1 = make_job(CronSchedule::Interval(Duration::from_secs(60)));
        let j2 = make_job(CronSchedule::Interval(Duration::from_secs(120)));
        scheduler.add(j1).await.unwrap();
        scheduler.add(j2).await.unwrap();

        let runs = scheduler.next_runs().await;
        assert_eq!(runs.len(), 2);

        scheduler.stop().await.unwrap();
    }

    // ── CronAction/CronSchedule serde round-trip ───────────────────────

    #[test]
    fn test_serde_round_trip() {
        let job = CronJob {
            id: "abc".to_string(),
            name: "serde-test".to_string(),
            schedule: CronSchedule::Cron("0 * * * *".to_string()),
            action: CronAction::RunPrompt {
                prompt: "do something".to_string(),
                model: Some("claude-3-5-sonnet".to_string()),
            },
            enabled: true,
            last_run: None,
            next_run: None,
            created_at: SystemTime::now(),
        };

        let json = serde_json::to_string(&job).unwrap();
        let decoded: CronJob = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.id, job.id);
        assert_eq!(decoded.name, job.name);
        assert!(matches!(decoded.schedule, CronSchedule::Cron(_)));
        assert!(matches!(decoded.action, CronAction::RunPrompt { .. }));
    }
}
