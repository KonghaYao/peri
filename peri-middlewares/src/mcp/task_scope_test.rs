use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use super::{McpTaskKey, McpTaskOwner, McpTaskShutdownReport};

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
