use super::*;

fn lease_process(path: &Path, id: &str, expected: &str) {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "sessions::sqlite_store::workspace::tests::execution::test_worktree_execution_child_process",
            "--nocapture",
        ])
        .env("PERI_TEST_WORKSPACE_DB", path)
        .env("PERI_TEST_WORKSPACE_ID", id)
        .env("PERI_TEST_WORKSPACE_EXPECT", expected)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "lease child failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("running 1 test"));
}

fn lease_process_hold(path: &Path, id: &str) -> std::process::Child {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "sessions::sqlite_store::workspace::tests::execution::test_worktree_execution_child_process",
            "--nocapture",
        ])
        .env("PERI_TEST_WORKSPACE_DB", path)
        .env("PERI_TEST_WORKSPACE_ID", id)
        .env("PERI_TEST_WORKSPACE_EXPECT", "hold")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_worktree_execution_overlaps_across_processes_and_recovers_after_crash() {
    let repo = repository();
    let (store, db) = store().await;
    let (id, workspace) = bound(&store, repo.path()).await;
    let path = db.path().join("threads.db");
    let lease = store.acquire_execution_lease(&id).await.unwrap();
    lease_process(&path, &id, "clean");
    store
        .append_message(&id, BaseMessage::human("original owner still active"))
        .await
        .unwrap();
    lease.mark_clean().await.unwrap();
    lease_process(&path, &id, "crash");
    let next = store.acquire_execution_lease(&id).await.unwrap();
    assert_eq!(next.thread_id(), &id);
    store
        .append_message(&id, BaseMessage::human("continued after process crash"))
        .await
        .unwrap();
    next.mark_clean().await.unwrap();
    assert_eq!(
        store.validate_session_binding(&id).await.unwrap(),
        workspace
    );
    assert_eq!(store.load_messages(&id).await.unwrap().len(), 2);
    assert!(!db.path().join("threads.db.execution-locks").exists());
}

#[tokio::test]
async fn test_worktree_active_reacquisition_reuses_owner_and_old_close_does_not_close_new_owner() {
    let repo = repository();
    let (store, _db) = store().await;
    let (id, workspace) = bound(&store, repo.path()).await;
    let first = store.acquire_execution_lease(&id).await.unwrap();
    let reused = store.acquire_execution_lease(&id).await.unwrap();
    assert!(Arc::ptr_eq(&first, &reused));
    first.mark_clean().await.unwrap();
    assert!(store
        .append_message(&id, BaseMessage::human("closed owner"))
        .await
        .is_err());
    let next = store.acquire_execution_lease(&id).await.unwrap();
    assert!(!Arc::ptr_eq(&first, &next));
    first.mark_clean().await.unwrap();
    store
        .append_message(&id, BaseMessage::human("new owner remains active"))
        .await
        .unwrap();
    assert_eq!(store.load_messages(&id).await.unwrap().len(), 1);
    assert_eq!(
        store.validate_session_binding(&id).await.unwrap(),
        workspace
    );
    next.mark_clean().await.unwrap();
}

#[tokio::test]
async fn test_worktree_execution_child_process() {
    let Ok(path) = std::env::var("PERI_TEST_WORKSPACE_DB") else {
        return;
    };
    let id = std::env::var("PERI_TEST_WORKSPACE_ID").unwrap();
    let expected = std::env::var("PERI_TEST_WORKSPACE_EXPECT").unwrap();
    let store = SqliteThreadStore::new(path).await.unwrap();
    let lease = store.acquire_execution_lease(&id).await.unwrap();
    match expected.as_str() {
        "clean" => lease.mark_clean().await.unwrap(),
        "crash" => std::process::exit(0),
        "hold" => {
            use std::io::{Read, Write};
            println!("READY");
            std::io::stdout().flush().unwrap();
            let mut signal = [0_u8; 1];
            std::io::stdin().read_exact(&mut signal).unwrap();
            lease.mark_clean().await.unwrap();
        }
        other => panic!("unknown expected child result: {other}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_worktree_live_process_does_not_block_another_run() {
    use std::io::{BufRead, BufReader, Write};
    let repo = repository();
    let (store, db) = store().await;
    let (id, workspace) = bound(&store, repo.path()).await;
    let path = db.path().join("threads.db");
    let mut child = lease_process_hold(&path, &id);
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    loop {
        let mut line = String::new();
        assert_ne!(
            reader.read_line(&mut line).unwrap(),
            0,
            "child exited before READY"
        );
        if line.trim() == "READY" {
            break;
        }
    }
    let lease = store.acquire_execution_lease(&id).await.unwrap();
    store
        .append_message(&id, BaseMessage::human("concurrent process owner"))
        .await
        .unwrap();
    lease.mark_clean().await.unwrap();
    child.stdin.take().unwrap().write_all(b"x").unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(
        store.validate_session_binding(&id).await.unwrap(),
        workspace
    );
    assert_eq!(store.load_messages(&id).await.unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_worktree_clean_waits_for_admitted_mutation_before_closing_run() {
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    let repo = repository();
    let (store, db) = store().await;
    let (id, _) = bound(&store, repo.path()).await;
    let lease = store.acquire_execution_lease(&id).await.unwrap();
    let facts = store.database.local_session_facts(&id).await.unwrap();
    let mutation = store
        .database
        .require_execution_lease(&id, &facts)
        .await
        .unwrap()
        .unwrap();
    let mut close = std::pin::pin!(lease.mark_clean());
    assert!(matches!(
        close.as_mut().poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    sqlx::query("UPDATE threads SET title = 'last owner write' WHERE id = ?")
        .bind(&id)
        .execute(&store.database.pool)
        .await
        .unwrap();
    mutation.finish();
    close.await.unwrap();
    assert_eq!(
        store.load_meta(&id).await.unwrap().title.as_deref(),
        Some("last owner write")
    );
    assert!(store
        .append_message(&id, BaseMessage::human("after close"))
        .await
        .is_err());
    lease_process(&db.path().join("threads.db"), &id, "clean");
}

#[tokio::test]
async fn test_worktree_cancelled_mutation_blocks_reacquisition_until_instance_restart() {
    let repo = repository();
    let (store, db) = store().await;
    let (id, workspace) = bound(&store, repo.path()).await;
    let lease = store.acquire_execution_lease(&id).await.unwrap();
    let facts = store.database.local_session_facts(&id).await.unwrap();
    let mutation = store
        .database
        .require_execution_lease(&id, &facts)
        .await
        .unwrap()
        .unwrap();
    drop(mutation);
    assert!(lease.mark_clean().await.is_err());
    assert!(store.acquire_execution_lease(&id).await.is_err());
    assert!(store
        .append_message(&id, BaseMessage::human("unknown effect retry"))
        .await
        .is_err());
    drop(lease);
    assert!(store.acquire_execution_lease(&id).await.is_err());
    assert_eq!(
        store.validate_session_binding(&id).await.unwrap(),
        workspace
    );
    assert!(store.load_messages(&id).await.unwrap().is_empty());
    store.close().await;
    let reopened = SqliteThreadStore::new(db.path().join("threads.db"))
        .await
        .unwrap();
    let next = reopened.acquire_execution_lease(&id).await.unwrap();
    reopened
        .append_message(&id, BaseMessage::human("continued in reopened instance"))
        .await
        .unwrap();
    assert_eq!(
        reopened.validate_session_binding(&id).await.unwrap(),
        workspace
    );
    assert_eq!(reopened.load_messages(&id).await.unwrap().len(), 1);
    next.mark_clean().await.unwrap();
}

#[tokio::test]
async fn test_worktree_closed_store_rejects_new_owner() {
    let repo = repository();
    let (store, _db) = store().await;
    let (id, _) = bound(&store, repo.path()).await;
    let lease = store.acquire_execution_lease(&id).await.unwrap();
    store.close().await;
    assert!(store.acquire_execution_lease(&id).await.is_err());
    lease.mark_clean().await.unwrap();
}

#[tokio::test]
async fn test_worktree_readonly_store_rejects_new_owner_without_changing_canonical_facts() {
    let repo = repository();
    let (store, db) = store().await;
    let (id, workspace) = bound(&store, repo.path()).await;
    store
        .append_message(&id, BaseMessage::human("readonly history"))
        .await
        .unwrap();
    let before = serde_json::to_value(store.load_meta(&id).await.unwrap()).unwrap();
    let read = SqliteThreadStore::open_existing_read_only(db.path().join("threads.db"))
        .await
        .unwrap();
    assert!(read.acquire_execution_lease(&id).await.is_err());
    assert_eq!(
        read.load_session_binding(&id).await.unwrap(),
        store.load_session_binding(&id).await.unwrap()
    );
    assert_eq!(read.load_messages(&id).await.unwrap().len(), 1);
    assert_eq!(
        serde_json::to_value(read.load_meta(&id).await.unwrap()).unwrap(),
        before
    );
    assert_eq!(
        store.validate_session_binding(&id).await.unwrap(),
        workspace
    );
}

#[tokio::test]
async fn test_worktree_lease_supports_arbitrary_opaque_thread_ids() {
    let repo = repository();
    let (store, db) = store().await;
    let workspace = store.resolve_workspace(repo.path()).await.unwrap();
    let mut meta = ThreadMeta::new(workspace.cwd.to_str().unwrap());
    meta.id = format!("../arbitrary/会话-{}", "x".repeat(512));
    let id = store.create_bound_thread(meta, &workspace).await.unwrap();
    let lease = store.acquire_execution_lease(&id).await.unwrap();
    store
        .append_message(&id, BaseMessage::human("safe opaque identity"))
        .await
        .unwrap();
    lease.mark_clean().await.unwrap();
    assert_eq!(store.load_meta(&id).await.unwrap().id, id);
    assert_eq!(store.load_messages(&id).await.unwrap().len(), 1);
    assert!(!db.path().join("threads.db.execution-locks").exists());
    assert!(!db.path().join("arbitrary").exists());
}

/// [回归测试] writer 已持连接时不得为绑定重验再次向同一五连接池取连接。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_worktree_concurrent_creation_exceeds_pool_capacity_without_nested_acquisition() {
    const CONCURRENCY: usize = 8;
    let repo = repository();
    let (store, _db) = store().await;
    let workspace = store.resolve_workspace(repo.path()).await.unwrap();
    let store = Arc::new(store);
    let barrier = Arc::new(tokio::sync::Barrier::new(CONCURRENCY + 1));
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..CONCURRENCY {
        let store = store.clone();
        let workspace = workspace.clone();
        let barrier = barrier.clone();
        tasks.spawn(async move {
            barrier.wait().await;
            store
                .create_bound_thread(ThreadMeta::new(workspace.cwd.to_str().unwrap()), &workspace)
                .await
        });
    }
    barrier.wait().await;
    let mut ids = std::collections::HashSet::new();
    while let Some(result) = tasks.join_next().await {
        ids.insert(result.unwrap().unwrap());
    }
    assert_eq!(ids.len(), CONCURRENCY);
    for id in ids {
        assert_eq!(
            store.validate_session_binding(&id).await.unwrap(),
            workspace
        );
    }
}

/// [回归测试] 不同会话的 lease 竞争数据库 writer 时，重验必须复用各自事务连接。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_worktree_concurrent_leases_exceed_pool_capacity_without_nested_acquisition() {
    const CONCURRENCY: usize = 8;
    let repo = repository();
    let (store, _db) = store().await;
    let workspace = store.resolve_workspace(repo.path()).await.unwrap();
    let mut ids = Vec::new();
    for _ in 0..CONCURRENCY {
        ids.push(
            store
                .create_bound_thread(ThreadMeta::new(workspace.cwd.to_str().unwrap()), &workspace)
                .await
                .unwrap(),
        );
    }
    let store = Arc::new(store);
    let barrier = Arc::new(tokio::sync::Barrier::new(CONCURRENCY + 1));
    let mut tasks = tokio::task::JoinSet::new();
    for id in ids {
        let store = store.clone();
        let barrier = barrier.clone();
        tasks.spawn(async move {
            barrier.wait().await;
            store.acquire_execution_lease(&id).await
        });
    }
    barrier.wait().await;
    let mut leases = Vec::new();
    while let Some(result) = tasks.join_next().await {
        leases.push(result.unwrap().unwrap());
    }
    assert_eq!(leases.len(), CONCURRENCY);
    for lease in leases {
        lease.mark_clean().await.unwrap();
    }
}
