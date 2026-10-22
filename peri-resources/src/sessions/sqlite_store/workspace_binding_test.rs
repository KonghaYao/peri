use super::*;

/// [回归测试] 写绑定前的纯路径结构校验：root/relative/cwd 自相矛盾时拒绝。
#[tokio::test]
async fn test_worktree_create_bound_thread_rejects_inconsistent_path_structure() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let (store, _db) = store().await;
    let workspace = store.resolve_workspace(&root).await.unwrap();

    let inconsistent = [
        // 归属行路径与 root 不一致。
        ResolvedWorkspace {
            root: directory.path().join("other"),
            ..workspace.clone()
        },
        // root + relative 推不出执行 cwd。
        ResolvedWorkspace {
            cwd: root.join("elsewhere"),
            ..workspace.clone()
        },
    ];
    for candidate in inconsistent {
        let meta = ThreadMeta::new_at(candidate.cwd.to_str().unwrap(), peri_time::now_wall());
        let id = meta.id.clone();
        let error = store
            .create_bound_thread(meta, &candidate)
            .await
            .unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<WorkspaceError>(),
                Some(WorkspaceError::ExecutionBindingMismatch)
            ),
            "自相矛盾的路径结构必须拒绝：{error}"
        );
        let (threads,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM threads WHERE id = ?1")
            .bind(id.as_str())
            .fetch_one(&store.database.pool)
            .await
            .unwrap();
        let (bindings,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM session_bindings WHERE thread_id = ?1")
                .bind(id.as_str())
                .fetch_one(&store.database.pool)
                .await
                .unwrap();
        assert_eq!((threads, bindings), (0, 0), "失败不得留下半条会话行");
    }
}

/// [回归测试] `Full` 与 `Recorded` 复核等价：诊断快照变化不改变结论。
#[tokio::test]
async fn test_worktree_full_and_recorded_rechecks_are_equivalent() {
    let repo = repository();
    let (store, _db) = store().await;
    let (id, registered) = bound(&store, repo.path()).await;

    // 只改发现快照里的对象身份（模拟容器重启后的 inode 变化）。
    let (snapshot,): (String,) = sqlx::query_as("SELECT discovery FROM workspaces WHERE id = ?1")
        .bind(registered.workspace_id.to_string())
        .fetch_one(&store.database.pool)
        .await
        .unwrap();
    let mut snapshot: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
    let inode = snapshot["root_identity"]["inode"].as_u64().unwrap();
    snapshot["root_identity"]["inode"] = serde_json::json!(inode + 1);
    sqlx::query("UPDATE workspaces SET discovery = ?1 WHERE id = ?2")
        .bind(snapshot.to_string())
        .bind(registered.workspace_id.to_string())
        .execute(&store.database.pool)
        .await
        .unwrap();

    let full = store.validate_session_binding(&id).await.unwrap();
    let recorded = store.reassert_session_binding(&id).await.unwrap();
    assert_eq!(full, recorded);
    assert_eq!(full.workspace_id, registered.workspace_id);
    assert_eq!(full.project_id, registered.project_id);
}

/// [回归测试] 准入不比较归属行当前项目与绑定项目：绑定项目仍被保留。
#[tokio::test]
async fn test_worktree_binding_project_survives_workspace_project_change() {
    let repo = repository();
    let (store, _db) = store().await;
    let (id, registered) = bound(&store, repo.path()).await;

    // 同库登记另一个合法项目，再把归属行项目改到它：旧绑定仍指向原项目。
    let other_project = ProjectId::new();
    sqlx::query("INSERT INTO projects (id, locator, object_identity) VALUES (?1, ?2, ?3)")
        .bind(other_project.to_string())
        .bind("/another-project")
        .bind(r#"{"device":1,"inode":1}"#)
        .execute(&store.database.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE workspaces SET project_id = ?1 WHERE id = ?2")
        .bind(other_project.to_string())
        .bind(registered.workspace_id.to_string())
        .execute(&store.database.pool)
        .await
        .unwrap();

    let resolved = store.validate_session_binding(&id).await.unwrap();
    assert_eq!(resolved.workspace_id, registered.workspace_id);
    assert_eq!(
        resolved.project_id, registered.project_id,
        "返回的归属必须沿用绑定项目，不被行项目改绑"
    );
    assert_ne!(resolved.project_id, other_project);
    assert_eq!(
        store
            .load_session_binding(&id)
            .await
            .unwrap()
            .unwrap()
            .project_id,
        registered.project_id
    );
}

#[tokio::test]
async fn test_worktree_writes_do_not_claim_ownership_and_metadata_cannot_rebind() {
    let repo = repository();
    let (store, db) = store().await;
    let (id, resolved) = bound(&store, repo.path()).await;
    store
        .append_message(&id, BaseMessage::human("without live run"))
        .await
        .unwrap();
    let other = SqliteThreadStore::new(db.path().join("threads.db"))
        .await
        .unwrap();
    other
        .append_message(&id, BaseMessage::human("other host"))
        .await
        .unwrap();
    assert_eq!(store.load_messages(&id).await.unwrap().len(), 2);
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
    let mut child = ThreadMeta::new_at(resolved.cwd.to_str().unwrap(), peri_time::now_wall());
    child.parent_thread_id = Some(id.clone());
    child.hidden = true;
    let child = store.create_bound_thread(child, &resolved).await.unwrap();
    store
        .append_message(&child, BaseMessage::human("owned child"))
        .await
        .unwrap();
    store
        .append_message(&child, BaseMessage::human("continued"))
        .await
        .unwrap();
    assert_eq!(store.load_messages(&child).await.unwrap().len(), 2);
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
    .fetch_one(&store.database.pool)
    .await
    .unwrap();
    assert_eq!(revision_columns, 0);

    let (id, workspace) = bound(&store, &cwd).await;
    store
        .append_message(&id, BaseMessage::human("bound history"))
        .await
        .unwrap();
    let expected = SessionBinding::from_workspace(&workspace);
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
    // 后缀的分隔符随平台：Windows 的解析文本用 `\`。
    for (cwd, suffix) in [
        (repo.path().to_path_buf(), String::new()),
        (nested.clone(), format!("{}sub", std::path::MAIN_SEPARATOR)),
    ] {
        let (id, resolved) = bound(&store, &cwd).await;
        let registered = resolved.cwd.to_str().unwrap();
        assert!(
            registered.ends_with(suffix.as_str()),
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
        .execute(&reopened.database.pool)
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
            ThreadMeta::new_at(registered.cwd.to_str().unwrap(), peri_time::now_wall()),
            &registered,
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::ReadOnlyStore)
    ));
}
