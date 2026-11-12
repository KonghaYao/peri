use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use super::{
    Completion, CompletionGuard, McpTaskKey, McpTaskOwner, McpTaskShutdownReport,
    TaskAdmissionError, TaskPhase, TaskRecord,
};

fn retained_records(owner: &McpTaskOwner) -> usize {
    owner
        .inner
        .as_ref()
        .map(|inner| {
            inner
                .state
                .lock()
                .expect("MCP task state poisoned")
                .records
                .values()
                .map(Vec::len)
                .sum()
        })
        .unwrap_or_default()
}

async fn wait_registry_baseline(owner: &McpTaskOwner) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while owner.active_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("completed tasks must deregister from the owner");
}

#[tokio::test]
async fn test_keyed_stop_before_first_poll_still_completes() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let started = Arc::new(AtomicBool::new(false));
    let started_task = started.clone();
    let key = McpTaskKey::OAuth("flow".into());
    spawner
        .spawn(key.clone(), async move {
            started_task.store(true, Ordering::SeqCst);
            std::future::pending::<()>().await;
        })
        .unwrap();
    spawner.stop_key(&key).await;
    assert!(!started.load(Ordering::SeqCst));
    assert_eq!(owner.active_count(), 0);
    assert_eq!(owner.shutdown().await, McpTaskShutdownReport::Complete);
}

#[tokio::test]
async fn test_external_owner_breaks_strong_pool_task_cycle() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let pool = Arc::new(crate::mcp::McpClientPool::new_pending_with_spawner(
        spawner.clone(),
    ));
    let weak = Arc::downgrade(&pool);
    let held_pool = pool.clone();
    spawner
        .spawn(McpTaskKey::Initialize, async move {
            let _pool = held_pool;
            std::future::pending::<()>().await;
        })
        .unwrap();
    pool.begin_shutdown();
    drop(pool);
    owner.shutdown().await;
    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn test_mcp_owner_shutdown_is_idempotent() {
    let (mut owner, _spawner) = McpTaskOwner::new();
    owner.begin_shutdown();
    assert_eq!(owner.shutdown().await, McpTaskShutdownReport::Complete);
    assert_eq!(owner.shutdown().await, McpTaskShutdownReport::Complete);
}

#[tokio::test]
async fn test_mcp_owner_drop_only_aborts_as_fallback() {
    struct Probe(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for Probe {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.send(());
            }
        }
    }

    let (owner, spawner) = McpTaskOwner::new();
    let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
    let probe = Probe(Some(dropped_tx));
    spawner
        .spawn(McpTaskKey::Initialize, async move {
            let _probe = probe;
            std::future::pending::<()>().await;
        })
        .unwrap();

    drop(owner);

    dropped_rx
        .await
        .expect("aborted task must release captures");
    assert!(spawner.spawn(McpTaskKey::Initialize, async {}).is_err());
}

#[tokio::test]
async fn test_missing_or_dropped_mcp_spawner_rejects_background_work() {
    let pool = Arc::new(crate::mcp::McpClientPool::new_pending());
    let started = Arc::new(AtomicBool::new(false));
    let started_task = started.clone();
    assert!(pool
        .spawn_background(McpTaskKey::Initialize, async move {
            started_task.store(true, Ordering::SeqCst);
        })
        .is_err());
    assert!(!started.load(Ordering::SeqCst));
}

#[tokio::test]
async fn test_terminal_shutdown_owns_task_after_keyed_waiter_cancel() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let key = McpTaskKey::Subscription("server".into());
    spawner
        .spawn(key.clone(), async move {
            std::future::pending::<()>().await;
        })
        .unwrap();
    let stop_spawner = spawner.clone();
    let stop_key = key.clone();
    let waiter = tokio::spawn(async move {
        stop_spawner.stop_key(&stop_key).await;
    });
    tokio::task::yield_now().await;
    waiter.abort();
    let _ = waiter.await;

    owner.begin_shutdown();
    assert_eq!(owner.shutdown().await, McpTaskShutdownReport::Complete);
    assert_eq!(owner.active_count(), 0);
}

#[tokio::test]
async fn test_admission_distinguishes_duplicate_from_owner_closed() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let key = McpTaskKey::Control("example".into());
    spawner.spawn(key.clone(), std::future::pending()).unwrap();
    assert_eq!(
        spawner.spawn(key, async {}).unwrap_err(),
        TaskAdmissionError::DuplicateKey
    );
    owner.begin_shutdown();
    assert_eq!(
        spawner.spawn(McpTaskKey::Initialize, async {}).unwrap_err(),
        TaskAdmissionError::OwnerClosed
    );
    owner.shutdown().await;
}

/// 30k 个不同 task id 的任务自然结束后，登记表不得保留任何已完成记录。
///
/// RSS 仅在 Linux 上可读，用作辅助观察；核心断言是 owner 仍开放时的残留记录数。
#[tokio::test]
async fn review_monitor_memory_completed_subscriptions_release_records() {
    fn rss_kib() -> usize {
        std::fs::read_to_string("/proc/self/status")
            .unwrap_or_default()
            .lines()
            .find(|line| line.starts_with("VmRSS:"))
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
    }

    let (mut owner, spawner) = McpTaskOwner::new();
    println!("MEMORY baseline rss_kib={}", rss_kib());
    let mut completed = 0;

    for target in [1_000, 10_000, 30_000] {
        while completed < target {
            let end = (completed + 250).min(target);
            for index in completed..end {
                spawner
                    .spawn(
                        McpTaskKey::TaskStatus {
                            server: "monitor".into(),
                            task_id: format!("mcp-00000000-0000-0000-0000-{index:012}"),
                        },
                        async {},
                    )
                    .unwrap();
            }
            while owner.active_count() != 0 {
                tokio::task::yield_now().await;
            }
            completed = end;
        }

        let state = owner.inner.as_ref().unwrap().state.lock().unwrap();
        let records: usize = state.records.values().map(Vec::len).sum();
        println!(
            "MEMORY completed={completed} retained_keys={} retained_records={records} rss_kib={}",
            state.records.len(),
            rss_kib(),
        );
    }

    let retained = owner
        .inner
        .as_ref()
        .unwrap()
        .state
        .lock()
        .unwrap()
        .records
        .len();
    owner.shutdown().await;
    assert_eq!(
        owner
            .inner
            .as_ref()
            .unwrap()
            .state
            .lock()
            .unwrap()
            .records
            .len(),
        0
    );
    assert_eq!(
        retained, 0,
        "naturally completed unique task subscriptions must release owner records"
    );
}

#[tokio::test]
async fn completed_task_releases_future_captures() {
    struct Probe(Arc<AtomicBool>);
    impl Drop for Probe {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    let (mut owner, spawner) = McpTaskOwner::new();
    let dropped = Arc::new(AtomicBool::new(false));
    let probe = Probe(dropped.clone());
    spawner
        .spawn(McpTaskKey::Control("capture".into()), async move {
            let _probe = probe;
        })
        .unwrap();

    wait_registry_baseline(&owner).await;
    assert!(
        dropped.load(Ordering::SeqCst),
        "task captures must drop on completion"
    );
    assert_eq!(retained_records(&owner), 0);
    owner.shutdown().await;
}

#[tokio::test]
async fn completed_key_can_be_reused_after_registration_release() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let key = McpTaskKey::Control("reuse".into());

    for round in 0..3 {
        spawner.spawn(key.clone(), async {}).unwrap();
        wait_registry_baseline(&owner).await;
        assert_eq!(retained_records(&owner), 0, "round {round} must not retain");
    }
    // The completed registration is gone, so the same key is admitted again.
    spawner.spawn(key, async {}).unwrap();
    wait_registry_baseline(&owner).await;
    assert_eq!(retained_records(&owner), 0);
    owner.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn natural_completion_and_stop_key_release_the_same_record() {
    let (mut owner, spawner) = McpTaskOwner::new();

    for round in 0..64 {
        let key = McpTaskKey::Control(format!("race-{round}"));
        spawner
            .spawn(key.clone(), async {
                tokio::task::yield_now().await;
            })
            .unwrap();
        let stop = spawner.clone();
        let stop_key = key.clone();
        tokio::spawn(async move { stop.stop_key(&stop_key).await })
            .await
            .unwrap();

        // The completion signal is published only after deregistration, so a
        // returned stop_key implies the record is gone.
        assert_eq!(retained_records(&owner), 0, "round {round} must deregister");
    }
    owner.shutdown().await;
}

#[tokio::test]
async fn canceled_stop_key_waiter_still_releases_the_task() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let key = McpTaskKey::Subscription("cancel-waiter".into());
    spawner
        .spawn(key.clone(), async {
            std::future::pending::<()>().await;
        })
        .unwrap();

    // The abort is issued synchronously before the first await, so canceling
    // the waiter cannot strand the task or its registration.
    let _ = tokio::time::timeout(Duration::from_millis(50), spawner.stop_key(&key)).await;
    wait_registry_baseline(&owner).await;
    assert_eq!(retained_records(&owner), 0);
    assert_eq!(owner.shutdown().await, McpTaskShutdownReport::Complete);
}

#[tokio::test]
async fn panicking_task_releases_registration() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let key = McpTaskKey::Control("panic".into());
    spawner
        .spawn(key.clone(), async {
            panic!("task panic probe");
        })
        .unwrap();

    wait_registry_baseline(&owner).await;
    assert_eq!(retained_records(&owner), 0);
    assert_eq!(owner.shutdown().await, McpTaskShutdownReport::Complete);
}

#[tokio::test]
async fn closing_owner_drains_inflight_registration_and_rejects_new_tasks() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let (release, wait) = tokio::sync::oneshot::channel::<()>();
    spawner
        .spawn(McpTaskKey::Control("closing".into()), async move {
            let _ = wait.await;
        })
        .unwrap();

    owner.begin_shutdown();
    assert_eq!(
        spawner.spawn(McpTaskKey::Initialize, async {}).unwrap_err(),
        TaskAdmissionError::OwnerClosed
    );

    release.send(()).unwrap();
    wait_registry_baseline(&owner).await;
    assert_eq!(retained_records(&owner), 0);
    assert_eq!(owner.shutdown().await, McpTaskShutdownReport::Complete);
}

#[tokio::test]
async fn completed_rounds_reuse_registry_capacity() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let key = McpTaskKey::Control("capacity".into());
    let mut warmed_capacity = None;

    for round in 0..256 {
        spawner.spawn(key.clone(), async {}).unwrap();
        wait_registry_baseline(&owner).await;
        assert_eq!(retained_records(&owner), 0, "round {round} must not retain");

        let capacity = owner
            .inner
            .as_ref()
            .unwrap()
            .state
            .lock()
            .unwrap()
            .records
            .capacity();
        match warmed_capacity {
            Some(expected) => {
                assert_eq!(
                    capacity, expected,
                    "round {round} grew the registry container"
                )
            }
            None if round >= 16 => warmed_capacity = Some(capacity),
            None => {}
        }
    }
    owner.shutdown().await;
}

#[tokio::test]
async fn stale_generation_guard_keeps_newer_registration() {
    let (mut owner, _spawner) = McpTaskOwner::new();
    let key = McpTaskKey::Control("generation".into());
    let stale_completion = Arc::new(Completion::new());
    let current_completion = Arc::new(Completion::new());
    let abort_stale = tokio::spawn(std::future::pending::<()>()).abort_handle();
    let abort_current = tokio::spawn(std::future::pending::<()>()).abort_handle();

    {
        let inner = owner.inner.as_ref().unwrap();
        let mut state = inner.state.lock().unwrap();
        state.records.insert(
            key.clone(),
            vec![
                TaskRecord {
                    generation: 1,
                    phase: TaskPhase::Running,
                    abort: abort_stale,
                    completion: stale_completion.clone(),
                },
                TaskRecord {
                    generation: 2,
                    phase: TaskPhase::Running,
                    abort: abort_current,
                    completion: current_completion.clone(),
                },
            ],
        );
    }

    let stale_guard = CompletionGuard {
        key: key.clone(),
        generation: 1,
        completion: stale_completion.clone(),
        inner: Arc::downgrade(owner.inner.as_ref().unwrap()),
    };
    drop(stale_guard);

    assert!(stale_completion.done.load(Ordering::Acquire));
    assert!(!current_completion.done.load(Ordering::Acquire));
    {
        let state = owner.inner.as_ref().unwrap().state.lock().unwrap();
        let records = state
            .records
            .get(&key)
            .expect("the newer generation must stay registered");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].generation, 2);
    }
    assert_eq!(owner.shutdown().await, McpTaskShutdownReport::Complete);
}
