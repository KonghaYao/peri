use super::*;

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
                .create_bound_thread(
                    ThreadMeta::new_at(workspace.cwd.to_str().unwrap(), peri_time::now_wall()),
                    &workspace,
                )
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

#[tokio::test]
async fn independent_handles_write_without_execution_registration() {
    let repo = repository();
    let (store, database) = store().await;
    let (id, _) = bound(&store, repo.path()).await;
    let second = SqliteThreadStore::new(database.path().join("threads.db"))
        .await
        .unwrap();
    store
        .append_message(&id, BaseMessage::human("first"))
        .await
        .unwrap();
    second
        .append_message(&id, BaseMessage::human("second"))
        .await
        .unwrap();
    assert_eq!(store.load_messages(&id).await.unwrap().len(), 2);
    assert!(!database.path().join("threads.db.execution-locks").exists());
}

#[tokio::test]
async fn readonly_store_rejects_mutation_without_execution_registration() {
    let repo = repository();
    let (store, database) = store().await;
    let (id, _) = bound(&store, repo.path()).await;
    let reader = SqliteThreadStore::open_existing_read_only(database.path().join("threads.db"))
        .await
        .unwrap();
    assert!(reader
        .append_message(&id, BaseMessage::human("rejected"))
        .await
        .is_err());
    assert!(reader.load_messages(&id).await.unwrap().is_empty());
}
