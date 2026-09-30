use super::*;

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
    .execute(&store.database.pool)
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
        .execute(&store.database.pool)
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
