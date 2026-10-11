//! 打开路径的形状探测（R3）与批次拒绝面（守卫）的断言。
//!
//! 夹具（真 SQLite 扮演的远端服务端）与 `schema_upgrade_test` 共用——两边断言的是同一份
//! 生产的语句、判定与批次，只是切入的时点不同：这里管**打开**（形状探测与 `open_step`）与
//! **批次的守卫**（前置的形状/行核对与重建之后的历史行数结果守卫），那边管搬运本身与终点形状。

use std::sync::atomic::Ordering;

use peri_acp_types::session_resources::SessionResourceErrorKind;
use turso_serverless::Error as SdkError;

use super::connection::RemoteTransport;
use super::mutation::StoreAccess;
use super::schema::{self, StoreShapeRead};
use super::schema_upgrade::{
    self, GUARD_MESSAGES_COUNT_SQL, GUARD_REGISTRATION_COUNT_SQL, GUARD_REGISTRATION_ROW_SQL,
    GUARD_UNASSIGNED_THREADS_SQL,
};
use super::schema_upgrade_tests::{probed_shape, Fixture};
use super::session_data::{open_step, probe_shape, OpenStep};
use crate::sessions::canonical::{CREATE_INDEXES, CREATE_REBUILD_THREADS_TABLE_SQL};

/// 批次形状：canonical 索引集在**末尾**一次性重放（版本推进之前），每条只出现一次；
/// 拒绝面的守卫落在破坏性语句之前。
#[tokio::test]
async fn upgrade_plan_replays_the_canonical_index_set_before_advancing_the_version() {
    let fixture = Fixture::new().await;
    let store = fixture.store(StoreAccess::ReadWrite);
    let read = schema_upgrade::read_input(&store).await.unwrap();
    let plan = schema_upgrade::upgrade_plan(&fixture.snapshot, &read, "test-machine").unwrap();
    let sql: Vec<&str> = plan.iter().map(|statement| statement.sql).collect();
    let occurrences = |text: &str| sql.iter().filter(|statement| **statement == text).count();

    // 末条是版本与契约推进：`apply_schema_upgrade` 按「末条影响 1 行」确认升级落地。
    let advance = sql.len() - 1;
    assert!(sql[advance].starts_with("UPDATE peri_store_meta SET schema_version"));
    // 索引重放紧邻其前、整份 canonical 清单，且不再有逐表补索引的第二处。
    let indexes = advance - CREATE_INDEXES.len();
    assert_eq!(&sql[indexes..advance], CREATE_INDEXES);
    for index in CREATE_INDEXES {
        assert_eq!(occurrences(index), 1, "{index} 只应重放一次");
    }
    // 登记行守卫在重建 `workspaces` 之前；无归属守卫在重建 `threads` 之前。
    let earlier = |guard: &str, target: &str| {
        let guard_position = sql
            .iter()
            .position(|statement| *statement == guard)
            .unwrap();
        let target_position = sql
            .iter()
            .position(|statement| *statement == target)
            .unwrap();
        assert!(
            guard_position < target_position,
            "{guard} 必须早于 {target}"
        );
    };
    earlier(GUARD_REGISTRATION_COUNT_SQL, "DROP TABLE workspaces");
    earlier(GUARD_REGISTRATION_ROW_SQL, "DROP TABLE workspaces");
    earlier(
        GUARD_UNASSIGNED_THREADS_SQL,
        CREATE_REBUILD_THREADS_TABLE_SQL,
    );
    // 历史行数守卫是**结果守卫**：它必须落在整条重建链之后（前置守卫看不到级联删除）。
    let messages_guard = sql
        .iter()
        .position(|statement| *statement == GUARD_MESSAGES_COUNT_SQL)
        .unwrap();
    let rebuild_end = sql
        .iter()
        .position(|statement| *statement == "DROP TABLE threads_rebuild")
        .unwrap();
    assert!(
        rebuild_end < messages_guard,
        "结果守卫必须在重建链走完之后才核对历史行数"
    );
}

/// 结果守卫的反例：父行检查被打开时 `DROP TABLE threads` 会隐式删除并级联清空 `messages`，
/// 重建链之后的守卫把它拦下——整批回滚，历史原样保留。
#[tokio::test]
async fn upgrade_guard_catches_messages_dropped_by_the_rebuild_cascade() {
    let fixture = Fixture::new_with_foreign_keys(true).await;
    let store = fixture.store(StoreAccess::ReadWrite);
    let read = schema_upgrade::read_input(&store).await.unwrap();
    let plan = schema_upgrade::upgrade_plan(&fixture.snapshot, &read, "test-machine").unwrap();
    let guard = plan
        .iter()
        .position(|statement| statement.sql == GUARD_MESSAGES_COUNT_SQL)
        .unwrap();

    // 直接下发批次：夹具逐条执行并把失败的下标报出来——失败必须正好落在这条守卫上，
    // 说明级联确实发生了，且是在重建之后才被发现的。
    let error = fixture.transport.managed_batch(plan).await.unwrap_err();
    match error {
        SdkError::BatchStatementFailed { index, .. } => assert_eq!(
            index, guard,
            "级联删除必须由重建链之后的结果守卫拦下（实际失败于第 {index} 条）"
        ),
        _ => panic!("expected the batch to fail at the messages guard"),
    }
    // 整批回滚：历史行还在，旧状态与版本保持。
    assert_eq!(
        fixture
            .count("SELECT COUNT(*) FROM messages".to_owned())
            .await,
        1,
        "回滚把级联删掉的历史行带了回来"
    );
    fixture.assert_old_state().await;
}

/// 两批读取之后 `workspaces` 被并发写入：登记行守卫让整批失败，并发行按原值留下。
#[tokio::test]
async fn upgrade_guards_registrations_written_after_its_snapshot() {
    let fixture = Fixture::new().await;
    *fixture.transport.before_batch.lock().unwrap() = Some(
        "INSERT INTO workspaces (id, project_id, root, root_identity, discovery) VALUES
         ('22222222-2222-4222-8222-222222222222', 'project', '/concurrent', 'concurrent identity',
          '{\"root\":\"/concurrent\",\"common_dir\":null,\"private_dir\":null}')",
    );
    assert!(
        schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
            .await
            .is_err()
    );
    fixture.assert_old_state().await;
    let concurrent: (String, String) = sqlx::query_as(
        "SELECT root, root_identity FROM workspaces
         WHERE id = '22222222-2222-4222-8222-222222222222'",
    )
    .fetch_one(&fixture.transport.pool)
    .await
    .unwrap();
    assert_eq!(
        concurrent,
        ("/concurrent".to_owned(), "concurrent identity".to_owned())
    );
    // 原有登记行也在：整批回滚，没有任何行被旧快照覆盖。
    assert_eq!(
        fixture
            .count("SELECT COUNT(*) FROM workspaces".to_owned())
            .await,
        2
    );
    assert_eq!(fixture.transport.writes.load(Ordering::SeqCst), 1);
}

/// 同名对象冒充绑定表（VIEW 返回重复 `thread_id`）：拒绝，而不是让后行覆盖前行。
#[tokio::test]
async fn upgrade_refuses_binding_rows_it_cannot_identify() {
    let fixture = Fixture::new().await;
    // 视图的列与绑定表同名同序：形状判定过得去，重复的主键由 decoder 显式拒绝。
    sqlx::raw_sql(
        "DROP TABLE session_bindings;
         CREATE VIEW session_bindings AS
         SELECT 'session' AS thread_id, 1 AS schema_version, 'project' AS project_id,
                '11111111-1111-4111-8111-111111111111' AS workspace_id, '' AS relative_cwd
         UNION ALL
         SELECT 'session', 1, 'project', '11111111-1111-4111-8111-111111111111', ''",
    )
    .execute(&fixture.transport.pool)
    .await
    .unwrap();

    let error = schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Unsupported
    ));
    assert_eq!(fixture.transport.writes.load(Ordering::SeqCst), 0);
    fixture.assert_old_state().await;
    assert_eq!(
        fixture
            .count(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'view' AND name = 'session_bindings'"
                    .to_owned()
            )
            .await,
        1,
        "拒绝之后同名的对象保持原样"
    );
}

/// 打开路径的形状探测（R3）：v2 库缺 `session_bindings` 时，探测在只读打开上就把它拦下。
#[tokio::test]
async fn open_probe_refuses_a_previous_store_missing_its_bindings_table() {
    let fixture = Fixture::new().await;
    let store = fixture.store(StoreAccess::ReadOnly);
    let read = store.read_identity().await.unwrap();
    assert_eq!(
        probe_shape(&store, &read).await.unwrap(),
        Some(StoreShapeRead::Consistent),
        "完整夹具先探一次：判定不是恒拒绝"
    );

    sqlx::query("DROP TABLE session_bindings")
        .execute(&fixture.transport.pool)
        .await
        .unwrap();
    let shape = probe_shape(&store, &read).await.unwrap();
    assert!(
        matches!(&shape, Some(StoreShapeRead::Drifted(detail)) if detail.contains("session_bindings")),
        "{shape:?}"
    );
    for access in [StoreAccess::ReadOnly, StoreAccess::ReadWrite] {
        let error = open_step(read.clone(), shape.clone(), access).unwrap_err();
        assert!(
            matches!(error.kind(), SessionResourceErrorKind::Unsupported),
            "形状漂移必须在打开路径被拒绝（{access:?}）"
        );
    }
    assert_eq!(
        fixture.transport.writes.load(Ordering::SeqCst),
        0,
        "探测只发 SELECT"
    );
}

/// 打开路径的形状探测（R3）：本代库（v5 + 11）的 `threads` 漂移，同样在打开时就拒绝。
#[tokio::test]
async fn open_probe_refuses_a_current_store_with_drifted_threads() {
    let fixture = Fixture::new().await;
    schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
        .await
        .unwrap();
    let store = fixture.store(StoreAccess::ReadOnly);
    let read = store.read_identity().await.unwrap();
    assert_eq!(
        probe_shape(&store, &read).await.unwrap(),
        Some(StoreShapeRead::Consistent),
        "升级终点先探一次：迁移留下的形状就是 canonical"
    );

    sqlx::query("ALTER TABLE threads ADD COLUMN extension_data TEXT")
        .execute(&fixture.transport.pool)
        .await
        .unwrap();
    let shape = probe_shape(&store, &read).await.unwrap();
    assert!(
        matches!(&shape, Some(StoreShapeRead::Drifted(detail)) if detail.contains("threads")),
        "{shape:?}"
    );
    let error = open_step(read, shape, StoreAccess::ReadWrite).unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Unsupported
    ));
}

/// 身份先于会话表建立的库（只有 meta 与账本）：探测结论是「形状还没建立」——不是漂移。
/// 写打开照旧补齐（本代库的幂等 DDL），只读打开拒绝；两者的判定都由 `open_step` 给出。
#[tokio::test]
async fn open_probe_reports_a_store_without_session_tables_as_unbuilt() {
    let fixture = Fixture::new().await;
    sqlx::raw_sql(
        "DROP TABLE thread_goals; DROP TABLE execution_runs; DROP TABLE messages; DROP TABLE threads;
         DROP TABLE session_bindings; DROP TABLE session_environments; DROP TABLE workspaces;
         DROP TABLE projects; DROP TABLE mcp_oauth_credentials",
    )
    .execute(&fixture.transport.pool)
    .await
    .unwrap();
    fixture
        .declare(schema::REMOTE_SCHEMA_VERSION, schema::STORE_CONTRACT)
        .await;

    let store = fixture.store(StoreAccess::ReadWrite);
    let read = store.read_identity().await.unwrap();
    assert_eq!(
        probe_shape(&store, &read).await.unwrap(),
        Some(StoreShapeRead::Unbuilt),
        "一张声明的会话表都没有 = 形状还没建立"
    );
    assert!(matches!(
        open_step(
            read.clone(),
            Some(StoreShapeRead::Unbuilt),
            StoreAccess::ReadWrite
        )
        .unwrap(),
        OpenStep::Existing(_)
    ));
    let error = open_step(read, Some(StoreShapeRead::Unbuilt), StoreAccess::ReadOnly).unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Unsupported
    ));
}

/// 探测结论对生产路径的接线：`probed_shape` 是 `open_step` 拿到的第二个事实。
#[tokio::test]
async fn open_probe_serves_a_store_whose_shape_matches_its_contract() {
    let fixture = Fixture::new().await;
    let store = fixture.store(StoreAccess::ReadWrite);
    let read = store.read_identity().await.unwrap();
    assert_eq!(
        probed_shape(&store).await,
        Some(StoreShapeRead::Consistent),
        "压缩前形状（v2）在探测上就是「迁移输入」"
    );
    assert!(matches!(
        open_step(read, probed_shape(&store).await, StoreAccess::ReadWrite).unwrap(),
        OpenStep::Upgrade(_)
    ));
    assert_eq!(
        fixture.transport.writes.load(Ordering::SeqCst),
        0,
        "探测与打开判定都不发写批次：写打开之后才由迁移批次补齐"
    );
}
