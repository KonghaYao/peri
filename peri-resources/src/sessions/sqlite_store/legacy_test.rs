use super::*;
use peri_acp_types::workspace::{ScopedThreadQuery, ThreadScope};
use sqlx::{Connection, SqliteConnection};

async fn legacy_database(path: &std::path::Path, cwd: &std::path::Path) -> String {
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    sqlx::raw_sql(include_str!("fixtures/legacy_with_goals.sql"))
        .execute(&mut connection)
        .await
        .unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count) VALUES (?, 'old history', ?, '2026-09-01T00:00:00Z', '2026-09-02T00:00:00Z', 1)")
        .bind(&id).bind(cwd.to_str().unwrap()).execute(&mut connection).await.unwrap();
    let message = BaseMessage::human("history survives upgrade");
    sqlx::query(
        "INSERT INTO messages (message_id, thread_id, role, content) VALUES (?, ?, 'user', ?)",
    )
    .bind(message.id().as_uuid().to_string())
    .bind(&id)
    .bind(serialize_persisted_payload(&PersistedPayload::Message(message)).unwrap())
    .execute(&mut connection)
    .await
    .unwrap();
    connection.close().await.unwrap();
    id
}

/// Upgrading, and reopening a database already upgraded by 3.15.0, retain usable history lists.
#[tokio::test]
async fn legacy_history_visible_after_upgrade_and_schema3_reopen() {
    let dir = tempfile::tempdir().unwrap();
    // Old releases saved the caller's ordinary path, not canonical/verbatim Windows paths.
    let cwd = dir.path().to_owned();
    let path = dir.path().join("threads.db");
    let id = legacy_database(&path, &cwd).await;
    for _ in 0..2 {
        let store = SqliteThreadStore::new(&path).await.unwrap();
        let workspace = store.resolve_workspace(&cwd).await.unwrap();
        for scope in [
            ThreadScope::Project(workspace.project_id),
            ThreadScope::Workspace(workspace.workspace_id),
            ThreadScope::ExactDirectory {
                workspace_id: workspace.workspace_id,
                relative_cwd: workspace.relative_cwd.clone(),
            },
            ThreadScope::All,
        ] {
            let page = store
                .list_scoped_threads(&ScopedThreadQuery {
                    scope: scope.clone(),
                    cursor: None,
                    limit: 10,
                })
                .await
                .unwrap();
            assert_eq!(page.entries.len(), 1, "old history missing from {scope:?}");
            assert_eq!(page.entries[0].thread.id, id);
        }
        assert!(
            store.load_session_binding(&id).await.unwrap().is_none(),
            "listing must not grant execution identity"
        );
        assert_eq!(
            store.load_messages(&id).await.unwrap()[0].content(),
            "history survives upgrade"
        );
        store.close().await;
    }
}

#[tokio::test]
async fn legacy_history_in_missing_directory_remains_in_all_scope() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("removed-checkout");
    let path = dir.path().join("threads.db");
    let id = legacy_database(&path, &missing).await;
    let store = SqliteThreadStore::new(&path).await.unwrap();
    let page = store
        .list_scoped_threads(&ScopedThreadQuery {
            scope: ThreadScope::All,
            cursor: None,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].thread.id, id);
    assert_eq!(page.entries[0].effective_cwd, missing);
    assert!(store.load_session_binding(&id).await.unwrap().is_none());
    store.close().await;
}

#[tokio::test]
async fn legacy_adoption_commits_snapshot_once_across_concurrent_restorers() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(dir.path()).unwrap();
    let path = dir.path().join("threads.db");
    let id = legacy_database(&path, &cwd).await;
    let left = SqliteThreadStore::new(&path).await.unwrap();
    let right = SqliteThreadStore::new(&path).await.unwrap();
    let workspace = left.resolve_workspace(&cwd).await.unwrap();
    let (a, b) = tokio::join!(
        left.adopt_legacy_thread(&id, cwd.to_str().unwrap(), &workspace, "first snapshot"),
        right.adopt_legacy_thread(&id, cwd.to_str().unwrap(), &workspace, "second snapshot"),
    );
    a.unwrap();
    b.unwrap();
    let winner = left.load_frozen_snapshot(&id).await.unwrap().unwrap();
    assert!(matches!(
        winner.as_str(),
        "first snapshot" | "second snapshot"
    ));
    assert_eq!(left.validate_session_binding(&id).await.unwrap(), workspace);
    left.adopt_legacy_thread(&id, cwd.to_str().unwrap(), &workspace, "must not overwrite")
        .await
        .unwrap();
    assert_eq!(
        right.load_frozen_snapshot(&id).await.unwrap().unwrap(),
        winner
    );
    let lease = left.acquire_execution_lease(&id).await.unwrap();
    let error = right.acquire_execution_lease(&id).await.err().unwrap();
    assert!(matches!(
        error.downcast_ref::<peri_acp_types::workspace::WorkspaceError>(),
        Some(peri_acp_types::workspace::WorkspaceError::ExecutionBusy)
    ));
    left.append_message(&id, BaseMessage::human("continued"))
        .await
        .unwrap();
    lease.mark_clean().await.unwrap();
    assert_eq!(right.load_messages(&id).await.unwrap().len(), 2);
    left.close().await;
    right.close().await;
    let reopened = SqliteThreadStore::new(&path).await.unwrap();
    assert_eq!(
        reopened.load_frozen_snapshot(&id).await.unwrap().unwrap(),
        winner
    );
    reopened
        .acquire_execution_lease(&id)
        .await
        .unwrap()
        .mark_clean()
        .await
        .unwrap();
    reopened.close().await;
}

#[tokio::test]
async fn legacy_adoption_failure_rolls_back_binding_and_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(dir.path()).unwrap();
    let path = dir.path().join("threads.db");
    let id = legacy_database(&path, &cwd).await;
    let store = SqliteThreadStore::new(&path).await.unwrap();
    let workspace = store.resolve_workspace(&cwd).await.unwrap();
    sqlx::raw_sql("CREATE TRIGGER reject_binding BEFORE INSERT ON session_bindings BEGIN SELECT RAISE(FAIL, 'injected binding failure'); END;")
        .execute(&store.pool).await.unwrap();
    let error = store
        .adopt_legacy_thread(&id, cwd.to_str().unwrap(), &workspace, "snapshot")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("injected binding failure"));
    assert!(store.load_session_binding(&id).await.unwrap().is_none());
    assert!(store.load_frozen_snapshot(&id).await.unwrap().is_none());
    sqlx::query("DROP TRIGGER reject_binding")
        .execute(&store.pool)
        .await
        .unwrap();
    store
        .adopt_legacy_thread(&id, cwd.to_str().unwrap(), &workspace, "snapshot")
        .await
        .unwrap();
    assert_eq!(
        store.load_frozen_snapshot(&id).await.unwrap().as_deref(),
        Some("snapshot")
    );
    store.close().await;
}

#[tokio::test]
async fn legacy_adoption_rejects_changed_cwd_child_and_lost_native_binding() {
    use peri_acp_types::workspace::WorkspaceError;
    let dir = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(dir.path()).unwrap();
    let store = SqliteThreadStore::new(dir.path().join("threads.db"))
        .await
        .unwrap();
    let workspace = store.resolve_workspace(&cwd).await.unwrap();
    let id = store
        .create_thread(ThreadMeta::new(cwd.to_str().unwrap()))
        .await
        .unwrap();
    let mut meta = store.load_meta(&id).await.unwrap();
    meta.cwd = cwd.join("changed").to_str().unwrap().to_owned();
    store.update_meta(&id, meta.clone()).await.unwrap();
    let error = store
        .adopt_legacy_thread(&id, cwd.to_str().unwrap(), &workspace, "snapshot")
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::ExecutionBindingMismatch)
    ));
    meta.cwd = cwd.to_str().unwrap().to_owned();
    meta.parent_thread_id = Some("parent".into());
    store.update_meta(&id, meta.clone()).await.unwrap();
    let error = store
        .adopt_legacy_thread(&id, cwd.to_str().unwrap(), &workspace, "snapshot")
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::ExecutionBindingMismatch)
    ));
    meta.parent_thread_id = None;
    store.update_meta(&id, meta).await.unwrap();
    sqlx::query("INSERT INTO execution_runs VALUES (?, 1, 0)")
        .bind(&id)
        .execute(&store.pool)
        .await
        .unwrap();
    let error = store
        .adopt_legacy_thread(&id, cwd.to_str().unwrap(), &workspace, "snapshot")
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::InvalidBinding)
    ));
    assert!(store.load_session_binding(&id).await.unwrap().is_none());
    assert!(store.load_frozen_snapshot(&id).await.unwrap().is_none());
    store.close().await;
}

#[tokio::test]
async fn legacy_children_follow_adopted_root_execution_owner() {
    use peri_acp_types::workspace::WorkspaceError;
    let dir = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(dir.path()).unwrap();
    let store = SqliteThreadStore::new(dir.path().join("threads.db"))
        .await
        .unwrap();
    let workspace = store.resolve_workspace(&cwd).await.unwrap();
    let root = store
        .create_thread(ThreadMeta::new(cwd.to_str().unwrap()))
        .await
        .unwrap();
    let mut child = ThreadMeta::new(cwd.to_str().unwrap());
    child.parent_thread_id = Some(root.clone());
    child.hidden = true;
    let child = store.create_thread(child).await.unwrap();
    store
        .adopt_legacy_thread(&root, cwd.to_str().unwrap(), &workspace, "snapshot")
        .await
        .unwrap();
    let error = store
        .append_message(&child, BaseMessage::human("without owner"))
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::ExecutionLeaseRequired)
    ));
    let lease = store.acquire_execution_lease(&root).await.unwrap();
    store
        .append_message(&child, BaseMessage::human("with root owner"))
        .await
        .unwrap();
    assert_eq!(store.load_messages(&child).await.unwrap().len(), 1);
    assert!(store.load_session_binding(&child).await.unwrap().is_none());
    lease.mark_clean().await.unwrap();
    let error = store
        .append_message(&child, BaseMessage::human("after close"))
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::ExecutionLeaseRequired)
    ));
    store.close().await;
}

#[tokio::test]
async fn legacy_history_scopes_keep_path_boundaries_and_mixed_pagination() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(dir.path()).unwrap().join("project_%");
    std::fs::create_dir(&cwd).unwrap();
    let store = SqliteThreadStore::new(dir.path().join("threads.db"))
        .await
        .unwrap();
    let ws = store.resolve_workspace(&cwd).await.unwrap();
    let native = store
        .create_bound_thread(ThreadMeta::new(cwd.to_str().unwrap()), &ws)
        .await
        .unwrap();
    let lease = store.acquire_execution_lease(&native).await.unwrap();
    store
        .append_message(&native, BaseMessage::human("new"))
        .await
        .unwrap();
    lease.mark_clean().await.unwrap();
    let mut expected = vec![native];
    for path in [
        cwd.clone(),
        cwd.join("deleted-subdir"),
        cwd.with_file_name("project_%other"),
        cwd.with_file_name("project_AB"),
    ] {
        let id = store
            .create_thread(ThreadMeta::new(path.to_str().unwrap()))
            .await
            .unwrap();
        store
            .append_message(&id, BaseMessage::human("old"))
            .await
            .unwrap();
        if path.starts_with(&cwd) {
            expected.push(id);
        }
    }
    // Register an overlapping root: an EXISTS filter must not duplicate any row.
    store.resolve_workspace(dir.path()).await.unwrap();
    let mut cursor = None;
    let mut found = Vec::new();
    loop {
        let page = store
            .list_scoped_threads(&ScopedThreadQuery {
                scope: ThreadScope::Workspace(ws.workspace_id),
                cursor,
                limit: 1,
            })
            .await
            .unwrap();
        found.extend(page.entries.into_iter().map(|entry| entry.thread.id));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    found.sort();
    expected.sort();
    assert_eq!(found, expected);
    let exact = store
        .list_scoped_threads(&ScopedThreadQuery {
            scope: ThreadScope::ExactDirectory {
                workspace_id: ws.workspace_id,
                relative_cwd: "deleted-subdir".into(),
            },
            cursor: None,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(exact.entries.len(), 1);
    assert!(exact.entries[0].binding.is_none());
    store.close().await;
}
