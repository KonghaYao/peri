use super::super::*;
use peri_acp_types::workspace::*;
#[cfg(unix)]
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
#[cfg(unix)]
use std::time::{Duration, Instant};
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

/// 建立绑定会话并写入一条消息，使历史进入列表查询的可见范围。
async fn bound_with_history(
    store: &SqliteThreadStore,
    cwd: &Path,
) -> (ThreadId, ResolvedWorkspace) {
    let (id, workspace) = bound(store, cwd).await;
    let lease = store.acquire_execution_lease(&id).await.unwrap();
    store
        .append_message(&id, BaseMessage::human("bound history"))
        .await
        .unwrap();
    lease.mark_clean().await.unwrap();
    (id, workspace)
}

/// 断言该会话仍能在原项目范围里被列出（历史可见，不被隐藏或改绑）。
async fn assert_only_history(store: &SqliteThreadStore, project: ProjectId, thread: &ThreadId) {
    let page = store
        .list_scoped_threads(&ScopedThreadQuery {
            scope: ThreadScope::Project(project),
            cursor: None,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(&page.entries[0].thread.id, thread);
}

/// [回归测试] 目录对象被替换后，新会话仍必须可建立。
///
/// 登记键是 (canonical root, 该目录的文件对象证据) 组合：同一路径上的新对象不命中
/// 原登记，但它仍是可访问的目录，必须得到新的项目与工作区登记；旧绑定按各自登记
/// 证据复核，继续失败关闭，历史不被改绑或隐藏。
///
/// 文件对象证据只有 device/inode，删除后重建时文件系统可能复用刚释放的 inode，
/// 那种情况下新旧对象在证据上等同（设计 §8：不依赖 creation time）。所以这里让
/// 旧对象改名到另一路径继续存活，新对象才确定是一个不同的对象，断言不随分配策略
/// 摆动。
#[tokio::test]
async fn test_worktree_replaced_directory_registers_new_workspace_keeps_old_history() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let (store, _db) = store().await;
    let (old_thread, registered) = bound_with_history(&store, &root).await;

    // 同一路径上换成一个新的文件对象：路径可用不等于身份延续。
    std::fs::rename(&root, directory.path().join("replaced")).unwrap();
    std::fs::create_dir(&root).unwrap();

    // 旧会话不再可执行，但历史仍可见且绑定没有被改写。
    assert!(matches!(
        store
            .validate_session_binding(&old_thread)
            .await
            .unwrap_err()
            .downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::NeedsRelink)
    ));
    assert_only_history(&store, registered.project_id, &old_thread).await;
    let binding = store
        .load_session_binding(&old_thread)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(binding.workspace_id, registered.workspace_id);
    assert_eq!(binding.project_id, registered.project_id);

    // 新对象得到独立登记，新会话可建，执行目录就是该路径。
    let (replacement_thread, replacement) = bound(&store, &root).await;
    assert_ne!(replacement.workspace_id, registered.workspace_id);
    assert_ne!(replacement.project_id, registered.project_id);
    assert_eq!(
        replacement.cwd,
        tokio::fs::canonicalize(&root).await.unwrap()
    );
    assert_eq!(
        store
            .validate_session_binding(&replacement_thread)
            .await
            .unwrap(),
        replacement
    );
}

/// [回归测试] 目录换位后，新路径仍必须可建立新会话。
#[tokio::test]
async fn test_worktree_moved_directory_registers_new_path_keeps_old_history() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let (store, _db) = store().await;
    let (old_thread, registered) = bound_with_history(&store, &root).await;

    let moved = directory.path().join("moved");
    std::fs::rename(&root, &moved).unwrap();

    // 旧路径消失：旧会话不可执行，历史保留。
    assert!(matches!(
        store
            .validate_session_binding(&old_thread)
            .await
            .unwrap_err()
            .downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::Unavailable)
    ));
    assert_only_history(&store, registered.project_id, &old_thread).await;

    // 新路径可建会话，且不把旧登记改写到新位置。
    let (relocated_thread, relocated) = bound(&store, &moved).await;
    assert_ne!(relocated.workspace_id, registered.workspace_id);
    assert_ne!(relocated.project_id, registered.project_id);
    assert_eq!(
        relocated.cwd,
        tokio::fs::canonicalize(&moved).await.unwrap()
    );
    assert_eq!(
        store
            .validate_session_binding(&relocated_thread)
            .await
            .unwrap(),
        relocated
    );
}

/// [回归测试] linked worktree 换位后：同一项目复用，新工作区独立，旧路径会话收历史。
///
/// `git worktree move` 改变的是工作区位置，common directory 与项目证据未变。
#[tokio::test]
async fn test_worktree_moved_linked_worktree_reuses_project_registers_new_workspace() {
    let repository = repository();
    let linked = tempfile::tempdir().unwrap();
    let original = linked.path().join("linked tree");
    git(
        repository.path(),
        &[
            "worktree",
            "add",
            "-qb",
            "linked",
            original.to_str().unwrap(),
        ],
    );
    let (store, _db) = store().await;
    let (old_thread, registered) = bound_with_history(&store, &original).await;

    let moved = linked.path().join("moved tree");
    git(
        repository.path(),
        &[
            "worktree",
            "move",
            original.to_str().unwrap(),
            moved.to_str().unwrap(),
        ],
    );

    assert!(matches!(
        store
            .validate_session_binding(&old_thread)
            .await
            .unwrap_err()
            .downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::Unavailable)
    ));

    let (relocated_thread, relocated) = bound(&store, &moved).await;
    assert_ne!(relocated.workspace_id, registered.workspace_id);
    assert_eq!(relocated.project_id, registered.project_id);
    assert_eq!(
        store
            .validate_session_binding(&relocated_thread)
            .await
            .unwrap(),
        relocated
    );
}

/// [回归测试] 同一文件对象在同一路径只登记一次，冲突仍由唯一约束挡住。
#[tokio::test]
async fn test_worktree_registration_reuses_exact_object_and_keeps_rows_unique() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let (store, _db) = store().await;
    let (_, registered) = bound(&store, &root).await;
    let resolved = store.resolve_workspace(&root).await.unwrap();
    assert_eq!(resolved, registered);
    let duplicate = sqlx::query(
        "INSERT INTO workspaces (id, project_id, root, root_identity, discovery)
         SELECT 'duplicate', project_id, root, root_identity, discovery FROM workspaces WHERE id = ?",
    )
    .bind(registered.workspace_id.to_string())
    .execute(&store.pool)
    .await;
    assert!(
        duplicate.is_err(),
        "同一 (root, root_identity) 不得重复登记"
    );
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
async fn test_worktree_binding_cwd_text_matches_registration_without_trailing_separator() {
    let repo = repository();
    let nested = repo.path().join("sub");
    std::fs::create_dir(&nested).unwrap();
    let (store, _db) = store().await;

    // 工作区根与子目录各建一个绑定：两者的执行目录文本都必须与解析结果一致。
    // `root.join("")` 会给出 `/a/b/` 这样的形式，与解析给出的 `/a/b` 只差一个
    // 分隔符；Path 比较看不出差别，按字符串比较目录的调用方会据此重跑完整发现。
    for (cwd, suffix) in [(repo.path().to_path_buf(), ""), (nested.clone(), "/sub")] {
        let (id, resolved) = bound(&store, &cwd).await;
        let registered = resolved.cwd.to_str().unwrap();
        assert!(
            registered.ends_with(suffix),
            "解析结果不符合预期：{registered}"
        );
        let revalidated = store.validate_session_binding(&id).await.unwrap();
        assert_eq!(revalidated.cwd.to_str().unwrap(), registered);
        let reasserted = store.reassert_session_binding(&id).await.unwrap();
        assert_eq!(reasserted.cwd.to_str().unwrap(), registered);
        let loaded = store.load_session_binding(&id).await.unwrap().unwrap();
        assert_eq!(
            loaded.cwd_relative_to_workspace,
            resolved.relative_cwd.to_path_buf()
        );
    }
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

/// 只读节点的工作区解析：命中已登记观测时不需要写入，登记因此仍可被读取（历史可浏
/// 览的前提）；未登记的目录无法登记，必须给出可诊断的原因而不是 SQL 层的只读报错。
#[tokio::test]
async fn test_worktree_read_only_resolves_registered_workspace_and_refuses_registration() {
    let repo = repository();
    let (store, db) = store().await;
    let registered = store.resolve_workspace(repo.path()).await.unwrap();
    store.close().await;

    let read = SqliteThreadStore::open_existing_read_only(db.path().join("threads.db"))
        .await
        .unwrap();
    assert_eq!(
        read.resolve_workspace(repo.path()).await.unwrap(),
        registered
    );
    let unregistered = tempfile::tempdir().unwrap();
    let error = read
        .resolve_workspace(unregistered.path())
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::ReadOnlyStore)
    ));
    let error = read
        .create_bound_thread(
            ThreadMeta::new(registered.cwd.to_str().unwrap()),
            &registered,
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::ReadOnlyStore)
    ));
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

/// 启动一个「短命持有者」子进程：取得所有权后打印就绪行、保持 `hold_ms` 再正常收尾。
///
/// 返回的句柄带 stdout 管道，父进程据此确定「锁已被持有」——`flock` 的持有者何时释放
/// 取决于调度，只有就绪信号之后的尝试才是确定性的重叠。
fn lease_process_hold(path: &Path, id: &str, hold_ms: u64) -> std::process::Child {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "sessions::sqlite_store::workspace::tests::test_worktree_execution_child_process",
            "--nocapture",
        ])
        .env("PERI_TEST_WORKSPACE_DB", path)
        .env("PERI_TEST_WORKSPACE_ID", id)
        .env("PERI_TEST_WORKSPACE_EXPECT", "hold")
        .env("PERI_TEST_WORKSPACE_HOLD_MS", hold_ms.to_string())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap()
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
        "hold" => {
            let lease = store.acquire_execution_lease(&id).await.unwrap();
            println!("HOLDER-READY");
            use std::io::Write as _;
            std::io::stdout().flush().unwrap();
            let hold_ms: u64 = std::env::var("PERI_TEST_WORKSPACE_HOLD_MS")
                .expect("hold duration required")
                .parse()
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(hold_ms));
            lease.mark_clean().await.unwrap();
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

/// [回归测试] 短命持有者释放后的取得所有权不得被误报成 ExecutionBusy。
///
/// `flock` 的锁挂在 open file description 上：本进程 `fork` 出的子进程在 `exec` 前共享父
/// 进程的描述符（`CLOEXEC` 只在子进程 `exec` 时关闭），会话生命周期里的 Git 发现、
/// `sw_vers`、LSP 等子进程因此会留下毫秒级的瞬时持有。没有重试时，这类窗口会被上报成
/// 「会话已被其他执行宿主占用」，把一次正常的取得所有权变成偶发失败；真正的外部持有者
/// 并不受重试影响（超时后仍报忙，见 `test_worktree_execution_competes_across_processes_and_crash_remains_dirty`）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_worktree_transient_holder_then_release_is_not_reported_busy() {
    const HOLD_MS: u64 = 250;
    use std::io::BufRead as _;
    let repo = repository();
    let (store, db) = store().await;
    let (id, _) = bound(&store, repo.path()).await;
    let mut holder = lease_process_hold(&db.path().join("threads.db"), &id, HOLD_MS);
    let mut lines = std::io::BufReader::new(holder.stdout.take().unwrap()).lines();
    assert!(
        lines.any(|line| line.unwrap().contains("HOLDER-READY")),
        "持有者未在尝试前就绪"
    );
    let started = std::time::Instant::now();
    let lease = store.acquire_execution_lease(&id).await.unwrap();
    // 证明这次成功来自等待而非抢先：取得所有权只能发生在持有者释放之后。
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(HOLD_MS / 2),
        "取得所有权发生在持有者释放之前：{:?}",
        started.elapsed()
    );
    lease.mark_clean().await.unwrap();
    assert!(holder.wait().unwrap().success());
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

/// 假 Git 的日志：每次调用追加一行，记录调用当时另一连接能否立刻取得写锁，
/// 以及本次调用的参数（用于把等待阶段对应到具体命令）。
const PROBE_LOG: &str = "PERI_TEST_PROBE_LOG";
const PROBE_DATABASE: &str = "PERI_TEST_PROBE_DB";
/// 假 Git 把本次调用的参数透传给记录者。
const PROBE_ARGS: &str = "PERI_TEST_PROBE_ARGS";
/// `git-not-repository` 时伪装真实 Git 在非仓库目录下的回答（stderr + 退出码）。
const PROBE_BEHAVIOR: &str = "PERI_TEST_PROBE_BEHAVIOR";
const ADMISSION_DATABASE: &str = "PERI_TEST_ADMISSION_DB";
const ADMISSION_CWD: &str = "PERI_TEST_ADMISSION_CWD";
const HISTORY_DATABASE: &str = "PERI_TEST_HISTORY_DB";
const HISTORY_THREAD: &str = "PERI_TEST_HISTORY_THREAD";
const VALIDATE_DATABASE: &str = "PERI_TEST_VALIDATE_DB";
const VALIDATE_THREAD: &str = "PERI_TEST_VALIDATE_THREAD";
const VALIDATE_CWD: &str = "PERI_TEST_VALIDATE_CWD";

#[cfg(unix)]
const PROBE_GIT_CHILD: &str =
    "sessions::sqlite_store::workspace::tests::test_worktree_probe_git_child";
#[cfg(unix)]
const ADMISSION_CHILD: &str =
    "sessions::sqlite_store::workspace::tests::test_worktree_registration_admission_child";
#[cfg(unix)]
const HISTORY_CHILD: &str =
    "sessions::sqlite_store::workspace::tests::test_worktree_history_access_child";
#[cfg(unix)]
const VALIDATE_CHILD: &str =
    "sessions::sqlite_store::workspace::tests::test_worktree_bound_session_validation_child";

/// 子进程模式：作为假 Git 被调用，先记录调用当时写锁是否空闲。
#[tokio::test]
async fn test_worktree_probe_git_child() {
    let Ok(log) = std::env::var(PROBE_LOG) else {
        return;
    };
    let database = std::env::var(PROBE_DATABASE).unwrap();
    let args = std::env::var(PROBE_ARGS).unwrap_or_default();
    let state = write_lock_available(&database).await;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .unwrap();
    std::io::Write::write_all(&mut file, format!("{state} {args}\n").as_bytes()).unwrap();
    if std::env::var(PROBE_BEHAVIOR).as_deref() == Ok("git-not-repository") {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        std::process::exit(128);
    }
}

/// 一行记录里的写锁状态（`free` / `busy`）。
#[cfg(unix)]
fn lock_state(line: &str) -> &str {
    line.split_whitespace().next().unwrap_or_default()
}

/// 一行记录里的调用参数（写锁状态之后的部分）。
#[cfg(unix)]
fn call_args(line: &str) -> &str {
    line.split_once(' ').map_or("", |(_, args)| args)
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

/// 子进程模式：以受控 PATH 执行一次完整准入（解析 + 绑定 + 取执行所有权 + 准入内复核）。
///
/// 这是 `session/new` 在存储层的形状：解析给出本次准入的观测，其余步骤只复核已记录
/// 证据。用例用它度量「一次准入的外部探测次数」。
#[tokio::test]
async fn test_worktree_registration_admission_child() {
    let Ok(database) = std::env::var(ADMISSION_DATABASE) else {
        return;
    };
    let cwd = std::env::var(ADMISSION_CWD).unwrap();
    let store = SqliteThreadStore::new(Path::new(&database)).await.unwrap();
    let workspace = store.resolve_workspace(Path::new(&cwd)).await.unwrap();
    let thread = store
        .create_bound_thread(ThreadMeta::new(cwd.as_str()), &workspace)
        .await
        .unwrap();
    let lease = store.acquire_execution_lease(&thread).await.unwrap();
    // 准入内复核（host 的 validate_expected / acquire_for_load 第二道检查）：
    // 复核结果必须与本次准入解析出的工作区一致。
    let reasserted = store.reassert_session_binding(&thread).await.unwrap();
    assert_eq!(reasserted, workspace);
    let identity = store.reassert_session_binding(&thread).await.unwrap();
    assert_eq!(identity.cwd, workspace.cwd);
    lease.mark_clean().await.unwrap();
}

/// 子进程模式：只读历史访问——列表、消息、frozen 与绑定读取，复核身份的动作不在其中。
#[tokio::test]
async fn test_worktree_history_access_child() {
    let Ok(database) = std::env::var(HISTORY_DATABASE) else {
        return;
    };
    let thread = std::env::var(HISTORY_THREAD).unwrap();
    for read_only in [false, true] {
        let store = if read_only {
            SqliteThreadStore::open_existing_read_only(&database)
                .await
                .unwrap()
        } else {
            SqliteThreadStore::new(Path::new(&database)).await.unwrap()
        };
        let binding = store
            .load_session_binding(&thread)
            .await
            .unwrap()
            .expect("已登记会话必须能读到绑定");
        assert_eq!(store.load_meta(&thread).await.unwrap().id, thread);
        assert_eq!(store.load_messages(&thread).await.unwrap().len(), 1);
        assert!(store.load_frozen_snapshot(&thread).await.unwrap().is_none());
        let project = store
            .list_scoped_threads(&ScopedThreadQuery {
                scope: ThreadScope::Project(binding.project_id),
                cursor: None,
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(project.entries.len(), 1);
        let exact = store
            .list_scoped_threads(&ScopedThreadQuery {
                scope: ThreadScope::ExactDirectory {
                    workspace_id: binding.workspace_id,
                    relative_cwd: binding.cwd_relative_to_workspace.clone(),
                },
                cursor: None,
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(exact.entries.len(), 1);
    }
}

/// 子进程模式：已有会话恢复执行前的绑定复核（`session/load` 的准入步骤）。
#[tokio::test]
async fn test_worktree_bound_session_validation_child() {
    let Ok(database) = std::env::var(VALIDATE_DATABASE) else {
        return;
    };
    let thread = std::env::var(VALIDATE_THREAD).unwrap();
    let cwd = std::env::var(VALIDATE_CWD).unwrap();
    let store = SqliteThreadStore::new(Path::new(&database)).await.unwrap();
    let workspace = store.validate_session_binding(&thread).await.unwrap();
    assert_eq!(workspace.cwd, tokio::fs::canonicalize(&cwd).await.unwrap());
}

/// [回归测试] 历史只读访问不依赖目录可用性，也不触发任何外部探测。
///
/// 列出会话、读取消息、读 frozen 与绑定是历史能力，不该因为目录身份发现而失败或被拖慢。
/// 假 Git 每次被调用都会在日志里记一行，用例要求只读访问前后行数不变——把发现引入列表
/// 路径会直接失败，而不是只能靠耗时观察。子进程运行前登记目录已被删除，因此读路径若引入
/// canonicalize / stat 之类的目录检查同样会失败。
#[cfg(unix)]
#[tokio::test]
async fn test_worktree_history_access_never_probes_git() {
    let directory = tempfile::tempdir().unwrap();
    let work = directory.path().join("plain-project");
    std::fs::create_dir(&work).unwrap();
    let (recorded, database) = probe_admission(directory.path(), &work, "git-not-repository", None);
    assert!(!recorded.is_empty(), "准入必须先真的调用过假 Git");
    let thread = bound_thread_id(&database).await;
    // 写入一条历史供只读访问读取；这一步在测量之前完成。
    let store = SqliteThreadStore::new(&database).await.unwrap();
    let lease = store.acquire_execution_lease(&thread).await.unwrap();
    store
        .append_message(&thread, BaseMessage::human("bound history"))
        .await
        .unwrap();
    lease.mark_clean().await.unwrap();
    store.close().await;
    // 登记目录消失后历史仍必须可读：执行身份不可复核不等于历史不可访问。
    std::fs::remove_dir_all(&work).unwrap();

    let log = directory.path().join("probe.log");
    let before = std::fs::read_to_string(&log).unwrap().lines().count();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", HISTORY_CHILD, "--nocapture"])
        .env("PATH", directory.path().join("bin"))
        .env(PROBE_LOG, &log)
        .env(PROBE_DATABASE, &database)
        .env(PROBE_BEHAVIOR, "git-not-repository")
        .env(HISTORY_DATABASE, &database)
        .env(HISTORY_THREAD, &thread)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "只读历史访问子进程失败：{}",
        String::from_utf8_lossy(&output.stderr),
    );
    // 子进程提前返回（例如环境变量名写错）时，用例会变成没有覆盖读路径的空断言。
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("running 1 test"),
        "子进程必须真的执行了只读历史访问用例：{}",
        String::from_utf8_lossy(&output.stdout),
    );
    let after = std::fs::read_to_string(&log).unwrap().lines().count();
    assert_eq!(
        before, after,
        "历史只读访问不得调用 Git：调用次数 {before} → {after}",
    );
}

#[cfg(unix)]
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

/// 在父进程 PATH 里解析一个真实工具并写成绝对路径。
///
/// 假 Git 运行在受控 PATH（只有 bin 目录）下，脚本里按名字调用外部工具会找不到：
/// 那些工具的路径必须在生成脚本时定下来。
#[cfg(unix)]
fn real_tool(name: &str) -> std::path::PathBuf {
    let output = std::process::Command::new("sh")
        .args(["-c", &format!("command -v {name}")])
        .output()
        .unwrap();
    assert!(output.status.success(), "测试环境需要真实 {name}");
    std::path::PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
}

#[cfg(unix)]
fn real_git_path() -> std::path::PathBuf {
    real_tool("git")
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

/// 读出准入子进程建立的绑定会话 ID，供只读访问子进程复用它读历史。
#[cfg(unix)]
async fn bound_thread_id(database: &Path) -> String {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(database))
        .await
        .unwrap();
    let row: (String,) = sqlx::query_as("SELECT thread_id FROM session_bindings")
        .fetch_one(&pool)
        .await
        .unwrap();
    pool.close().await;
    row.0
}

/// 写出受控 PATH 下的假 Git，返回它的 bin 目录。
///
/// `sleep_ms` 是每次调用前的固定等待，用来把「等待阶段」变成可测量的时间线；
/// `real_git` 为 `None` 时假 Git 直接扮演「明确回答不是仓库」的 Git。
#[cfg(unix)]
fn write_probe_git(directory: &Path, sleep_ms: u64, real_git: Option<&Path>) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let shim = directory.join("bin");
    std::fs::create_dir(&shim).unwrap();
    let binary = std::env::current_exe().unwrap();
    // 假 Git 的 stdout 是发现过程解析的对象，测试框架的横幅不能混进去；
    // 角色扮演所需的 stderr 与退出码仍由子进程自己给出。
    let probe = format!(
        "{} --exact {PROBE_GIT_CHILD} --nocapture",
        shell_quote(&binary)
    );
    let sleep = if sleep_ms == 0 {
        String::new()
    } else {
        format!(
            "{} {}.{:03}\n",
            shell_quote(&real_tool("sleep")),
            sleep_ms / 1000,
            sleep_ms % 1000
        )
    };
    let script = match real_git {
        Some(real) => format!(
            "#!/bin/sh\n{sleep}{PROBE_ARGS}=\"$*\"\nexport {PROBE_ARGS}\n{probe} >/dev/null\nexec {} \"$@\"\n",
            shell_quote(real)
        ),
        None => format!(
            "#!/bin/sh\n{sleep}{PROBE_ARGS}=\"$*\"\nexport {PROBE_ARGS}\nexec {probe} >/dev/null\n"
        ),
    };
    let git = shim.join("git");
    std::fs::write(&git, script).unwrap();
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
    shim
}

/// 受控 PATH 下一次准入子进程的命令；调用方决定同步等待还是边跑边观察。
#[cfg(unix)]
fn admission_command(
    shim: &Path,
    database: &Path,
    work: &Path,
    behavior: &str,
) -> std::process::Command {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", ADMISSION_CHILD, "--nocapture"])
        .env("PATH", shim)
        .env(ADMISSION_DATABASE, database)
        .env(ADMISSION_CWD, work)
        .env(PROBE_LOG, database.parent().unwrap().join("probe.log"))
        .env(PROBE_DATABASE, database)
        .env(PROBE_BEHAVIOR, behavior);
    command
}

/// 以受控 PATH 跑一次准入子进程；失败留给调用方按用例语义断言。
#[cfg(unix)]
fn spawn_admission_child(
    shim: &Path,
    database: &Path,
    work: &Path,
    behavior: &str,
) -> std::process::Output {
    admission_command(shim, database, work, behavior)
        .output()
        .unwrap()
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
    let database = directory.join("threads.db");
    let shim = write_probe_git(directory, 0, real_git);
    let output = spawn_admission_child(&shim, &database, work, behavior);
    assert!(
        output.status.success(),
        "准入子进程失败：{}",
        String::from_utf8_lossy(&output.stderr),
    );
    (read_probe_log(directory), database)
}

/// 读出假 Git 的调用记录；文件不存在时返回空序列。
#[cfg(unix)]
fn read_probe_log(directory: &Path) -> Vec<String> {
    let log = directory.join("probe.log");
    match std::fs::read_to_string(log) {
        Ok(text) => text.lines().map(str::to_owned).collect(),
        Err(_) => Vec::new(),
    }
}

/// [回归测试] 准入的外部探测必须全部发生在写事务之外。
///
/// 上一版实现把完整 Git 发现放在 `BEGIN IMMEDIATE` 内，并在一次准入里重复多轮：
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
        recorded.iter().all(|line| lock_state(line) == "free"),
        "写事务持有期间不得执行外部探测，实际记录：{recorded:?}",
    );
    assert_eq!(
        recorded.len(),
        1,
        "目录模式一次准入只应观测一轮（准入解析）：{recorded:?}",
    );
    assert_eq!(
        binding_count(&database).await,
        1,
        "用例必须真的完成了一次登记"
    );
}

/// [回归测试] 仓库模式一次准入的真实 Git 调用次数保持有界：一轮观测，三条命令。
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
        recorded.iter().all(|line| lock_state(line) == "free"),
        "写事务持有期间不得执行外部探测，实际记录：{recorded:?}",
    );
    assert_eq!(
        recorded.len(),
        3,
        "一次准入的 Git 调用次数应有界（一轮观测三条命令）：{recorded:?}",
    );
    assert_eq!(
        binding_count(&database).await,
        1,
        "用例必须真的完成了一次登记"
    );
}

/// [回归测试] 已有会话恢复执行前的绑定复核只有一轮观测，且发生在写事务之外。
///
/// 恢复执行必须复核 cwd 与登记一致（设计 §3.2），但复核不该比一次发现更贵：重复发现会
/// 把 Git 的等待时间叠加到每次 `session/load`。子进程复用准入时建好的假 Git，日志尾部
/// 即本轮复核的调用序列。
#[cfg(unix)]
#[tokio::test]
async fn test_worktree_bound_session_validation_probes_once() {
    let repository = repository();
    let directory = tempfile::tempdir().unwrap();
    let (recorded, database) = probe_admission(
        directory.path(),
        repository.path(),
        "log-only",
        Some(&real_git_path()),
    );
    assert_eq!(recorded.len(), 3, "准入本身是一轮观测：{recorded:?}");
    let thread = bound_thread_id(&database).await;

    let log = directory.path().join("probe.log");
    let before = std::fs::read_to_string(&log).unwrap().lines().count();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", VALIDATE_CHILD, "--nocapture"])
        .env("PATH", directory.path().join("bin"))
        .env(PROBE_LOG, &log)
        .env(PROBE_DATABASE, &database)
        .env(PROBE_BEHAVIOR, "log-only")
        .env(VALIDATE_DATABASE, &database)
        .env(VALIDATE_THREAD, &thread)
        .env(VALIDATE_CWD, repository.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "绑定复核子进程失败：{}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("running 1 test"),
        "子进程必须真的执行了绑定复核用例：{}",
        String::from_utf8_lossy(&output.stdout),
    );
    let calls: Vec<String> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    let validation = &calls[before..];
    assert_eq!(
        validation.len(),
        3,
        "已有会话复核只应观测一轮（三条命令）：{validation:?}",
    );
    assert!(
        validation.iter().all(|line| lock_state(line) == "free"),
        "复核不得在写事务内执行：{validation:?}",
    );
}

/// [回归测试] 慢 Git 的等待是被实测的：每次等待可归因到具体命令，且不在写事务内。
///
/// 假 Git 每次调用前固定等待 400ms，把「Git 慢」变成可测量的时间线：调用参数说明等待
/// 发生在哪个阶段（一轮 × 三条命令），到达时刻给出这些等待的真实分布。断言用实测值；
/// 代码允许的单次上限（5s × 3 次 = 15s）是静态最坏预算，不作为这里的耗时证据。
///
/// 慢响应下准入必须仍然成立（等待远小于单次超时，不能因为慢就失败），代价必须是可测量的
/// 等待，而不是被写事务挡住其他 writer。
#[cfg(unix)]
#[tokio::test]
async fn test_worktree_slow_git_wait_is_measured_per_call_outside_the_write_lock() {
    const SLEEP_MS: u64 = 400;
    const POLL_MS: u64 = 20;
    /// 一轮观测三条命令，准入一轮。
    const CALLS: u64 = 3;
    let repository = repository();
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("threads.db");
    let shim = write_probe_git(directory.path(), SLEEP_MS, Some(&real_git_path()));

    let started = Instant::now();
    let mut child = admission_command(&shim, &database, repository.path(), "log-only")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // 子进程运行期间轮询日志，按到达时刻记录每次调用；轮询间隔远小于固定等待，
    // 因此相邻到达的间隔就是上一个阶段真实的 Git 等待。
    let mut timeline: Vec<(Duration, String)> = Vec::new();
    while timeline.len() < CALLS as usize && started.elapsed() < Duration::from_secs(60) {
        for line in read_probe_log(directory.path())
            .into_iter()
            .skip(timeline.len())
        {
            timeline.push((started.elapsed(), line));
        }
        if child.try_wait().unwrap().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(POLL_MS)).await;
    }
    let status = child.wait().unwrap();
    let measured = started.elapsed();
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();

    assert!(status.success(), "慢 Git 不得让准入失败：{stdout}");
    assert!(
        stdout.contains("running 1 test"),
        "子进程必须真的执行了准入用例：{stdout}",
    );
    assert_eq!(
        timeline.len(),
        CALLS as usize,
        "慢 Git 下的调用次数必须与快 Git 相同：{timeline:?}",
    );
    let expected = [
        "rev-parse --is-inside-work-tree",
        "rev-parse --show-toplevel --git-dir",
        "worktree list --porcelain -z",
    ];
    // 假 Git 收到的是完整命令行（`-C <cwd>` 在内），去掉前缀后比对命令本身。
    let prefix = format!(
        "-C {} ",
        std::fs::canonicalize(repository.path()).unwrap().display()
    );
    let phases: Vec<String> = timeline
        .iter()
        .map(|(_, line)| {
            call_args(line)
                .trim_start_matches(prefix.as_str())
                .to_owned()
        })
        .collect();
    assert_eq!(phases, expected, "等待阶段必须对应到具体命令：{timeline:?}",);
    assert!(
        timeline.iter().all(|(_, line)| lock_state(line) == "free"),
        "慢 Git 的等待不得落在写事务内：{timeline:?}",
    );

    let span = timeline.last().unwrap().0 - timeline[0].0;
    println!(
        "慢 Git 实测：准入总耗时 {measured:?}，首次到末次调用 {span:?}，固定等待 {SLEEP_MS}ms × {CALLS}"
    );
    for (offset, line) in &timeline {
        println!("  {offset:>12.3?}  {line}");
    }
    // 每次调用的额外开销必须远小于单次超时预算：相邻两次调用之间只能有上一次调用的
    // 固定等待加上很小的开销。子进程启动与数据库打开发生在首次调用之前，不属于任何
    // 一次调用，因此不参与这条判定（否则用例会把启动耗时当成探测开销）。
    const OVERHEAD_MS: u64 = 2_000;
    for window in timeline.windows(2) {
        let gap = window[1].0 - window[0].0;
        assert!(
            gap < Duration::from_millis(SLEEP_MS + OVERHEAD_MS),
            "相邻调用间隔 {gap:?} 超出固定等待 {SLEEP_MS}ms 加开销上限：{timeline:?}",
        );
    }
    // 等待分布的下限：首次到末次之间有 CALLS - 1 段固定等待。
    let floor = Duration::from_millis(SLEEP_MS * (CALLS - 1));
    assert!(
        span >= floor,
        "首次到末次调用只有 {span:?}（下限 {floor:?}），等待没有分布在整条时间线上：{timeline:?}",
    );
}
