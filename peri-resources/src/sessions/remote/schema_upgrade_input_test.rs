//! 迁移输入的**合法退化**：远端正式发布的 v2 库不保证每张可选表都在。
//!
//! 夹具与 `schema_upgrade_test` 共用。这里断言的是两条与「输入缺损」有关的语义：
//! 环境表整表缺席时升级照常完成（删除语句必须对缺表幂等），以及库里没有会话但有登记行时
//! 登记证据必须完整保留（机器归属落到当前机器，不能静默丢行）。

use std::sync::atomic::Ordering;

use peri_acp_types::session_resources::SessionResourceErrorKind;

use super::mutation::StoreAccess;
use super::schema::{self, StoreShapeRead};
use super::schema_upgrade;
use super::schema_upgrade_tests::{Fixture, DISCOVERY};

/// v2 库没有 `session_environments` 表（正式基线 10 的库可能整表缺席）：
/// 会话的机器归属按缺行处理，升级照常完成——删除语句对缺表幂等，不因 no such table 失败。
///
/// 这条用例覆盖的正是「无条件 `DROP TABLE` 打不动缺表库」的回归：删除语句是
/// `always_dropped`（缺表也照常下发），所以本用例的 `upgrade` 成功本身就是它幂等的证据。
#[tokio::test]
async fn upgrade_accepts_a_previous_store_without_environments() {
    let fixture = Fixture::new().await;
    sqlx::query("DROP TABLE session_environments")
        .execute(&fixture.transport.pool)
        .await
        .unwrap();

    schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
        .await
        .unwrap();

    assert_eq!(fixture.version().await, schema::REMOTE_SCHEMA_VERSION);
    assert_eq!(fixture.contract().await, schema::STORE_CONTRACT);
    // 机器归属缺行 → `legacy:<store id>` 占位机器（与本机未知机器同类），不是当前机器。
    let (machine,): (String,) = sqlx::query_as(
        "SELECT machine_id FROM workspaces WHERE id = '11111111-1111-4111-8111-111111111111'",
    )
    .fetch_one(&fixture.transport.pool)
    .await
    .unwrap();
    assert_eq!(machine, "legacy:existing-store");
    let (machines,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM machines")
        .fetch_one(&fixture.transport.pool)
        .await
        .unwrap();
    assert_eq!(machines, 2, "占位机器与当前机器各一行");
    // 升级终点仍然逐列等于 canonical（环境表不参与当前形状）。
    assert_eq!(
        fixture.shape_verdict(schema::ShapeProbe::Current).await,
        StoreShapeRead::Consistent
    );
}

/// 库里有登记行但 threads 空（会话都被删过）：登记行按原 UUID 保留，证据三列不丢，
/// 机器归属落到本次迁移的当前机器（`plan_workspace_rows` 的兜底机器集合非空）。
#[tokio::test]
async fn upgrade_keeps_registration_evidence_without_any_sessions() {
    let fixture = Fixture::new().await;
    // 所有会话事实都删掉（迁移输入只剩登记行）；绑定行随会话一起消失，否则绑定找不到归属
    // 会话本身就会被拒绝（那是另一种拒绝面）。
    sqlx::raw_sql(
        "DELETE FROM messages; DELETE FROM session_bindings; DELETE FROM session_environments;
         DELETE FROM threads",
    )
    .execute(&fixture.transport.pool)
    .await
    .unwrap();

    // 用显式机器身份构造批次：断言的是规划语义（登记行落在哪台机器上），与本次运行的本机
    // 身份无关，因此这里不走 `upgrade`（它会去取真实机器身份）。
    let store = fixture.store(StoreAccess::ReadWrite);
    let read = schema_upgrade::read_input(&store).await.unwrap();
    let plan = schema_upgrade::upgrade_plan(&fixture.snapshot, &read, "current-machine").unwrap();
    store.apply_schema_upgrade(plan).await.unwrap();

    let (id, machine, project, identity, discovery): (String, String, String, String, String) =
        sqlx::query_as("SELECT id, machine_id, project_id, identity, discovery FROM workspaces")
            .fetch_one(&fixture.transport.pool)
            .await
            .unwrap();
    assert_eq!(
        id, "11111111-1111-4111-8111-111111111111",
        "保留行沿用旧登记 UUID"
    );
    assert_eq!(machine, "current-machine", "没有会话引用时落在当前机器上");
    assert_eq!(project, "project");
    assert_eq!(identity, "root identity");
    assert_eq!(discovery, DISCOVERY);
    assert_eq!(
        fixture
            .count("SELECT COUNT(*) FROM threads".to_owned())
            .await,
        0
    );
    assert_eq!(
        fixture
            .count("SELECT COUNT(*) FROM session_bindings".to_owned())
            .await,
        0
    );
    assert_eq!(fixture.version().await, schema::REMOTE_SCHEMA_VERSION);
    assert_eq!(fixture.contract().await, schema::STORE_CONTRACT);
}

/// 迁移输入缺 `messages` 表：形状判定拒绝整条迁移（分类仍是「不认识」），批次不下发。
#[tokio::test]
async fn upgrade_refuses_an_input_without_the_history_table() {
    let fixture = Fixture::new().await;
    sqlx::query("DROP TABLE messages")
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
}

/// 同名异形的 `session_environments`（列集合仍合法，但 `thread_id` 被声明成 NOT NULL 且没有
/// 主键）：输入列的上下界过得去，**删除前**的严格判定不过——迁移不认识的对象不删，整批不下发。
#[tokio::test]
async fn upgrade_refuses_an_environments_table_of_another_shape() {
    let fixture = Fixture::new().await;
    sqlx::raw_sql(
        "DROP TABLE session_environments;
         CREATE TABLE session_environments (thread_id TEXT NOT NULL, machine_id TEXT NOT NULL)",
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
            .count("SELECT COUNT(*) FROM pragma_table_xinfo('session_environments')".to_owned())
            .await,
        2,
        "同名异形的表保持原样（没有被删）"
    );
}
