//! Inherited snapshots must survive a new store and reject corrupt state.
use super::*;

#[tokio::test]
async fn test_inherited_context_freezes_payloads_and_flags_across_store_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inherited.db");
    let store = SqliteThreadStore::new(&path).await.unwrap();
    let parent_id = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();
    let parent = BaseMessage::human("parent snapshot");
    store
        .append_messages(&parent_id, std::slice::from_ref(&parent))
        .await
        .unwrap();
    let flags = MessageFlags {
        truncated: true,
        ..Default::default()
    };
    let mut child_meta = ThreadMeta::new("/tmp");
    child_meta.parent_thread_id = Some(parent_id.clone());
    child_meta.snapshot_at_message_id = Some(parent.id().as_uuid().to_string());
    let child_id = store.create_thread(child_meta).await.unwrap();
    store
        .store_inherited_context(
            &child_id,
            &InheritedContext {
                payloads: vec![PersistedPayload::Message(parent.clone())],
                flags: HashMap::from([(parent.id(), flags.clone())]),
            },
        )
        .await
        .unwrap();
    let own = BaseMessage::human("child own");
    store
        .append_messages(&child_id, std::slice::from_ref(&own))
        .await
        .unwrap();
    store
        .update_message_flags(
            &parent.id(),
            &MessageFlags {
                excluded: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    store
        .delete_messages(&parent_id, &[parent.id()])
        .await
        .unwrap();
    store.close().await;
    let reopened = SqliteThreadStore::new(&path).await.unwrap();
    let inherited = reopened.load_inherited_context(&child_id).await.unwrap();
    assert_eq!(
        inherited.payloads[0].as_message().unwrap().content(),
        "parent snapshot"
    );
    assert_eq!(
        inherited.flags[&parent.id()],
        flags,
        "父线程后续状态不能改写已冻结的子上下文"
    );
    let context = reopened.load_context_payloads(&child_id).await.unwrap();
    assert_eq!(
        context.iter().map(PersistedPayload::id).collect::<Vec<_>>(),
        vec![parent.id(), own.id()]
    );
    assert_eq!(
        reopened.load_payloads(&child_id).await.unwrap().len(),
        1,
        "父 ID 不得进入 child own rows"
    );
    assert!(reopened
        .store_inherited_context(&child_id, &InheritedContext::default())
        .await
        .unwrap_err()
        .to_string()
        .contains("already exists"));
    reopened.close().await;
}

#[tokio::test]
async fn test_inherited_context_rejects_future_corrupt_and_foreign_flags_without_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteThreadStore::new(dir.path().join("invalid.db"))
        .await
        .unwrap();
    let id = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();
    let future = r#"{"version":2,"payloads":[],"flags":{}}"#;
    for snapshot in ["{", future] {
        sqlx::query("UPDATE threads SET inherited_context = ?1 WHERE id = ?2")
            .bind(snapshot)
            .bind(&id)
            .execute(&store.pool)
            .await
            .unwrap();
        let error = store.load_inherited_context(&id).await.unwrap_err();
        if snapshot == future {
            assert!(error
                .to_string()
                .contains("unsupported inherited context version"));
        }
        assert!(store
            .store_inherited_context(&id, &InheritedContext::default())
            .await
            .is_err());
        let raw: (String,) = sqlx::query_as("SELECT inherited_context FROM threads WHERE id = ?1")
            .bind(&id)
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(raw.0, snapshot);
    }
    let fresh = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();
    let invalid = InheritedContext {
        payloads: vec![],
        flags: HashMap::from([(BaseMessage::human("foreign").id(), MessageFlags::default())]),
    };
    assert!(store
        .store_inherited_context(&fresh, &invalid)
        .await
        .unwrap_err()
        .to_string()
        .contains("invalid inherited context message references"));
    assert!(store
        .load_inherited_context(&fresh)
        .await
        .unwrap()
        .payloads
        .is_empty());
    store.close().await;
}

#[tokio::test]
async fn test_inherited_context_legacy_missing_cutoff_and_cycle_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteThreadStore::new(dir.path().join("legacy.db"))
        .await
        .unwrap();
    let parent_id = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();
    let mut child_meta = ThreadMeta::new("/tmp");
    child_meta.parent_thread_id = Some(parent_id.clone());
    child_meta.snapshot_at_message_id =
        Some(BaseMessage::human("missing").id().as_uuid().to_string());
    let child_id = store.create_thread(child_meta).await.unwrap();
    assert!(store
        .load_inherited_context(&child_id)
        .await
        .unwrap_err()
        .to_string()
        .contains("cutoff is missing"));
    let mut parent_meta = store.load_meta(&parent_id).await.unwrap();
    parent_meta.parent_thread_id = Some(child_id.clone());
    store.update_meta(&parent_id, parent_meta).await.unwrap();
    assert!(store
        .load_inherited_context(&child_id)
        .await
        .unwrap_err()
        .to_string()
        .contains("cyclic thread ancestry"));
    store.close().await;
}
