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
    let binding = store.load_session_binding(&id).await.unwrap().unwrap();
    // cwd 被 symlink 指向别处：canonical 与保存路径不一致，无法证明仍是同一位置。
    std::fs::remove_dir(repo.path().join("sub")).unwrap();
    std::os::unix::fs::symlink(aliases.path(), repo.path().join("sub")).unwrap();
    let error = store.validate_session_binding(&id).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::NeedsRelink)
    ));
    // 复核失败不改写绑定或历史。
    assert_eq!(
        store.load_session_binding(&id).await.unwrap(),
        Some(binding)
    );
}

#[tokio::test]
async fn test_worktree_cwd_replaced_by_file_is_unavailable() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let (store, _db) = store().await;
    let (id, _) = bound(&store, &root).await;
    // 同路径换成普通文件：不是目录，按不可用失败。
    std::fs::remove_dir(&root).unwrap();
    std::fs::write(&root, b"not a directory").unwrap();
    let error = store.validate_session_binding(&id).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::Unavailable)
    ));
}

/// [回归测试] 祖先组件被 symlink 换位：canonical 与保存路径不一致，拒绝执行。
///
/// 临时目录 canonicalize 后可能位于 `/private/var`，因此全部用 canonical 路径构造。
#[cfg(unix)]
#[tokio::test]
async fn test_worktree_ancestor_symlink_swap_is_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let root = tokio::fs::canonicalize(directory.path()).await.unwrap();
    let parent = root.join("parent");
    let sub = parent.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside = tokio::fs::canonicalize(outside.path()).await.unwrap();
    std::fs::create_dir_all(outside.join("sub")).unwrap();

    let (store, _db) = store().await;
    let (id, _) = bound(&store, &sub).await;
    let binding = store.load_session_binding(&id).await.unwrap().unwrap();

    // 祖先 parent 改名，原位换成指向外部的 symlink；外部同样存在 sub。
    std::fs::rename(&parent, root.join("moved-parent")).unwrap();
    std::os::unix::fs::symlink(&outside, &parent).unwrap();

    let error = store.validate_session_binding(&id).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::NeedsRelink)
    ));
    // 复核失败不改写绑定。
    assert_eq!(
        store.load_session_binding(&id).await.unwrap(),
        Some(binding)
    );
}

/// 建立绑定会话并写入一条消息，使历史进入列表查询的可见范围。
async fn bound_with_history(
    store: &SqliteThreadStore,
    cwd: &Path,
) -> (ThreadId, ResolvedWorkspace) {
    let (id, workspace) = bound(store, cwd).await;
    store
        .append_message(&id, BaseMessage::human("bound history"))
        .await
        .unwrap();
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

/// [回归测试] 同路径换成新文件对象后，归属、项目与已有绑定都不受影响。
///
/// 归属键是 `(machine_id, 路径)`；项目按 canonical locator 归属。文件对象身份
/// （device/inode）只写入发现快照作诊断，不参与执行准入：旧会话只要 cwd 仍在就能
/// 继续执行，新会话复用同一工作区与同一项目。
///
/// 文件对象证据只有 device/inode，删除后重建时文件系统可能复用刚释放的 inode，
/// 那种情况下新旧对象在证据上等同（设计 §8：不依赖 creation time）。所以这里让
/// 旧对象改名到另一路径继续存活，新对象才确定是一个不同的对象，断言不随分配策略
/// 摆动。
#[tokio::test]
async fn test_worktree_replaced_directory_keeps_workspace_and_binding() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let (store, _db) = store().await;
    let (old_thread, registered) = bound_with_history(&store, &root).await;

    // 同一路径上换成一个新的文件对象：对象身份变化不再阻断恢复。
    std::fs::rename(&root, directory.path().join("replaced")).unwrap();
    std::fs::create_dir(&root).unwrap();

    // 旧会话继续可执行：归属行仍在、cwd 仍可用；绑定没有被改写。
    let resolved = store.validate_session_binding(&old_thread).await.unwrap();
    assert_eq!(resolved.workspace_id, registered.workspace_id);
    assert_eq!(resolved.project_id, registered.project_id);
    assert_eq!(resolved.cwd, registered.cwd);
    assert_only_history(&store, registered.project_id, &old_thread).await;
    let binding = store
        .load_session_binding(&old_thread)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(binding.workspace_id, registered.workspace_id);
    assert_eq!(binding.project_id, registered.project_id);

    // 同一路径上的新对象复用同一条归属行与同一个项目（locator-only）。
    let (replacement_thread, replacement) = bound(&store, &root).await;
    assert_eq!(replacement.workspace_id, registered.workspace_id);
    assert_eq!(replacement.project_id, registered.project_id);
    assert_ne!(
        replacement.discovery_snapshot, registered.discovery_snapshot,
        "同一路径的新对象仍把该行的诊断快照更新为本次观测"
    );
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

/// [回归测试] 同一路径只登记一次，冲突仍由唯一约束挡住。
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
        "INSERT INTO workspaces (id, machine_id, path, path_source, project_id, identity, discovery)
         SELECT 'duplicate', machine_id, path, path_source, project_id, identity, discovery
         FROM workspaces WHERE id = ?",
    )
    .bind(registered.workspace_id.to_string())
    .execute(&store.database.pool)
    .await;
    assert!(duplicate.is_err(), "同一 (machine_id, path) 不得重复登记");
}

/// [回归测试] 子目录随后初始化 Git，不再让原绑定失败关闭。
///
/// Git 布局变化只刷新观测快照；已有归属行沿用原项目，不因 locator 变化改项目。
/// 项目与快照都不参与执行准入：原路径的 cwd 仍在，已有会话继续执行，也不被静默改绑。
#[tokio::test]
async fn test_worktree_new_nested_repository_keeps_original_binding() {
    let repo = repository();
    let nested = repo.path().join("sub");
    std::fs::create_dir(&nested).unwrap();
    let (store, _db) = store().await;
    let (id, registered) = bound(&store, &nested).await;
    git(&nested, &["init", "-q"]);
    let resolved = store.validate_session_binding(&id).await.unwrap();
    assert_eq!(resolved.workspace_id, registered.workspace_id);
    assert_eq!(resolved.project_id, registered.project_id);
    assert_eq!(resolved.cwd, registered.cwd);
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
    let rediscovered = store.resolve_workspace(&root).await.unwrap();
    assert_eq!(rediscovered.workspace_id, registered.workspace_id);
    assert_ne!(
        rediscovered.discovery_snapshot,
        registered.discovery_snapshot
    );
    // Subdirectories now resolve into that same repository workspace.
    let nested = store.resolve_workspace(&nested).await.unwrap();
    assert_eq!(nested.workspace_id, registered.workspace_id);
    assert_eq!(nested.project_id, registered.project_id);
    assert_eq!(nested.relative_cwd, Path::new("sub"));
    // The existing session is neither rebound nor hidden, and new sessions work.
    let existing = store.validate_session_binding(&root_id).await.unwrap();
    assert_eq!(existing.workspace_id, registered.workspace_id);
    assert_eq!(existing.project_id, registered.project_id);
    let (fresh, fresh_workspace) = bound(&store, &root).await;
    assert_ne!(fresh, root_id);
    assert_eq!(fresh_workspace.workspace_id, registered.workspace_id);
    assert_eq!(
        fresh_workspace.discovery_snapshot,
        rediscovered.discovery_snapshot
    );
    // The session registered inside the subdirectory keeps executing: its own workspace
    // row still exists and the cwd is available. It is not silently rebound to the
    // repository root workspace.
    let nested_existing = store.validate_session_binding(&nested_id).await.unwrap();
    assert_eq!(nested_existing.workspace_id, nested_workspace.workspace_id);
    assert_eq!(nested_existing.project_id, nested_workspace.project_id);
    assert_eq!(nested_existing.cwd, nested_workspace.cwd);
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

    let rediscovered = store.resolve_workspace(repo.path()).await.unwrap();
    assert_eq!(rediscovered.workspace_id, registered.workspace_id);
    assert_ne!(
        rediscovered.discovery_snapshot,
        registered.discovery_snapshot
    );
    // 移除 `.git` 只刷新观测；已有绑定继续按 cwd 可用性执行，不被改绑。
    let existing = store.validate_session_binding(&id).await.unwrap();
    assert_eq!(existing.workspace_id, registered.workspace_id);
    assert_eq!(existing.project_id, registered.project_id);
    let _ = bound(&store, repo.path()).await;
    // Without a repository each directory is again its own workspace; the
    // subdirectory no longer belongs to the registered root workspace.
    let separate = store.resolve_workspace(&nested).await.unwrap();
    assert_ne!(separate.workspace_id, registered.workspace_id);
    assert_eq!(separate.relative_cwd, Path::new(""));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_worktree_concurrent_registration_reuses_winner() {
    let repo = repository();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("concurrent.db");
    let (left, right) = tokio::join!(SqliteThreadStore::new(&path), SqliteThreadStore::new(&path));
    let left = left.unwrap();
    let right = right.unwrap();
    let (left_workspace, right_workspace) = tokio::join!(
        left.resolve_workspace(repo.path()),
        right.resolve_workspace(repo.path())
    );
    assert_eq!(left_workspace.unwrap(), right_workspace.unwrap());
    let (projects, workspaces): (i64, i64) =
        sqlx::query_as("SELECT (SELECT COUNT(*) FROM projects), (SELECT COUNT(*) FROM workspaces)")
            .fetch_one(&left.database.pool)
            .await
            .unwrap();
    assert_eq!((projects, workspaces), (1, 1));
    tokio::join!(left.close(), right.close());
}

#[tokio::test]
async fn test_worktree_scoped_pages_and_exact_directory_are_lightweight() {
    let repo = repository();
    let sub = repo.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let (store, _db) = store().await;
    let (root_id, root) = bound(&store, repo.path()).await;
    let (sub_id, sub) = bound(&store, &sub).await;
    store
        .append_message(&root_id, BaseMessage::human("root history"))
        .await
        .unwrap();
    store
        .append_message(&sub_id, BaseMessage::human("sub history"))
        .await
        .unwrap();
    // Deliberately corrupt large owner blobs; listing never decodes or aggregates them.
    sqlx::query("UPDATE threads SET frozen_context = 'broken'")
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
}
