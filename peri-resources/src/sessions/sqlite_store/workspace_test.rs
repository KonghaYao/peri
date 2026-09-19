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

/// [回归测试] 普通目录登记后出现 `.git`：工作区身份是目录对象本身，Git 布局是
/// 它的派生观测。同一目录对象必须继续可解析，且项目 / 工作区标识与历史绑定不变。
#[tokio::test]
async fn test_worktree_directory_gaining_repository_keeps_registration() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project");
    let nested = root.join("sub");
    std::fs::create_dir_all(&nested).unwrap();
    let (store, _db) = store().await;
    let (root_id, registered) = bound(&store, &root).await;
    let (nested_id, nested_workspace) = bound(&store, &nested).await;
    assert_ne!(nested_workspace.workspace_id, registered.workspace_id);

    git(&root, &["init", "-q"]);

    // The registered directory object keeps its identity instead of failing closed.
    assert_eq!(store.resolve_workspace(&root).await.unwrap(), registered);
    // Subdirectories now resolve into that same repository workspace.
    let nested = store.resolve_workspace(&nested).await.unwrap();
    assert_eq!(nested.workspace_id, registered.workspace_id);
    assert_eq!(nested.project_id, registered.project_id);
    assert_eq!(nested.relative_cwd, Path::new("sub"));
    // The existing session is neither rebound nor hidden, and new sessions work.
    assert_eq!(
        store.validate_session_binding(&root_id).await.unwrap(),
        registered
    );
    let (fresh, fresh_workspace) = bound(&store, &root).await;
    assert_ne!(fresh, root_id);
    assert_eq!(fresh_workspace, registered);
    // The session registered inside the directory that became a repository root
    // keeps its history but no longer executes there; the layout change is not
    // silently rewritten into a different workspace.
    assert!(matches!(
        store
            .validate_session_binding(&nested_id)
            .await
            .unwrap_err()
            .downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::NeedsRelink)
    ));
}

/// [回归测试] 已登记仓库移除 `.git`：根目录对象仍然相同，注册继续可用。
#[tokio::test]
async fn test_worktree_repository_losing_git_keeps_registration() {
    let repo = repository();
    let nested = repo.path().join("sub");
    std::fs::create_dir(&nested).unwrap();
    let (store, _db) = store().await;
    let (id, registered) = bound(&store, repo.path()).await;
    assert_eq!(
        store.resolve_workspace(&nested).await.unwrap().workspace_id,
        registered.workspace_id
    );

    std::fs::remove_dir_all(repo.path().join(".git")).unwrap();

    assert_eq!(
        store.resolve_workspace(repo.path()).await.unwrap(),
        registered
    );
    assert_eq!(
        store.validate_session_binding(&id).await.unwrap(),
        registered
    );
    let _ = bound(&store, repo.path()).await;
    // Without a repository each directory is again its own workspace; the
    // subdirectory no longer belongs to the registered root workspace.
    let separate = store.resolve_workspace(&nested).await.unwrap();
    assert_ne!(separate.workspace_id, registered.workspace_id);
    assert_eq!(separate.relative_cwd, Path::new(""));
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
    assert_eq!(page.entries[0].binding, Some(expected));
    assert_eq!(page.entries[0].effective_cwd, workspace.cwd);
    assert_eq!(page.entries[0].workspace_root, Some(workspace.root));
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

/// 同 `lease_process`，额外把目标代次传给子进程（reset/观测用）。
fn lease_process_at(path: &Path, id: &str, expected: &str, generation: i64) {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "sessions::sqlite_store::workspace::tests::test_worktree_execution_child_process",
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_worktree_execution_competes_across_processes_and_crash_remains_dirty() {
    let repo = repository();
    let (store, db) = store().await;
    let (id, _) = bound(&store, repo.path()).await;
    let path = db.path().join("threads.db");
    let lease = store.acquire_execution_lease(&id).await.unwrap();
    lease_process(&path, &id, "busy");
    lease_process(&path, &id, "reset_busy");
    lease.mark_clean().await.unwrap();
    lease_process(&path, &id, "clean");
    lease_process(&path, &id, "crash");
    let error = store.acquire_execution_lease(&id).await.err().unwrap();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::RecoveryRequired(_))
    ));
    lease_process(&path, &id, "recovery");
    let WorkspaceError::RecoveryRequired(target) = error.downcast_ref::<WorkspaceError>().unwrap()
    else {
        unreachable!()
    };
    store.reset_dirty_execution(target).await.unwrap();
    let next = store.acquire_execution_lease(&id).await.unwrap();
    assert_eq!(next.thread_id(), &id);
    next.mark_clean().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_worktree_dirty_reset_held_stale_and_exact_generation() {
    let repo = repository();
    let (store, db) = store().await;
    let (id, _) = bound(&store, repo.path()).await;
    let path = db.path().join("threads.db");
    let before = store.load_session_binding(&id).await.unwrap();

    // 活 owner：本进程持有稳定锁期间，另一进程只能报忙，不得解除 dirty。
    let lease = store.acquire_execution_lease(&id).await.unwrap();
    lease_process_at(&path, &id, "reset_busy", 1);
    drop(lease);

    // 用户接受风险后的精确解除：只清这一代。
    lease_process_at(&path, &id, "reset_ok", 1);
    // 同代重复确认不得再写（已是 clean），过期代次必须拒绝。
    lease_process_at(&path, &id, "reset_stale", 1);

    // 之后的正常取得所有权只推进代次并保持 dirty，不伪造 clean。
    lease_process(&path, &id, "crash");
    lease_process_at(&path, &id, "recovery_gen", 2);
    // 过期代次不能解掉新代（防止确认旧代次时误清新代）。
    lease_process_at(&path, &id, "reset_stale", 1);
    lease_process_at(&path, &id, "recovery_gen", 2);
    // 精确解除新代后，原 ThreadId 仍可正常取得所有权并收尾。
    lease_process_at(&path, &id, "reset_ok", 2);
    lease_process(&path, &id, "clean");

    assert_eq!(store.load_session_binding(&id).await.unwrap(), before);
}

#[tokio::test]
async fn test_worktree_execution_child_process() {
    let Ok(path) = std::env::var("PERI_TEST_WORKSPACE_DB") else {
        return;
    };
    let id = std::env::var("PERI_TEST_WORKSPACE_ID").unwrap();
    let expected = std::env::var("PERI_TEST_WORKSPACE_EXPECT").unwrap();
    let store = SqliteThreadStore::new(path).await.unwrap();
    let generation = std::env::var("PERI_TEST_WORKSPACE_GENERATION")
        .ok()
        .and_then(|value| value.parse::<i64>().ok());
    let target = || RecoveryRequiredDetails {
        thread_id: id.clone(),
        generation: generation.expect("generation required"),
    };
    // reset/观测分支只做目标操作，不能先自行持锁（否则与自身 try_lock 冲突）。
    match expected.as_str() {
        "busy" => assert!(matches!(
            store
                .acquire_execution_lease(&id)
                .await
                .err()
                .unwrap()
                .downcast_ref::<WorkspaceError>(),
            Some(WorkspaceError::ExecutionBusy)
        )),
        "recovery" => assert!(matches!(
            store
                .acquire_execution_lease(&id)
                .await
                .err()
                .unwrap()
                .downcast_ref::<WorkspaceError>(),
            Some(WorkspaceError::RecoveryRequired(_))
        )),
        "recovery_gen" => {
            let error = store.acquire_execution_lease(&id).await.err().unwrap();
            let Some(WorkspaceError::RecoveryRequired(details)) =
                error.downcast_ref::<WorkspaceError>()
            else {
                panic!("expected dirty generation, got: {error:?}");
            };
            assert_eq!(details.generation, generation.expect("generation required"));
        }
        "clean" => store
            .acquire_execution_lease(&id)
            .await
            .unwrap()
            .mark_clean()
            .await
            .unwrap(),
        "crash" => {
            let _lease = store.acquire_execution_lease(&id).await.unwrap();
            std::process::exit(0);
        }
        "reset_busy" => {
            let error = store
                .reset_dirty_execution(&RecoveryRequiredDetails {
                    thread_id: id,
                    generation: generation.unwrap_or(1),
                })
                .await
                .unwrap_err();
            assert!(
                matches!(
                    error.downcast_ref::<WorkspaceError>(),
                    Some(WorkspaceError::ExecutionBusy)
                ),
                "expected busy rejection, got: {error:?}"
            );
        }
        "reset_ok" => store.reset_dirty_execution(&target()).await.unwrap(),
        "reset_stale" => {
            let error = store.reset_dirty_execution(&target()).await.unwrap_err();
            assert!(
                matches!(
                    error.downcast_ref::<WorkspaceError>(),
                    Some(WorkspaceError::RecoveryGenerationMismatch)
                ),
                "expected stale generation rejection, got: {error:?}"
            );
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
        Some(WorkspaceError::RecoveryRequired(_))
    ));
    drop(lease);
    let error = store.acquire_execution_lease(&id).await.err().unwrap();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::RecoveryRequired(_))
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

// ── 外部探测的时机与次数：写事务内不得执行外部进程 ──

/// 假 Git 的日志：每次调用追加一行，记录调用当时另一连接能否立刻取得写锁。
const PROBE_LOG: &str = "PERI_TEST_PROBE_LOG";
const PROBE_DATABASE: &str = "PERI_TEST_PROBE_DB";
/// `git-not-repository` 时伪装真实 Git 在非仓库目录下的回答（stderr + 退出码）。
const PROBE_BEHAVIOR: &str = "PERI_TEST_PROBE_BEHAVIOR";
const ADMISSION_DATABASE: &str = "PERI_TEST_ADMISSION_DB";
const ADMISSION_CWD: &str = "PERI_TEST_ADMISSION_CWD";

const PROBE_GIT_CHILD: &str =
    "sessions::sqlite_store::workspace::tests::test_worktree_probe_git_child";
const ADMISSION_CHILD: &str =
    "sessions::sqlite_store::workspace::tests::test_worktree_registration_admission_child";

/// 子进程模式：作为假 Git 被调用，先记录调用当时写锁是否空闲。
#[tokio::test]
async fn test_worktree_probe_git_child() {
    let Ok(log) = std::env::var(PROBE_LOG) else {
        return;
    };
    let database = std::env::var(PROBE_DATABASE).unwrap();
    let state = write_lock_available(&database).await;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .unwrap();
    std::io::Write::write_all(&mut file, format!("{state}\n").as_bytes()).unwrap();
    if std::env::var(PROBE_BEHAVIOR).as_deref() == Ok("git-not-repository") {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        std::process::exit(128);
    }
}

/// 另一连接尝试立刻取得写锁：成功即说明此刻没有写事务持有者。
async fn write_lock_available(database: &str) -> &'static str {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(database)
                .busy_timeout(std::time::Duration::ZERO),
        )
        .await
        .unwrap();
    let mut connection = pool.acquire().await.unwrap();
    let acquired = sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *connection)
        .await
        .is_ok();
    if acquired {
        sqlx::query("ROLLBACK")
            .execute(&mut *connection)
            .await
            .unwrap();
    }
    drop(connection);
    pool.close().await;
    if acquired {
        "free"
    } else {
        "busy"
    }
}

/// 子进程模式：以受控 PATH 执行一次完整准入（解析 + 绑定）。
#[tokio::test]
async fn test_worktree_registration_admission_child() {
    let Ok(database) = std::env::var(ADMISSION_DATABASE) else {
        return;
    };
    let cwd = std::env::var(ADMISSION_CWD).unwrap();
    let store = SqliteThreadStore::new(Path::new(&database)).await.unwrap();
    let workspace = store.resolve_workspace(Path::new(&cwd)).await.unwrap();
    store
        .create_bound_thread(ThreadMeta::new(cwd.as_str()), &workspace)
        .await
        .unwrap();
}

#[cfg(unix)]
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

#[cfg(unix)]
fn real_git_path() -> std::path::PathBuf {
    let output = std::process::Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    assert!(output.status.success(), "测试环境需要真实 Git");
    std::path::PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
}

#[cfg(unix)]
async fn binding_count(database: &Path) -> i64 {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(database))
        .await
        .unwrap();
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM session_bindings")
        .fetch_one(&pool)
        .await
        .unwrap();
    pool.close().await;
    row.0
}

/// 在受控 PATH 下跑一次准入子进程，返回假 Git 记录的调用序列。
///
/// `real_git` 为 `None` 时假 Git 直接扮演「明确回答不是仓库」的 Git；为 `Some`
/// 时先记录再转交真实 Git，用于统计真实仓库下的调用次数。
#[cfg(unix)]
fn probe_admission(
    directory: &Path,
    work: &Path,
    behavior: &str,
    real_git: Option<&Path>,
) -> (Vec<String>, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let database = directory.join("threads.db");
    let log = directory.join("probe.log");
    let shim = directory.join("bin");
    std::fs::create_dir(&shim).unwrap();
    let binary = std::env::current_exe().unwrap();
    // 假 Git 的 stdout 是发现过程解析的对象，测试框架的横幅不能混进去；
    // 角色扮演所需的 stderr 与退出码仍由子进程自己给出。
    let probe = format!(
        "{} --exact {PROBE_GIT_CHILD} --nocapture",
        shell_quote(&binary)
    );
    let script = match real_git {
        Some(real) => format!(
            "#!/bin/sh\n{probe} >/dev/null\nexec {} \"$@\"\n",
            shell_quote(real)
        ),
        None => format!("#!/bin/sh\nexec {probe} >/dev/null\n"),
    };
    let git = shim.join("git");
    std::fs::write(&git, script).unwrap();
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = std::process::Command::new(&binary)
        .args(["--exact", ADMISSION_CHILD, "--nocapture"])
        .env("PATH", &shim)
        .env(ADMISSION_DATABASE, &database)
        .env(ADMISSION_CWD, work)
        .env(PROBE_LOG, &log)
        .env(PROBE_DATABASE, &database)
        .env(PROBE_BEHAVIOR, behavior)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "准入子进程失败：{}",
        String::from_utf8_lossy(&output.stderr),
    );
    let recorded = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    (recorded, database)
}

/// [回归测试] 准入的外部探测必须全部发生在写事务之外。
///
/// 上一版实现把完整 Git 发现放在 `BEGIN IMMEDIATE` 内，并在一次准入里重复四轮：
/// 写锁持有期间等待 Git 子进程，慢盘 / 慢 Git 会阻塞同一数据库上的其他 writer。
/// 假 Git 在每次被调用时尝试立刻取得写锁，把「探测是否在事务外」变成可断言的事实。
#[cfg(unix)]
#[tokio::test]
async fn test_worktree_registration_probes_filesystem_outside_write_lock() {
    let directory = tempfile::tempdir().unwrap();
    let work = directory.path().join("plain-project");
    std::fs::create_dir(&work).unwrap();
    let (recorded, database) = probe_admission(directory.path(), &work, "git-not-repository", None);
    assert!(
        !recorded.is_empty(),
        "假 Git 未被调用，用例没有覆盖准入路径"
    );
    assert!(
        recorded.iter().all(|state| state == "free"),
        "写事务持有期间不得执行外部探测，实际记录：{recorded:?}",
    );
    assert_eq!(
        recorded.len(),
        2,
        "目录模式一次准入只应观测两轮（解析 + 绑定复核）：{recorded:?}",
    );
    assert_eq!(
        binding_count(&database).await,
        1,
        "用例必须真的完成了一次登记"
    );
}

/// [回归测试] 仓库模式一次准入的真实 Git 调用次数保持有界：两轮观测，每轮三条命令。
#[cfg(unix)]
#[tokio::test]
async fn test_worktree_repository_registration_keeps_git_calls_bounded() {
    let repository = repository();
    let directory = tempfile::tempdir().unwrap();
    let (recorded, database) = probe_admission(
        directory.path(),
        repository.path(),
        "log-only",
        Some(&real_git_path()),
    );
    assert!(
        recorded.iter().all(|state| state == "free"),
        "写事务持有期间不得执行外部探测，实际记录：{recorded:?}",
    );
    assert_eq!(
        recorded.len(),
        6,
        "仓库模式一次准入的 Git 调用次数应有界：{recorded:?}",
    );
    assert_eq!(
        binding_count(&database).await,
        1,
        "用例必须真的完成了一次登记"
    );
}
