//! Swarm 窄口的远端行为与 v15 → v16 追加升级。
//!
//! 传输夹具仍由 SQLite 扮演「远端服务端」（见 `schema_upgrade_tests::Fixture`）：批次是真实
//! 的 `BEGIN IMMEDIATE`/`COMMIT`，冲突是真实的唯一键冲突，因此这里证明的是端口在既有三分类
//! 与语句形态上的行为，不是某个 mock 的约定。
//!
//! 真实 Loom 专属断言（服务端 `rows_affected` 与 `ON CONFLICT` 的线上行为）不在本阶段执行；
//! 本模块覆盖的是同一批静态 SQL 在托管批次语义下的可复现结论。

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::session_resources::{
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};
use turso_serverless::Error as SdkError;

use super::connection::RemoteTransport;
use super::execution::RemoteExecution;
use super::generation::{ConnectionFactory, ConnectionGate};
use super::mutation::{RemoteStore, StoreAccess};
use super::schema::{self, StoreId, StoreShapeRead};
use super::schema_upgrade;
use super::schema_upgrade_tests::{Fixture, SqliteTransport};
use super::session_data::{probe_shape, OpenStep, RemoteSessionData};
use super::sql::StatementSpec;
use super::swarm_store::RemoteSwarmStore;
use crate::sessions::data::SessionDataPort;
use crate::sessions::local_port::LocalExecutionPort;
use crate::sessions::resources::SessionDataHome;
use crate::sessions::{
    SessionResourcesImpl, SwarmBindingRow, SwarmInputPut, SwarmInputRow, SwarmItemRow,
    SwarmSessionRow, SwarmSessionUpdate, SwarmStorePort,
};

/// 夹具不重连：本模块的失败路径只验证「本次调用的结论」，重建不是被测行为。
struct NoReconnect;

#[async_trait]
impl ConnectionFactory for NoReconnect {
    async fn connect(&self) -> SessionResourceResult<RemoteStore> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unavailable {
                detail: "test fixture does not reconnect".to_owned(),
            },
        ))
    }
}

fn port_with(transport: Arc<SqliteTransport>, access: StoreAccess) -> Arc<dyn SwarmStorePort> {
    let gate = Arc::new(ConnectionGate::default());
    let store = RemoteStore::new(transport, access, gate.mint(), gate.clone());
    let data = Arc::new(RemoteSessionData::with_connection_for_test(
        StoreId::mint(),
        store,
        Arc::new(NoReconnect),
        gate,
    ));
    Arc::new(RemoteSwarmStore::new(data))
}

fn session_row() -> SwarmSessionRow {
    SwarmSessionRow {
        logical_id: "agent-one".to_owned(),
        root_session_id: "root-session-one".to_owned(),
        actor_key: "swarm-actor-one".to_owned(),
        cwd: "/work".to_owned(),
        state: "idle".to_owned(),
        model: Some("gpt-5".to_owned()),
        instructions: Some("be precise".to_owned()),
        environment_json: r#"{"type":"none","id":"agent-one"}"#.to_owned(),
        created_at: "2026-01-01T00:00:00Z".to_owned(),
        last_activity_at: "2026-01-01T00:00:00Z".to_owned(),
        last_item_seq: 0,
    }
}

fn item_row(seq: u64, item_id: &str) -> SwarmItemRow {
    SwarmItemRow {
        logical_id: "agent-one".to_owned(),
        seq,
        item_id: item_id.to_owned(),
        kind: "message".to_owned(),
        source_json: r#"{"agentId":"root"}"#.to_owned(),
        payload_json: format!(r#"{{"seq":{seq}}}"#),
        created_at: "2026-01-01T00:00:01Z".to_owned(),
        truncated: false,
    }
}

fn input_row(fingerprint: &str) -> SwarmInputRow {
    SwarmInputRow {
        logical_id: "agent-one".to_owned(),
        input_id: "input-one".to_owned(),
        fingerprint: fingerprint.to_owned(),
        state: "accepted".to_owned(),
        created_at: "2026-01-01T00:00:02Z".to_owned(),
    }
}

// ─── 端口语义 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn sessions_round_trip_upsert_and_update() {
    let fixture = Fixture::current_shape().await;
    let port = port_with(fixture.transport.clone(), StoreAccess::ReadWrite);

    port.swarm_upsert_session(session_row()).await.unwrap();
    assert_eq!(
        port.swarm_get_session("agent-one").await.unwrap(),
        Some(session_row()),
        "instructions 与 environment 必须逐字落库"
    );

    // 同一身份的再次 upsert 更新可变字段（不改 cwd / root_session_id）。
    let mut updated = session_row();
    updated.state = "running".to_owned();
    updated.model = None;
    updated.last_item_seq = 4;
    updated.last_activity_at = "2026-01-01T00:00:09Z".to_owned();
    port.swarm_upsert_session(updated.clone()).await.unwrap();
    assert_eq!(
        port.swarm_get_session("agent-one").await.unwrap(),
        Some(updated)
    );

    // 身份不同即冲突：不覆盖、不改号。
    let mut moved = session_row();
    moved.cwd = "/elsewhere".to_owned();
    let conflict = port.swarm_upsert_session(moved).await.unwrap_err();
    assert!(matches!(
        conflict.kind(),
        SessionResourceErrorKind::Conflict { .. }
    ));

    // 另一个逻辑会话占用同一 root session：唯一键冲突同样如实上报。
    let mut second = session_row();
    second.logical_id = "agent-two".to_owned();
    let conflict = port.swarm_upsert_session(second).await.unwrap_err();
    assert!(matches!(
        conflict.kind(),
        SessionResourceErrorKind::Conflict { .. }
    ));

    // 定向更新：命中一行即成功，目标不存在即 NotFound（不伪造成功）。
    port.swarm_update_session(SwarmSessionUpdate {
        logical_id: "agent-one".to_owned(),
        state: "stopped".to_owned(),
        last_item_seq: 5,
        last_activity_at: "2026-01-01T00:00:10Z".to_owned(),
    })
    .await
    .unwrap();
    let stored = port.swarm_get_session("agent-one").await.unwrap().unwrap();
    assert_eq!(stored.state, "stopped");
    assert_eq!(stored.last_item_seq, 5);
    let missing = port
        .swarm_update_session(SwarmSessionUpdate {
            logical_id: "agent-missing".to_owned(),
            state: "idle".to_owned(),
            last_item_seq: 0,
            last_activity_at: "2026-01-01T00:00:00Z".to_owned(),
        })
        .await
        .unwrap_err();
    assert!(matches!(missing.kind(), SessionResourceErrorKind::NotFound));
    assert_eq!(port.swarm_get_session("agent-missing").await.unwrap(), None);
}

#[tokio::test]
async fn items_are_ordered_and_conflicts_are_not_renumbered() {
    let fixture = Fixture::current_shape().await;
    let port = port_with(fixture.transport.clone(), StoreAccess::ReadWrite);
    for (seq, item_id) in [(1, "itim-1"), (2, "itim-2"), (3, "itim-3")] {
        port.swarm_append_item(item_row(seq, item_id))
            .await
            .unwrap();
    }

    let ascending: Vec<u64> = port
        .swarm_items_after("agent-one", 0, 10)
        .await
        .unwrap()
        .iter()
        .map(|row| row.seq)
        .collect();
    assert_eq!(ascending, vec![1, 2, 3]);
    assert_eq!(
        port.swarm_items_after("agent-one", 2, 1)
            .await
            .unwrap()
            .iter()
            .map(|row| row.seq)
            .collect::<Vec<_>>(),
        vec![3]
    );
    assert!(port
        .swarm_items_after("other", 0, 10)
        .await
        .unwrap()
        .is_empty());

    // 降序：None 取最新一页，Some(before) 取严格小于 before 的行。
    assert_eq!(
        port.swarm_items_before("agent-one", None, 2)
            .await
            .unwrap()
            .iter()
            .map(|row| row.seq)
            .collect::<Vec<_>>(),
        vec![3, 2]
    );
    assert_eq!(
        port.swarm_items_before("agent-one", Some(2), 10)
            .await
            .unwrap()
            .iter()
            .map(|row| row.seq)
            .collect::<Vec<_>>(),
        vec![1]
    );
    assert!(port
        .swarm_items_before("agent-one", Some(1), 10)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(port.swarm_last_seq("agent-one").await.unwrap(), 3);
    assert_eq!(port.swarm_last_seq("other").await.unwrap(), 0);

    // 同一 (logical_id, seq) 重放：冲突，不改号。
    let duplicate_seq = port
        .swarm_append_item(item_row(2, "itim-other"))
        .await
        .unwrap_err();
    assert!(matches!(
        duplicate_seq.kind(),
        SessionResourceErrorKind::Conflict { .. }
    ));
    // 同一 item_id 跨 seq 复用**不是**冲突：这是 S4 reader 的 delta + complete 形状，
    // 由下面的专门用例逐行验证。
    assert_eq!(port.swarm_last_seq("agent-one").await.unwrap(), 3);
}

/// 回归（S6b2 §4）：同一 `logical_id`、同一 `item_id`、不同 `seq` 的两行都必须 append
/// 成功，并按 seq 升序读回。schema 16 曾声明 `UNIQUE(item_id)`，使逐 delta 行流在第二个
/// delta 上撞唯一键、reader 重试后把会话置 `failed`；本用例锁定修正后的端口语义。
#[tokio::test]
async fn repeated_item_ids_are_delta_and_complete_rows_not_a_conflict() {
    let fixture = Fixture::current_shape().await;
    let port = port_with(fixture.transport.clone(), StoreAccess::ReadWrite);

    let mut delta = item_row(1, "itim-stream-1");
    delta.payload_json = r#"{"messageId":"m-1","text":"stub-","delta":true}"#.to_owned();
    let mut complete = item_row(2, "itim-stream-1");
    complete.payload_json = r#"{"messageId":"m-1","text":"stub-0","complete":true}"#.to_owned();

    port.swarm_append_item(delta.clone()).await.unwrap();
    port.swarm_append_item(complete.clone())
        .await
        .unwrap_or_else(|error| panic!("同一 item_id 的 second row 必须可追加: {error:?}"));

    let rows = port.swarm_items_after("agent-one", 0, 10).await.unwrap();
    assert_eq!(rows, vec![delta, complete]);
    assert_eq!(
        rows.iter()
            .map(|row| (row.seq, row.item_id.as_str()))
            .collect::<Vec<_>>(),
        vec![(1, "itim-stream-1"), (2, "itim-stream-1")]
    );
    assert_eq!(port.swarm_last_seq("agent-one").await.unwrap(), 2);
}

#[tokio::test]
async fn input_receipts_are_insert_if_absent_and_return_facts() {
    let fixture = Fixture::current_shape().await;
    let port = port_with(fixture.transport.clone(), StoreAccess::ReadWrite);

    assert_eq!(
        port.swarm_put_input(input_row("sha256:first"))
            .await
            .unwrap(),
        SwarmInputPut::Inserted
    );
    assert_eq!(
        port.swarm_put_input(input_row("sha256:first"))
            .await
            .unwrap(),
        SwarmInputPut::Existing(input_row("sha256:first")),
        "同 inputId 的重放返回既有收据"
    );
    // 同 inputId 不同 fingerprint：端口只返回事实，冲突由上层判。
    assert_eq!(
        port.swarm_put_input(input_row("sha256:different"))
            .await
            .unwrap(),
        SwarmInputPut::Existing(input_row("sha256:first"))
    );
    assert_eq!(
        port.swarm_get_input("agent-one", "input-one")
            .await
            .unwrap(),
        Some(input_row("sha256:first"))
    );
    assert_eq!(
        port.swarm_get_input("agent-one", "missing").await.unwrap(),
        None
    );
}

#[tokio::test]
async fn bindings_are_scoped_by_logical_environment() {
    let fixture = Fixture::current_shape().await;
    let port = port_with(fixture.transport.clone(), StoreAccess::ReadWrite);
    let mut binding = SwarmBindingRow {
        logical_env_id: "env-one".to_owned(),
        logical_id: "agent-one".to_owned(),
        sandbox_id: "sandbox-one".to_owned(),
        workspace_path: "/work".to_owned(),
        sandbox_root: "/sandboxes".to_owned(),
        settings_profile: Some("default".to_owned()),
        token_file: Some("/run/token".to_owned()),
        last_run_id: Some("run-one".to_owned()),
        last_used_at: "2026-01-01T00:00:03Z".to_owned(),
    };
    port.swarm_put_binding(binding.clone()).await.unwrap();
    assert_eq!(
        port.swarm_get_binding("env-one").await.unwrap(),
        Some(binding.clone())
    );

    binding.sandbox_id = "sandbox-two".to_owned();
    binding.last_run_id = None;
    port.swarm_put_binding(binding.clone()).await.unwrap();
    assert_eq!(
        port.swarm_get_binding("env-one").await.unwrap(),
        Some(binding)
    );
    assert_eq!(port.swarm_get_binding("env-missing").await.unwrap(), None);
}

#[tokio::test]
async fn read_only_ports_read_but_refuse_writes_before_sending() {
    let fixture = Fixture::current_shape().await;
    let read_only = port_with(fixture.transport.clone(), StoreAccess::ReadOnly);
    assert_eq!(
        read_only.swarm_get_session("agent-one").await.unwrap(),
        None
    );
    assert_eq!(read_only.swarm_last_seq("agent-one").await.unwrap(), 0);

    let writes_before = fixture.transport.writes.load(Ordering::SeqCst);
    let error = read_only
        .swarm_upsert_session(session_row())
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::ReadOnlyStore
    ));
    assert_eq!(
        fixture.transport.writes.load(Ordering::SeqCst),
        writes_before,
        "只读拒绝必须发生在发请求之前"
    );
}

#[tokio::test]
async fn an_unknown_effect_is_reported_once_and_never_retried() {
    let fixture = Fixture::current_shape().await;
    // 包一层传输：批次照常提交，但把回复按「丢失」上报（服务端已生效、本机没收到）。
    let wrapper = Arc::new(LostReplyTransport {
        inner: fixture.transport.clone(),
        lose_next: AtomicBool::new(true),
    });
    let gate = Arc::new(ConnectionGate::default());
    let store = RemoteStore::new(
        wrapper.clone(),
        StoreAccess::ReadWrite,
        gate.mint(),
        gate.clone(),
    );
    let data = Arc::new(RemoteSessionData::with_connection_for_test(
        StoreId::mint(),
        store,
        Arc::new(NoReconnect),
        gate,
    ));
    let port: Arc<dyn SwarmStorePort> = Arc::new(RemoteSwarmStore::new(data));

    // 夹具自身的建库写入先记账，只核对本次调用发出多少个批。
    let writes_before = fixture.transport.writes.load(Ordering::SeqCst);
    let error = port.swarm_upsert_session(session_row()).await.unwrap_err();
    assert!(
        error.is_persistence_uncertain(),
        "丢响应只能报未决持久化，不能报成功或确定未生效: {error:?}"
    );
    assert_eq!(
        fixture.transport.writes.load(Ordering::SeqCst) - writes_before,
        1,
        "Unknown 不得重试写"
    );
    // 写入确实已提交（服务端事实），只是本机无法证明；后续访问不得把它当成未发生。
    let stored: (String,) = sqlx::query_as("SELECT root_session_id FROM swarm_sessions")
        .fetch_one(&fixture.transport.pool)
        .await
        .unwrap();
    assert_eq!(stored.0, "root-session-one");
}

/// 提交后丢回复的传输：其余行为原样委托给 SQLite 夹具。
struct LostReplyTransport {
    inner: Arc<SqliteTransport>,
    lose_next: AtomicBool,
}

#[async_trait]
impl RemoteTransport for LostReplyTransport {
    async fn sql_values(
        &self,
        spec: &StatementSpec,
    ) -> turso_serverless::Result<Vec<Vec<turso_serverless::Value>>> {
        self.inner.sql_values(spec).await
    }

    async fn managed_batch(
        &self,
        statements: Vec<StatementSpec>,
    ) -> turso_serverless::Result<Vec<u64>> {
        let counts = self.inner.managed_batch(statements).await?;
        if self.lose_next.swap(false, Ordering::SeqCst) {
            return Err(SdkError::Http("simulated lost swarm response".into()));
        }
        Ok(counts)
    }

    async fn consistent_read(
        &self,
        statements: Vec<StatementSpec>,
    ) -> turso_serverless::Result<Vec<Vec<Vec<turso_serverless::Value>>>> {
        self.inner.consistent_read(statements).await
    }

    fn is_autocommit(&self) -> turso_serverless::Result<bool> {
        self.inner.is_autocommit()
    }

    async fn close(&self) -> turso_serverless::Result<()> {
        self.inner.close().await
    }
}

// ─── 组合面：共享连接与关闭生命周期 ───────────────────────────────────────────

#[tokio::test]
async fn the_facade_exposes_a_port_over_the_same_connection() {
    let fixture = Fixture::current_shape().await;
    let gate = Arc::new(ConnectionGate::default());
    let store = RemoteStore::new(
        fixture.transport.clone(),
        StoreAccess::ReadWrite,
        gate.mint(),
        gate.clone(),
    );
    let data = Arc::new(RemoteSessionData::with_connection_for_test(
        StoreId::mint(),
        store,
        Arc::new(NoReconnect),
        gate,
    ));
    let swarm = Arc::new(RemoteSwarmStore::new(Arc::clone(&data)));
    let local: Arc<dyn LocalExecutionPort> =
        Arc::new(RemoteExecution::new(Arc::clone(&data), false));
    let data_port: Arc<dyn SessionDataPort> = data;
    let facade = Arc::new(
        SessionResourcesImpl::from_ports(data_port, local, SessionDataHome::RemoteStore)
            .with_swarm_store(swarm),
    );

    let port = facade.swarm_store();
    port.swarm_upsert_session(session_row()).await.unwrap();
    let stored: (String,) = sqlx::query_as("SELECT root_session_id FROM swarm_sessions")
        .fetch_one(&fixture.transport.pool)
        .await
        .unwrap();
    assert_eq!(stored.0, "root-session-one");

    // 关闭门面即关闭同一个连接：端口在关闭后明确失败，而不是另开一条库。
    facade.close().await.unwrap();
    let error = port.swarm_get_session("agent-one").await.unwrap_err();
    assert!(
        !matches!(error.kind(), SessionResourceErrorKind::Unsupported),
        "远端端口在关闭后不能退化成 unsupported: {error:?}"
    );
    assert!(
        format!("{error:?}").contains("closed"),
        "关闭后的失败必须能回答连接已关闭: {error:?}"
    );
}

// ─── schema v16：只追加的 v15 → v16 升级 ──────────────────────────────────────

#[test]
fn the_addition_plan_only_creates_swarm_objects_and_advances_the_version() {
    let snapshot = schema::StoreSnapshot {
        store_id: StoreId::mint(),
        schema_version: schema::REMOTE_SCHEMA_VERSION - 1,
        contract: schema::V15_STORE_CONTRACT.to_owned(),
    };
    // 前置事实：v15 是 REMOTE_SCHEMA_VERSION - 1（挡住常量各自漂移）。
    assert_eq!(snapshot.schema_version, 15);
    let plan = schema_upgrade::swarm_addition_plan(&snapshot).unwrap();
    let sql: Vec<&str> = plan.iter().map(|statement| statement.sql).collect();
    assert_eq!(sql[0], schema_upgrade::GUARD_META_SQL);
    assert_eq!(
        &sql[1..1 + crate::sessions::canonical::CREATE_SWARM_TABLES.len()],
        crate::sessions::canonical::CREATE_SWARM_TABLES
    );
    let index_at = 1 + crate::sessions::canonical::CREATE_SWARM_TABLES.len();
    assert_eq!(
        sql[index_at],
        crate::sessions::canonical::CREATE_SWARM_ITEMS_INDEX_SQL
    );
    assert_eq!(plan.len(), index_at + 2);
    assert!(sql[index_at + 1].starts_with("UPDATE peri_store_meta SET schema_version"));
    for statement in &sql[1..index_at + 1] {
        assert!(
            statement.starts_with("CREATE TABLE IF NOT EXISTS swarm_")
                || statement == &crate::sessions::canonical::CREATE_SWARM_ITEMS_INDEX_SQL,
            "追加批次只能建 Swarm 对象: {statement}"
        );
        assert!(!statement.contains("DROP"), "{statement}");
        assert!(!statement.contains("ALTER"), "{statement}");
    }

    // 别的来源一律拒绝：追加批次不服务 v14 或未知版本。
    let mut other = snapshot.clone();
    other.schema_version = 14;
    assert!(schema_upgrade::swarm_addition_plan(&other).is_err());
}

#[tokio::test]
async fn v15_upgrade_appends_swarm_objects_and_keeps_existing_rows() {
    let fixture = Fixture::previous_swarm().await;
    assert_eq!(fixture.version().await, 15);
    assert_eq!(fixture.contract().await, schema::V15_STORE_CONTRACT);
    assert_eq!(
        fixture.shape_verdict(schema::ShapeProbe::PreviousV15).await,
        StoreShapeRead::Consistent,
        "v15 夹具必须通过 v15 冻结声明"
    );

    let store = fixture.store(StoreAccess::ReadWrite);
    let read = store.read_identity().await.unwrap();
    let shape = probe_shape(&store, &read).await.unwrap();
    assert_eq!(shape, Some(StoreShapeRead::Consistent));
    assert!(matches!(
        super::session_data::open_step(read, shape, StoreAccess::ReadWrite).unwrap(),
        OpenStep::Upgrade(_)
    ));

    schema_upgrade::upgrade(&store, &fixture.snapshot)
        .await
        .unwrap();
    assert_eq!(fixture.version().await, schema::REMOTE_SCHEMA_VERSION);
    assert_eq!(fixture.contract().await, schema::STORE_CONTRACT);

    // 四个新表 + 一条索引。
    for table in crate::sessions::canonical::SWARM_TABLES {
        assert!(table_exists(&fixture, table).await, "{table}");
    }
    let (index_count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'swarm_items_by_id'",
    )
    .fetch_one(&fixture.transport.pool)
    .await
    .unwrap();
    assert_eq!(index_count, 1);
    // 既有行逐项保留；MQ 正文与凭证字节不因升级被改写。
    for table in [
        "machines",
        "projects",
        "workspaces",
        "threads",
        "messages",
        "session_bindings",
        "thread_mq",
        "session_close_intents",
        "mcp_oauth_credentials",
    ] {
        let query = format!("SELECT COUNT(*) FROM {table}");
        assert_eq!(fixture.count(query).await, 1, "{table}");
    }
    let (content,): (String,) = sqlx::query_as("SELECT content FROM messages")
        .fetch_one(&fixture.transport.pool)
        .await
        .unwrap();
    assert_eq!(content, "kept");
    let (pending,): (String,) = sqlx::query_as("SELECT payload_json FROM thread_mq")
        .fetch_one(&fixture.transport.pool)
        .await
        .unwrap();
    assert_eq!(pending, r#"{"text":"pending"}"#);
    let (credential,): (String,) =
        sqlx::query_as("SELECT credentials_blob FROM mcp_oauth_credentials")
            .fetch_one(&fixture.transport.pool)
            .await
            .unwrap();
    assert_eq!(credential, "credential bytes");

    // 升级后按当前形状直接打开（不再走 Upgrade），且新表为空（没有伪造 Swarm 事实）。
    let reopened = fixture.store(StoreAccess::ReadWrite);
    let read = reopened.read_identity().await.unwrap();
    let shape = probe_shape(&reopened, &read).await.unwrap();
    assert_eq!(shape, Some(StoreShapeRead::Consistent));
    assert!(matches!(
        super::session_data::open_step(read, shape, StoreAccess::ReadWrite).unwrap(),
        OpenStep::Existing(_)
    ));
    for table in crate::sessions::canonical::SWARM_TABLES {
        assert_eq!(
            fixture.count(format!("SELECT COUNT(*) FROM {table}")).await,
            0
        );
    }
}

async fn table_exists(fixture: &Fixture, table: &str) -> bool {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1")
            .bind(table)
            .fetch_all(&fixture.transport.pool)
            .await
            .unwrap();
    !rows.is_empty()
}
