use super::*;
use peri_acp_types::session_resources::{BindingRecheck, SessionResources};
use peri_acp_types::workspace::{ScopedThreadQuery, ThreadScope};

#[tokio::test]
async fn migration_keeps_schema_version_and_existing_history() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let store = SqliteThreadStore::new(&path).await.unwrap();
    let id = store
        .create_thread(ThreadMeta::new("/missing/old-machine"))
        .await
        .unwrap();
    store
        .append_messages(&id, &[BaseMessage::human("preserved")])
        .await
        .unwrap();
    let before: (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&store.database.pool)
        .await
        .unwrap();
    assert_eq!(before.0, super::schema::CURRENT_SCHEMA_VERSION);
    let messages = store.load_messages(&id).await.unwrap();
    sqlx::query("DROP TABLE session_environments")
        .execute(&store.database.pool)
        .await
        .unwrap();
    store.close().await;
    let reopened = SqliteThreadStore::new(&path).await.unwrap();
    let after: (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&reopened.database.pool)
        .await
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(after.0, CURRENT_SCHEMA_VERSION);
    assert_eq!(
        reopened.load_meta(&id).await.unwrap().cwd,
        "/missing/old-machine"
    );
    assert_eq!(
        serde_json::to_value(reopened.load_messages(&id).await.unwrap()).unwrap(),
        serde_json::to_value(messages).unwrap()
    );
    let identity: (String,) =
        sqlx::query_as("SELECT w.machine_id FROM threads t JOIN workspaces w ON w.id=t.workspace_id WHERE t.id = ?1")
            .bind(&id)
            .fetch_one(&reopened.database.pool)
            .await
            .unwrap();
    assert_eq!(identity.0, crate::sessions::machine::current().unwrap());
    reopened.close().await;
    let again = SqliteThreadStore::new(&path).await.unwrap();
    let repeated: (String,) =
        sqlx::query_as("SELECT w.machine_id FROM threads t JOIN workspaces w ON w.id=t.workspace_id WHERE t.id = ?1")
            .bind(id)
            .fetch_one(&again.database.pool)
            .await
            .unwrap();
    assert_eq!(identity, repeated);
}

#[tokio::test]
async fn id_recovery_ignores_missing_paths_and_keeps_instance_owners_independent() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let (store, first) = SqliteThreadStore::open_shared(&path).await.unwrap();
    let cwd = directory.path().join("work");
    std::fs::create_dir(&cwd).unwrap();
    let resolved = store.resolve_workspace(&cwd).await.unwrap();
    let id = store
        .create_bound_thread(ThreadMeta::new(cwd.to_str().unwrap()), &resolved)
        .await
        .unwrap();
    let workspace = first
        .validate_bound_workspace(&id, BindingRecheck::Full)
        .await
        .unwrap();
    assert_eq!(workspace.cwd, resolved.cwd);
    let first_run = first.acquire_execution(&id, &workspace).await.unwrap();
    let second = SessionResourcesImpl::open(&path).await.unwrap();
    let second_run = second.acquire_execution(&id, &workspace).await.unwrap();
    first
        .append_history(
            &id,
            &[PersistedPayload::Message(BaseMessage::human(
                "first instance",
            ))],
        )
        .await
        .unwrap();
    assert!(!directory.path().join("threads.db.execution-locks").exists());
    first_run.mark_clean().await.unwrap();
    assert!(first
        .append_history(
            &id,
            &[PersistedPayload::Message(BaseMessage::human(
                "closed first"
            ))]
        )
        .await
        .is_err());
    second
        .append_history(
            &id,
            &[PersistedPayload::Message(BaseMessage::human(
                "second still active",
            ))],
        )
        .await
        .unwrap();
    second_run.mark_clean().await.unwrap();
    assert_eq!(store.load_messages(&id).await.unwrap().len(), 2);
    assert_eq!(
        store.load_meta(&id).await.unwrap().cwd,
        resolved.cwd.to_str().unwrap()
    );
}

#[tokio::test]
async fn environment_filters_do_not_change_id_lookup_and_children_inherit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let (store, facade) = SqliteThreadStore::open_shared(&path).await.unwrap();
    let root = store
        .create_thread(ThreadMeta::new("/same/path"))
        .await
        .unwrap();
    sqlx::query(
        "UPDATE session_environments SET machine_id = 'another-machine' WHERE thread_id = ?1",
    )
    .bind(&root)
    .execute(&store.database.pool)
    .await
    .unwrap();
    let mut child = ThreadMeta::new("/same/path");
    child.parent_thread_id = Some(root.clone());
    let child_id = store.create_thread(child).await.unwrap();
    assert_eq!(
        facade
            .session_environment_id(&child_id)
            .await
            .unwrap()
            .as_deref(),
        Some(crate::sessions::machine::current().unwrap())
    );
    let mut grandchild = ThreadMeta::new("/missing/child-path");
    grandchild.parent_thread_id = Some(child_id.clone());
    let grandchild_id = store.create_thread(grandchild).await.unwrap();
    assert_eq!(
        facade
            .session_environment_id(&grandchild_id)
            .await
            .unwrap()
            .as_deref(),
        Some(crate::sessions::machine::current().unwrap())
    );
    let other = store
        .create_thread(ThreadMeta::new("/same/path"))
        .await
        .unwrap();
    for id in [&root, &other] {
        store
            .append_messages(id, &[BaseMessage::human("visible")])
            .await
            .unwrap();
    }
    let page = facade
        .list_sessions(&ScopedThreadQuery {
            scope: ThreadScope::Environment("another-machine".to_owned()),
            cursor: None,
            limit: 20,
        })
        .await
        .unwrap();
    assert_eq!(page.entries.len(), 0);
    assert_eq!(facade.load_session_meta(&other).await.unwrap().id, other);
    sqlx::query("DELETE FROM session_environments WHERE thread_id = ?1")
        .bind(&child_id)
        .execute(&store.database.pool)
        .await
        .unwrap();
    store.database.init_schema().await.unwrap();
    assert_eq!(
        facade
            .session_environment_id(&child_id)
            .await
            .unwrap()
            .as_deref(),
        Some(crate::sessions::machine::current().unwrap())
    );
}
