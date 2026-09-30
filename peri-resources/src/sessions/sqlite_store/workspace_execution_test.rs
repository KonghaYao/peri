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

/// 同 `lease_process`，额外把目标代次传给子进程（reset/观测用）。
fn lease_process_at(path: &Path, id: &str, expected: &str, generation: i64) {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "sessions::sqlite_store::workspace::tests::execution::test_worktree_execution_child_process",
            "--nocapture",
        ])
        .env("PERI_TEST_WORKSPACE_DB", path)
        .env("PERI_TEST_WORKSPACE_ID", id)
        .env("PERI_TEST_WORKSPACE_EXPECT", expected)
        .env("PERI_TEST_WORKSPACE_GENERATION", generation.to_string())
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

/// 启动一个「短命持有者」子进程：取得所有权后打印就绪行、保持 `hold_ms` 再正常收尾。
///
/// 返回的句柄带 stdout 管道，父进程据此确定「锁已被持有」——`flock` 的持有者何时释放
/// 取决于调度，只有就绪信号之后的尝试才是确定性的重叠。
fn lease_process_hold(path: &Path, id: &str) -> std::process::Child {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "sessions::sqlite_store::workspace::tests::execution::test_worktree_execution_child_process", "--nocapture"])
        .env("PERI_TEST_WORKSPACE_DB", path)
        .env("PERI_TEST_WORKSPACE_ID", id)
        .env("PERI_TEST_WORKSPACE_EXPECT", "hold")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_worktree_execution_overlaps_across_processes_and_recovers_after_crash() {
    let repo = repository();
    let (store, db) = store().await;
    let (id, _) = bound(&store, repo.path()).await;
    let path = db.path().join("threads.db");
    let lease = store.acquire_execution_lease(&id).await.unwrap();
    lease_process(&path, &id, "clean");
    assert_eq!(
        store.database.load_execution_state(&id).await.unwrap(),
        Some((2, true))
    );
    lease.mark_clean().await.unwrap();
    assert_eq!(
        store.database.load_execution_state(&id).await.unwrap(),
        Some((2, true))
    );
    lease_process(&path, &id, "crash");
    assert_eq!(
        store.database.load_execution_state(&id).await.unwrap(),
        Some((3, false))
    );
    let next = store.acquire_execution_lease(&id).await.unwrap();
    assert_eq!(next.thread_id(), &id);
    assert_eq!(
        store.database.load_execution_state(&id).await.unwrap(),
        Some((4, false))
    );
    next.mark_clean().await.unwrap();
    assert_eq!(
        store.database.load_execution_state(&id).await.unwrap(),
        Some((4, true))
    );
    assert!(!db.path().join("threads.db.execution-locks").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_worktree_dirty_reset_uses_exact_generation_without_process_lock() {
    let repo = repository();
    let (store, db) = store().await;
    let (id, _) = bound(&store, repo.path()).await;
    let path = db.path().join("threads.db");
    let before = store.load_session_binding(&id).await.unwrap();
    let lease = store.acquire_execution_lease(&id).await.unwrap();
    lease_process_at(&path, &id, "reset_ok", 1);
    assert_eq!(
        store.database.load_execution_state(&id).await.unwrap(),
        Some((1, true))
    );
    drop(lease);
    lease_process_at(&path, &id, "reset_stale", 1);
    lease_process(&path, &id, "crash");
    lease_process_at(&path, &id, "generation", 2);
    lease_process_at(&path, &id, "reset_stale", 1);
    lease_process_at(&path, &id, "generation", 2);
    lease_process_at(&path, &id, "reset_ok", 2);
    lease_process(&path, &id, "clean");
    assert_eq!(
        store.database.load_execution_state(&id).await.unwrap(),
        Some((3, true))
    );
    assert_eq!(store.load_session_binding(&id).await.unwrap(), before);
}

#[tokio::test]
async fn test_worktree_execution_child_process() {
    let Ok(path) = std::env::var("PERI_TEST_WORKSPACE_DB") else {
        return;
    };
    let id = std::env::var("PERI_TEST_WORKSPACE_ID").unwrap();
    let expected = std::env::var("PERI_TEST_WORKSPACE_EXPECT").unwrap();
    let store = SqliteThreadStore::new(Path::new(&path)).await.unwrap();
    let target = || RecoveryRequiredDetails {
        thread_id: id.clone(),
        generation: std::env::var("PERI_TEST_WORKSPACE_GENERATION")
            .unwrap()
            .parse()
            .unwrap(),
    };
    match expected.as_str() {
        "generation" => {
            assert_eq!(
                store.database.load_execution_state(&id).await.unwrap(),
                Some((target().generation, false))
            );
        }
        "clean" => {
            let lease = store.acquire_execution_lease(&id).await.unwrap();
            lease.mark_clean().await.unwrap();
        }
        "crash" => {
            let _lease = store.acquire_execution_lease(&id).await.unwrap();
            std::process::exit(0);
        }
        "hold" => {
            use std::io::Write;
            let lease = store.acquire_execution_lease(&id).await.unwrap();
            std::io::stdout().write_all(b"HOLDER-READY\n").unwrap();
            std::io::stdout().flush().unwrap();
            let mut release = String::new();
            std::io::stdin().read_line(&mut release).unwrap();
            assert_eq!(release.trim(), "release");
            lease.mark_clean().await.unwrap();
        }
        "reset_ok" => store.reset_dirty_execution(&target()).await.unwrap(),
        "reset_stale" => {
            let error = store.reset_dirty_execution(&target()).await.unwrap_err();
            assert!(matches!(
                error.downcast_ref::<WorkspaceError>(),
                Some(WorkspaceError::RecoveryGenerationMismatch)
            ));
        }
        _ => panic!("unknown expected child result"),
    }
}

/// [回归测试] 短命持有者释放后的取得所有权不得被误报成 ExecutionBusy。
///
/// `flock` 的锁挂在 open file description 上：本进程 `fork` 出的子进程在 `exec` 前共享父
/// 进程的描述符（`CLOEXEC` 只在子进程 `exec` 时关闭），会话生命周期里的 Git 发现、
/// `sw_vers`、LSP 等子进程因此会留下毫秒级的瞬时持有。没有重试时，这类窗口会被上报成
/// 「会话已被其他执行宿主占用」，把一次正常的取得所有权变成偶发失败；真正的外部持有者
/// 并不受重试影响（超时后仍报忙，见 `test_worktree_execution_competes_across_processes_and_crash_remains_dirty`）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_worktree_live_process_does_not_block_another_run() {
    use std::io::{BufRead, Write};
    let repo = repository();
    let (store, db) = store().await;
    let (id, _) = bound(&store, repo.path()).await;
    let mut holder = lease_process_hold(&db.path().join("threads.db"), &id);
    let mut lines = std::io::BufReader::new(holder.stdout.take().unwrap()).lines();
    assert!(lines.any(|line| line.unwrap().contains("HOLDER-READY")));
    let acquisition =
        tokio::time::timeout(Duration::from_secs(2), store.acquire_execution_lease(&id)).await;
    holder
        .stdin
        .take()
        .unwrap()
        .write_all(b"release\n")
        .unwrap();
    assert!(holder.wait().unwrap().success());
    let lease = acquisition.unwrap().unwrap();
    assert_eq!(
        store.database.load_execution_state(&id).await.unwrap(),
        Some((2, false))
    );
    lease.mark_clean().await.unwrap();
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
    // Poll once with an admitted mutation suspended: close must not publish clean.
    assert!(matches!(
        close.as_mut().poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    sqlx::query("UPDATE threads SET title = 'last owner write' WHERE id = ?")
        .bind(&id)
        .execute(&store.database.pool)
        .await
        .unwrap();
    let run: (bool,) = sqlx::query_as("SELECT clean FROM execution_runs WHERE thread_id = ?")
        .bind(&id)
        .fetch_one(&store.database.pool)
        .await
        .unwrap();
    assert!(!run.0);
    mutation.finish();
    close.await.unwrap();
    assert_eq!(
        store.load_meta(&id).await.unwrap().title.as_deref(),
        Some("last owner write")
    );
    lease_process(&db.path().join("threads.db"), &id, "clean");
}

#[tokio::test]
async fn test_worktree_cancelled_mutation_remains_dirty_and_cannot_publish_clean() {
    let repo = repository();
    let (store, _db) = store().await;
    let (id, _) = bound(&store, repo.path()).await;
    let lease = store.acquire_execution_lease(&id).await.unwrap();
    let facts = store.database.local_session_facts(&id).await.unwrap();
    let mutation = store
        .database
        .require_execution_lease(&id, &facts)
        .await
        .unwrap()
        .unwrap();
    // Dropping the capability without its completion signal models future cancellation.
    drop(mutation);
    assert!(lease.mark_clean().await.is_err());
    assert_eq!(
        store.database.load_execution_state(&id).await.unwrap(),
        Some((1, false))
    );
    drop(lease);
    let next = store.acquire_execution_lease(&id).await.unwrap();
    assert_eq!(
        store.database.load_execution_state(&id).await.unwrap(),
        Some((2, false))
    );
    next.mark_clean().await.unwrap();
    assert_eq!(
        store.database.load_execution_state(&id).await.unwrap(),
        Some((2, true))
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
