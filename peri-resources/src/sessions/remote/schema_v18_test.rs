use super::*;

const RECOVERY_TABLES: &[&str] = &[
    "session_work_commands",
    "session_work_state",
    "session_work_events",
    "session_work_receipts",
    "session_control_state",
    "session_control_receipts",
];

async fn v17_fixture() -> (Fixture, RemoteStore, StoreSnapshot) {
    let fixture = Fixture::new().await;
    let store = fixture.store(StoreAccess::ReadWrite);
    schema_upgrade::upgrade(&store, &fixture.snapshot)
        .await
        .unwrap();
    // 升级链已经走到 v19，夹具要的是 v12..=18 的库：把 v19 删除的执行登记表补回来，
    // 版本与契约一起退回上一代（真实 v17 库的元数据就是 (17, v3) 加这张表）。
    fixture.declare_previous_generation(17).await;
    for (_, definition) in crate::sessions::schema_cleanup::RETIRED_EXECUTION_TABLES {
        sqlx::query(*definition)
            .execute(&fixture.transport.pool)
            .await
            .unwrap();
    }
    let StoreIdentityRead::Present(snapshot) = store.read_identity().await.unwrap() else {
        panic!("missing v17 identity")
    };
    (fixture, store, snapshot)
}

#[tokio::test]
async fn remote_schema_v18_drops_six_tables_without_rebuilding_history_or_business_tables() {
    let (fixture, store, snapshot) = v17_fixture().await;
    let pool = &fixture.transport.pool;
    sqlx::raw_sql(
        "CREATE INDEX extension_history_index ON messages(role);
        CREATE VIEW extension_goal_view AS SELECT * FROM thread_goals;",
    )
    .execute(pool)
    .await
    .unwrap();
    let before: Vec<(String, i64, Option<String>)> = sqlx::query_as(
        "SELECT name, rootpage, sql FROM sqlite_schema WHERE tbl_name NOT LIKE 'session_work_%'
         AND tbl_name NOT LIKE 'session_control_%'
         AND tbl_name NOT IN ('legacy_execution_registrations', 'session_bindings') ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let identity: (String, String, String) =
        sqlx::query_as("SELECT store_id, contract, created_at FROM peri_store_meta")
            .fetch_one(pool)
            .await
            .unwrap();
    schema_upgrade::upgrade(&store, &snapshot).await.unwrap();
    // 这一次调用走完 17 → 18 → 19 两步：v19 有意删除执行登记表、重建 `session_bindings`
    // （去掉指向它的外键），两者不在「不重建历史与业务表」的比对里；其余对象原样保留。
    let after: Vec<(String, i64, Option<String>)> = sqlx::query_as(
        "SELECT name, rootpage, sql FROM sqlite_schema
         WHERE tbl_name NOT IN ('legacy_execution_registrations', 'session_bindings') ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(before, after);
    let registrations: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_schema WHERE tbl_name = 'legacy_execution_registrations'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(registrations, 0);
    let after_identity: (String, String, String) =
        sqlx::query_as("SELECT store_id, contract, created_at FROM peri_store_meta")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(identity.0, after_identity.0);
    assert_eq!(identity.2, after_identity.2);
    assert_eq!(after_identity.1, schema::STORE_CONTRACT);
    let history: (i64, String) = sqlx::query_as("SELECT rowid, content FROM messages")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(history, (7, "original content".into()));
    let receipt: String = sqlx::query_scalar("SELECT receipt FROM peri_op_ledger")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(receipt, "receipt");
    assert_eq!(fixture.version().await, schema::REMOTE_SCHEMA_VERSION);
    for table in RECOVERY_TABLES {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_schema WHERE name = ?1")
            .bind(table)
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
}

#[tokio::test]
async fn remote_schema_v18_guards_version_store_identity_and_contract_before_deletion() {
    for mutation in [
        "UPDATE peri_store_meta SET schema_version = 18",
        "UPDATE peri_store_meta SET store_id = 'replacement-store'",
        "UPDATE peri_store_meta SET contract = 'unknown-contract'",
    ] {
        let (fixture, store, snapshot) = v17_fixture().await;
        sqlx::query(mutation)
            .execute(&fixture.transport.pool)
            .await
            .unwrap();
        let before: (i64, String, String) =
            sqlx::query_as("SELECT schema_version, store_id, contract FROM peri_store_meta")
                .fetch_one(&fixture.transport.pool)
                .await
                .unwrap();
        assert!(schema_upgrade::upgrade(&store, &snapshot).await.is_err());
        let after: (i64, String, String) =
            sqlx::query_as("SELECT schema_version, store_id, contract FROM peri_store_meta")
                .fetch_one(&fixture.transport.pool)
                .await
                .unwrap();
        assert_eq!(before, after);
        for table in RECOVERY_TABLES {
            let count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_schema WHERE name = ?1")
                    .bind(table)
                    .fetch_one(&fixture.transport.pool)
                    .await
                    .unwrap();
            assert_eq!(count, 1);
        }
    }
}

#[tokio::test]
async fn remote_schema_v18_failed_drop_rolls_back_prior_deletions_and_version() {
    let (fixture, store, snapshot) = v17_fixture().await;
    fixture
        .transport
        .fail_recovery_drop
        .store(true, Ordering::SeqCst);
    assert!(schema_upgrade::upgrade(&store, &snapshot).await.is_err());
    assert_eq!(fixture.version().await, 17);
    for table in RECOVERY_TABLES {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_schema WHERE name = ?1")
            .bind(table)
            .fetch_one(&fixture.transport.pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }
}

#[tokio::test]
async fn remote_schema_v18_refuses_unknown_shapes_and_external_references_before_writing() {
    for setup in [
        "ALTER TABLE session_work_state ADD COLUMN extension TEXT",
        "DROP TABLE session_work_state; CREATE VIEW session_work_state AS SELECT 1",
        "CREATE VIEW extension_view AS SELECT * FROM session_work_state",
        "CREATE TABLE extension_fk (id TEXT REFERENCES session_work_state(session_id))",
        "CREATE TRIGGER extension_trigger AFTER UPDATE ON threads BEGIN DELETE FROM session_work_state; END",
        "CREATE TRIGGER attached_trigger AFTER INSERT ON session_work_state BEGIN SELECT 1; END",
        "CREATE INDEX extension_index ON session_work_state(state_json)",
    ] {
        let (fixture, store, snapshot) = v17_fixture().await;
        sqlx::raw_sql(sqlx::AssertSqlSafe(setup.to_owned()))
            .execute(&fixture.transport.pool).await.unwrap();
        let before: Vec<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT type, name, sql FROM sqlite_schema ORDER BY name",
        ).fetch_all(&fixture.transport.pool).await.unwrap();
        let writes = fixture.transport.writes.load(Ordering::SeqCst);
        let error = schema_upgrade::upgrade(&store, &snapshot).await.unwrap_err();
        assert!(matches!(error.kind(), SessionResourceErrorKind::Unsupported));
        assert_eq!(writes, fixture.transport.writes.load(Ordering::SeqCst));
        let after: Vec<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT type, name, sql FROM sqlite_schema ORDER BY name",
        ).fetch_all(&fixture.transport.pool).await.unwrap();
        assert_eq!(before, after);
        assert_eq!(fixture.version().await, 17);
    }
}

#[tokio::test]
async fn remote_schema_v18_rechecks_external_objects_created_after_validation() {
    let (fixture, store, snapshot) = v17_fixture().await;
    fixture
        .transport
        .change_schema
        .store(true, Ordering::SeqCst);
    assert!(schema_upgrade::upgrade(&store, &snapshot).await.is_err());
    assert_eq!(fixture.version().await, 17);
    for table in RECOVERY_TABLES {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_schema WHERE name = ?1")
            .bind(table)
            .fetch_one(&fixture.transport.pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }
}

#[tokio::test]
async fn remote_schema_v18_lost_or_incomplete_commit_confirmation_stays_unknown() {
    for truncated in [false, true] {
        let (fixture, store, snapshot) = v17_fixture().await;
        if truncated {
            fixture
                .transport
                .truncate_reply
                .store(true, Ordering::SeqCst);
        } else {
            fixture.transport.drop_reply.store(true, Ordering::SeqCst);
        }
        let error = schema_upgrade::upgrade(&store, &snapshot)
            .await
            .unwrap_err();
        assert_eq!(
            error.effect(),
            peri_acp_types::session_resources::MutationOutcome::Unknown
        );
        // 17 → 19 分两段：丢失的回复属于 17 → 18 这一段，已提交的版本停在 18，
        // 下一次写打开读到这个中间态时还会要求补完 v19 升级。
        assert_eq!(fixture.version().await, 18);
        assert!(matches!(
            open_step(store.read_identity().await.unwrap(), StoreAccess::ReadWrite).unwrap(),
            OpenStep::Upgrade(_)
        ));
    }
}
