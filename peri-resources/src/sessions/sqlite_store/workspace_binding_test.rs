use super::*;

#[tokio::test]
async fn test_worktree_writes_do_not_claim_ownership_and_metadata_cannot_rebind() {
    let repo = repository();
    let (store, db) = store().await;
    let (id, resolved) = bound(&store, repo.path()).await;
    store
        .append_message(&id, BaseMessage::human("without live run"))
        .await
        .unwrap();
    let lease = store.acquire_execution_lease(&id).await.unwrap();
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
    .fetch_one(&store.database.pool)
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
