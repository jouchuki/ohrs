use oh_swarm::{InProcessBackend, TeammateConfig, TeammateId};
use oh_swarm::backend::Backend;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::time::{sleep, Duration};

#[tokio::test]
async fn spawn_runs_body() {
    let dir = tempfile::tempdir().unwrap();
    let backend = InProcessBackend::new(dir.path());

    let ran = Arc::new(AtomicBool::new(false));
    let ran2 = ran.clone();

    let id = TeammateId::new("worker-a");
    let config = TeammateConfig::with_body("worker-a", move |_cancel, _mb| {
        let ran3 = ran2.clone();
        async move {
            ran3.store(true, Ordering::SeqCst);
        }
    });

    let _handle = backend.spawn(id.clone(), config).await.unwrap();

    // Allow the spawned task to run.
    sleep(Duration::from_millis(50)).await;
    assert!(ran.load(Ordering::SeqCst), "body should have run");
}

#[tokio::test]
async fn kill_graceful_triggers_cancel() {
    let dir = tempfile::tempdir().unwrap();
    let backend = InProcessBackend::new(dir.path());

    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled2 = cancelled.clone();

    let id = TeammateId::new("worker-b");
    let config = TeammateConfig::with_body("worker-b", move |cancel, _mb| {
        let c = cancelled2.clone();
        async move {
            cancel.cancelled().await;
            c.store(true, Ordering::SeqCst);
        }
    });

    let _handle = backend.spawn(id.clone(), config).await.unwrap();

    // Give task time to start waiting on the token.
    sleep(Duration::from_millis(20)).await;

    backend.kill(&id, true).await.unwrap();

    // Give the task time to observe cancellation.
    sleep(Duration::from_millis(50)).await;
    assert!(cancelled.load(Ordering::SeqCst), "CancellationToken should have propagated");
}

#[tokio::test]
async fn duplicate_spawn_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    let backend = InProcessBackend::new(dir.path());

    let id = TeammateId::new("worker-c");
    let make_cfg = || {
        TeammateConfig::with_body("worker-c", |cancel, _mb| async move {
            cancel.cancelled().await;
        })
    };

    backend.spawn(id.clone(), make_cfg()).await.unwrap();
    let result = backend.spawn(id.clone(), make_cfg()).await;
    assert!(
        matches!(result, Err(oh_swarm::SwarmError::AlreadyRunning(_))),
        "expected AlreadyRunning, got {:?}",
        result
    );
}
