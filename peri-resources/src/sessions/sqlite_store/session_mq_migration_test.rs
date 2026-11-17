// Queue schema creation for supported older store generations.

// ─── 迁移：v13 → v14、v12 → v14 与 v11 → v14 ─────────────────────────────────

/// 把库降级成 v13 形状（删两列工作快照）或更早形状（再删 `thread_mq`）后重开：
/// 幂等补齐并推进到当前版本。
#[tokio::test]
async fn migration_adds_thread_mq_from_the_previous_generation() {
    let fixture = Fixture::new().await;
    let thread = fixture.thread("mq").await;
    let id = MessageId::new();
    fixture
        .data
        .mq_enqueue(&thread, input(id, "before downgrade"))
        .await
        .unwrap();
    let path = fixture.path();
    fixture.store.close().await;
    downgrade(&path, 12).await;

    let reopened = SqliteThreadStore::try_new(&path).await.unwrap();
    assert_mq_shape(&reopened).await;
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&reopened.database.pool)
        .await
        .unwrap();
    assert_eq!(version, canonical::CURRENT_SCHEMA_VERSION);
    reopened.close().await;
}

/// v11 形状（缺 inherited 派生索引表与 `thread_mq`）走同一条幂等补齐路径。
#[tokio::test]
async fn migration_adds_thread_mq_from_the_legacy_current_generation() {
    let fixture = Fixture::new().await;
    let thread = fixture.thread("mq").await;
    fixture
        .data
        .mq_enqueue(&thread, input(MessageId::new(), "kept?"))
        .await
        .unwrap();
    let path = fixture.path();
    fixture.store.close().await;
    downgrade(&path, 11).await;

    let reopened = SqliteThreadStore::try_new(&path).await.unwrap();
    assert_mq_shape(&reopened).await;
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&reopened.database.pool)
        .await
        .unwrap();
    assert_eq!(version, canonical::CURRENT_SCHEMA_VERSION);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages")
        .fetch_one(&reopened.database.pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "会话事实不因迁移被改写");
    reopened.close().await;
}

async fn downgrade(path: &Path, version: i64) {
    // 形状退回与 `schema_v14_test` 共用同一份夹具：v15 改形的三张表必须整表重建，
    // 否则列序（`identity` 在 `discovery` 之前）不满足旧代际的形状判定。
    super::schema_v14_tests::downgrade(path, version, true).await;
}

async fn assert_mq_shape(store: &SqliteThreadStore) {
    let (table,): (String,) = sqlx::query_as(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name = 'thread_mq'",
    )
    .fetch_one(&store.database.pool)
    .await
    .unwrap();
    assert_eq!(table, "thread_mq");
    let (index,): (String,) = sqlx::query_as(
        "SELECT name FROM sqlite_schema WHERE type = 'index' AND name = 'idx_thread_mq_order'",
    )
    .fetch_one(&store.database.pool)
    .await
    .unwrap();
    assert_eq!(index, "idx_thread_mq_order");
}
