use super::super::*;
use peri_acp_types::workspace::*;
use std::{path::Path, sync::Arc};
use tempfile::TempDir;

fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Git fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository() -> TempDir {
    let directory = tempfile::tempdir().unwrap();
    git(directory.path(), &["init", "-q"]);
    git(
        directory.path(),
        &[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
        ],
    );
    directory
}

async fn store() -> (SqliteThreadStore, TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteThreadStore::new(directory.path().join("threads.db"))
        .await
        .unwrap();
    (store, directory)
}

async fn bound(store: &SqliteThreadStore, cwd: &Path) -> (ThreadId, ResolvedWorkspace) {
    let workspace = store.resolve_workspace(cwd).await.unwrap();
    let id = store
        .create_bound_thread(ThreadMeta::new(cwd.to_str().unwrap()), &workspace)
        .await
        .unwrap();
    (id, workspace)
}

#[tokio::test]
async fn test_worktree_main_linked_subdirectory_and_clone_identity() {
    let repository = repository();
    let linked = tempfile::tempdir().unwrap();
    let linked_path = linked.path().join("linked tree");
    git(
        repository.path(),
        &[
            "worktree",
            "add",
            "-qb",
            "linked",
            linked_path.to_str().unwrap(),
        ],
    );
    let (store, _db) = store().await;
    let root = store.resolve_workspace(repository.path()).await.unwrap();
    let linked = store.resolve_workspace(&linked_path).await.unwrap();
    assert_eq!(root.project_id, linked.project_id);
    assert_ne!(root.workspace_id, linked.workspace_id);
    std::fs::create_dir(linked_path.join("nested space")).unwrap();
    let nested = store
        .resolve_workspace(&linked_path.join("nested space"))
        .await
        .unwrap();
    assert_eq!(nested.workspace_id, linked.workspace_id);
    assert_eq!(nested.relative_cwd, Path::new("nested space"));
    let clone = tempfile::tempdir().unwrap();
    git(
        clone.path(),
        &[
            "clone",
            "-q",
            repository.path().to_str().unwrap(),
            "independent",
        ],
    );
    let cloned = store
        .resolve_workspace(&clone.path().join("independent"))
        .await
        .unwrap();
    assert_ne!(cloned.project_id, root.project_id);
}

#[cfg(unix)]
#[tokio::test]
async fn test_worktree_symlink_discovery_reuses_identity_but_binding_escape_is_rejected() {
    let repo = repository();
    let aliases = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(repo.path(), aliases.path().join("alias")).unwrap();
    let (store, _db) = store().await;
    let original = store.resolve_workspace(repo.path()).await.unwrap();
    let alias = store
        .resolve_workspace(&aliases.path().join("alias"))
        .await
        .unwrap();
    assert_eq!(original, alias);
    std::fs::create_dir(repo.path().join("sub")).unwrap();
    let (id, _) = bound(&store, &repo.path().join("sub")).await;
    std::fs::remove_dir(repo.path().join("sub")).unwrap();
    std::os::unix::fs::symlink(aliases.path(), repo.path().join("sub")).unwrap();
    let error = store.validate_session_binding(&id).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::NeedsRelink)
    ));
}

#[tokio::test]
async fn test_worktree_missing_recreated_and_moved_locations_never_rebind() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let (store, _db) = store().await;
    let (id, _) = bound(&store, &root).await;
    let moved = directory.path().join("moved");
    std::fs::rename(&root, &moved).unwrap();
    assert!(matches!(
        store
            .validate_session_binding(&id)
            .await
            .unwrap_err()
            .downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::Unavailable)
    ));
    assert!(matches!(
        store
            .resolve_workspace(&moved)
            .await
            .unwrap_err()
            .downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::NeedsRelink)
    ));
    std::fs::create_dir(&root).unwrap();
    assert!(matches!(
        store
            .resolve_workspace(&root)
            .await
            .unwrap_err()
            .downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::NeedsRelink)
    ));
    assert!(store.load_messages(&id).await.unwrap().is_empty());
}

#[tokio::test]
async fn test_worktree_new_nested_repository_invalidates_original_binding() {
    let repo = repository();
    let nested = repo.path().join("sub");
    std::fs::create_dir(&nested).unwrap();
    let (store, _db) = store().await;
    let (id, _) = bound(&store, &nested).await;
    git(&nested, &["init", "-q"]);
    assert!(matches!(
        store
            .validate_session_binding(&id)
            .await
            .unwrap_err()
            .downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::NeedsRelink)
    ));
}

#[tokio::test]
async fn test_worktree_concurrent_registration_reuses_winner() {
    let repo = repository();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("concurrent.db");
    let (left, right) = tokio::join!(SqliteThreadStore::new(&path), SqliteThreadStore::new(&path));
    let left = left.unwrap();
    let right = right.unwrap();
    let (left, right) = tokio::join!(
        left.resolve_workspace(repo.path()),
        right.resolve_workspace(repo.path())
    );
    assert_eq!(left.unwrap(), right.unwrap());
}

#[tokio::test]
async fn test_worktree_scoped_pages_and_exact_directory_are_lightweight() {
    let repo = repository();
    let sub = repo.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let (store, _db) = store().await;
    let (root_id, root) = bound(&store, repo.path()).await;
    let (sub_id, sub) = bound(&store, &sub).await;
    let root_lease = store.acquire_execution_lease(&root_id).await.unwrap();
    let sub_lease = store.acquire_execution_lease(&sub_id).await.unwrap();
    store
        .append_message(&root_id, BaseMessage::human("root history"))
        .await
        .unwrap();
    store
        .append_message(&sub_id, BaseMessage::human("sub history"))
        .await
        .unwrap();
    // Deliberately corrupt large owner blobs; listing never decodes or aggregates them.
    sqlx::query("UPDATE threads SET frozen_context = 'broken', cached_context = 'broken'")
        .execute(&store.pool)
        .await
        .unwrap();
    let first = store
        .list_scoped_threads(&ScopedThreadQuery {
            scope: ThreadScope::Project(root.project_id),
            cursor: None,
            limit: 1,
        })
        .await
        .unwrap();
    let second = store
        .list_scoped_threads(&ScopedThreadQuery {
            scope: ThreadScope::Project(root.project_id),
            cursor: first.next_cursor.clone(),
            limit: 1,
        })
        .await
        .unwrap();
    assert_eq!(first.entries.len(), 1);
    assert_eq!(second.entries.len(), 1);
    assert_ne!(first.entries[0].thread.id, second.entries[0].thread.id);
    assert!(second.next_cursor.is_none());
    let exact = store
        .list_scoped_threads(&ScopedThreadQuery {
            scope: ThreadScope::ExactDirectory {
                workspace_id: sub.workspace_id,
                relative_cwd: sub.relative_cwd,
            },
            cursor: None,
            limit: 20,
        })
        .await
        .unwrap();
    assert_eq!(exact.entries.len(), 1);
    assert_eq!(exact.entries[0].thread.id, sub_id);
    root_lease.mark_clean().await.unwrap();
    sub_lease.mark_clean().await.unwrap();
}

#[tokio::test]
async fn test_worktree_bound_writes_require_owner_and_metadata_cannot_rebind() {
    let repo = repository();
    let (store, db) = store().await;
    let (id, resolved) = bound(&store, repo.path()).await;
    let error = store
        .append_message(&id, BaseMessage::human("unauthorized"))
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::ExecutionLeaseRequired)
    ));
    let lease = store.acquire_execution_lease(&id).await.unwrap();
    let other = SqliteThreadStore::new(db.path().join("threads.db"))
        .await
        .unwrap();
    assert!(matches!(
        other
            .append_message(&id, BaseMessage::human("other host"))
            .await
            .unwrap_err()
            .downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::ExecutionLeaseRequired)
    ));
    let mut changed = store.load_meta(&id).await.unwrap();
    changed.cwd = "/different".into();
    assert!(matches!(
        store
            .update_meta(&id, changed)
            .await
            .unwrap_err()
            .downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::ExecutionBindingMismatch)
    ));
    let mut child = ThreadMeta::new(resolved.cwd.to_str().unwrap());
    child.parent_thread_id = Some(id.clone());
    child.hidden = true;
    let child = store.create_bound_thread(child, &resolved).await.unwrap();
    store
        .append_message(&child, BaseMessage::human("owned child"))
        .await
        .unwrap();
    lease.mark_clean().await.unwrap();
    assert!(matches!(
        store
            .append_message(&child, BaseMessage::human("closed"))
            .await
            .unwrap_err()
            .downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::ExecutionLeaseRequired)
    ));
}

#[tokio::test]
async fn test_worktree_binding_keeps_wire_revision_without_persisted_column() {
    let repo = repository();
    let cwd = repo.path().join("nested space");
    std::fs::create_dir(&cwd).unwrap();
    let (store, _db) = store().await;
    let (revision_columns,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM pragma_table_info('session_bindings') WHERE name = 'revision'",
    )
    .fetch_one(&store.pool)
    .await
    .unwrap();
    assert_eq!(revision_columns, 0);

    let (id, workspace) = bound(&store, &cwd).await;
    let lease = store.acquire_execution_lease(&id).await.unwrap();
    store
        .append_message(&id, BaseMessage::human("bound history"))
        .await
        .unwrap();
    lease.mark_clean().await.unwrap();
    let expected = SessionBinding {
        schema_version: SESSION_BINDING_VERSION,
        revision: 1,
        project_id: workspace.project_id,
        workspace_id: workspace.workspace_id,
        cwd_relative_to_workspace: workspace.relative_cwd.clone(),
    };
    let loaded = store.load_session_binding(&id).await.unwrap().unwrap();
    assert_eq!(loaded, expected);
    assert_eq!(serde_json::to_value(&loaded).unwrap()["revision"], 1);
    assert_eq!(
        store.validate_session_binding(&id).await.unwrap(),
        workspace
    );

    let page = store
        .list_scoped_threads(&ScopedThreadQuery {
            scope: ThreadScope::Project(workspace.project_id),
            cursor: None,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].thread.id, id);
    assert_eq!(page.entries[0].binding, expected);
    assert_eq!(page.entries[0].effective_cwd, workspace.cwd);
    assert_eq!(page.entries[0].workspace_root, workspace.root);
    store.close().await;
}

#[tokio::test]
async fn test_worktree_binding_survives_clean_reopen_and_unknown_versions_fail_closed() {
    let repo = repository();
    let (store, db) = store().await;
    let (id, expected) = bound(&store, repo.path()).await;
    let lease = store.acquire_execution_lease(&id).await.unwrap();
    lease.mark_clean().await.unwrap();
    drop(lease);
    store.close().await;
    let reopened = SqliteThreadStore::new(db.path().join("threads.db"))
        .await
        .unwrap();
    assert_eq!(
        reopened.validate_session_binding(&id).await.unwrap(),
        expected
    );
    sqlx::query("UPDATE session_bindings SET schema_version = 99 WHERE thread_id = ?")
        .bind(&id)
        .execute(&reopened.pool)
        .await
        .unwrap();
    assert!(matches!(
        reopened
            .load_session_binding(&id)
            .await
            .unwrap_err()
            .downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::InvalidBinding)
    ));
}

#[tokio::test]
async fn test_worktree_incompatible_write_open_rejects_without_modifying_old_file() {
    use sqlx::Connection;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    sqlx::query("CREATE TABLE threads (id TEXT PRIMARY KEY)")
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    let before = std::fs::read(&path).unwrap();
    let error = SqliteThreadStore::new(&path).await.err().unwrap();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::UnsupportedDatabaseSchema)
    ));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!path.with_extension("db-wal").exists());
}

#[tokio::test]
async fn test_worktree_read_only_history_never_creates_execution_sidecar_or_mutates_database() {
    let repo = repository();
    let (store, db) = store().await;
    let (id, _) = bound(&store, repo.path()).await;
    store.close().await;
    let path = db.path().join("threads.db");
    let before = std::fs::read(&path).unwrap();
    let read = SqliteThreadStore::open_existing_read_only(&path)
        .await
        .unwrap();
    assert!(read.load_session_binding(&id).await.unwrap().is_some());
    assert!(read.load_context(&id).await.unwrap().is_empty());
    read.close().await;
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!db.path().join("threads.db.execution-locks").exists());
}

fn lease_process(path: &Path, id: &str, expected: &str) {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "sessions::sqlite_store::workspace::tests::test_worktree_execution_child_process",
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_worktree_execution_competes_across_processes_and_crash_remains_dirty() {
    let repo = repository();
    let (store, db) = store().await;
    let (id, _) = bound(&store, repo.path()).await;
    let path = db.path().join("threads.db");
    let lease = store.acquire_execution_lease(&id).await.unwrap();
    lease_process(&path, &id, "busy");
    lease.mark_clean().await.unwrap();
    lease_process(&path, &id, "clean");
    lease_process(&path, &id, "crash");
    let error = store.acquire_execution_lease(&id).await.err().unwrap();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::RecoveryRequired)
    ));
    lease_process(&path, &id, "recovery");
}

#[tokio::test]
async fn test_worktree_execution_child_process() {
    let Ok(path) = std::env::var("PERI_TEST_WORKSPACE_DB") else {
        return;
    };
    let id = std::env::var("PERI_TEST_WORKSPACE_ID").unwrap();
    let expected = std::env::var("PERI_TEST_WORKSPACE_EXPECT").unwrap();
    let store = SqliteThreadStore::new(path).await.unwrap();
    let result = store.acquire_execution_lease(&id).await;
    match expected.as_str() {
        "busy" => assert!(matches!(
            result.err().unwrap().downcast_ref::<WorkspaceError>(),
            Some(WorkspaceError::ExecutionBusy)
        )),
        "recovery" => assert!(matches!(
            result.err().unwrap().downcast_ref::<WorkspaceError>(),
            Some(WorkspaceError::RecoveryRequired)
        )),
        "clean" => result.unwrap().mark_clean().await.unwrap(),
        "crash" => {
            let _lease = result.unwrap();
            std::process::exit(0);
        }
        _ => panic!("unknown expected child result"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_worktree_clean_waits_for_admitted_mutation_before_releasing_os_ownership() {
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    let repo = repository();
    let (store, db) = store().await;
    let (id, _) = bound(&store, repo.path()).await;
    let lease = store.acquire_execution_lease(&id).await.unwrap();
    let mutation = store.require_execution_lease(&id).await.unwrap().unwrap();
    let mut close = std::pin::pin!(lease.mark_clean());
    // Poll once with an admitted mutation suspended: close must not publish clean.
    assert!(matches!(
        close.as_mut().poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    lease_process(&db.path().join("threads.db"), &id, "busy");
    sqlx::query("UPDATE threads SET title = 'last owner write' WHERE id = ?")
        .bind(&id)
        .execute(&store.pool)
        .await
        .unwrap();
    let run: (bool,) = sqlx::query_as("SELECT clean FROM execution_runs WHERE thread_id = ?")
        .bind(&id)
        .fetch_one(&store.pool)
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
    let mutation = store.require_execution_lease(&id).await.unwrap().unwrap();
    // Dropping the capability without its completion signal models future cancellation.
    drop(mutation);
    let error = lease.mark_clean().await.unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::RecoveryRequired)
    ));
    drop(lease);
    let error = store.acquire_execution_lease(&id).await.err().unwrap();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::RecoveryRequired)
    ));
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
    let files = std::fs::read_dir(db.path().join("threads.db.execution-locks"))
        .unwrap()
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 1);
    assert_eq!(
        files[0]
            .as_ref()
            .unwrap()
            .file_name()
            .to_str()
            .unwrap()
            .len(),
        69
    );
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
