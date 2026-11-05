//! 远端工作快照读取面：当前形状写入后经远端读取读回，旧代只读库读作默认值且不写。
//!
//! 夹具让一个真 SQLite 池扮演远端服务端（与其他远端用例共用 [`SqliteTransport`]），
//! 因此走的是真实语句、真实托管批与真实账本；只读库不缩小到「只断言 SQL 文本」。

use std::sync::atomic::Ordering;
use std::sync::Arc;

use peri_acp_types::thread::{ThreadId, WorkState};
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

use super::generation::{ConnectionFactory, ConnectionGate};
use super::mutation::{RemoteStore, StoreAccess};
use super::schema::{self, StoreId};
use super::schema_upgrade_tests::SqliteTransport;
use super::session_data::RemoteSessionData;
use crate::sessions::data::SessionDataPort;

const SESSION: &str = "work-state-session";

struct Fixture {
    _directory: tempfile::TempDir,
    pool: SqlitePool,
    transport: Arc<SqliteTransport>,
    data: RemoteSessionData,
}

struct WorkStateFactory {
    transport: Arc<SqliteTransport>,
    access: StoreAccess,
    gate: Arc<ConnectionGate>,
}

#[async_trait::async_trait]
impl ConnectionFactory for WorkStateFactory {
    async fn connect(
        &self,
    ) -> peri_acp_types::session_resources::SessionResourceResult<RemoteStore> {
        Ok(RemoteStore::new(
            self.transport.clone(),
            self.access,
            self.gate.mint(),
            self.gate.clone(),
        ))
    }
}

/// 当前形状的远端库（v14 + `v8` 契约）与一条会话行。
async fn current_fixture(access: StoreAccess) -> Fixture {
    build_fixture(
        schema::REMOTE_SCHEMA_VERSION,
        schema::STORE_CONTRACT,
        access,
        false,
    )
    .await
}

/// 上一代只读库（schema 13 + `v7` 契约）：建出当前 DDL 后去掉两列工作快照。
async fn previous_generation_fixture() -> Fixture {
    let fixture = build_fixture(
        13,
        schema::PREVIOUS_CURRENT_STORE_CONTRACT,
        StoreAccess::ReadOnly,
        true,
    )
    .await;
    sqlx::raw_sql(
        "ALTER TABLE threads DROP COLUMN context_usage_tokens;
         ALTER TABLE threads DROP COLUMN work_state;",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    fixture
}

async fn build_fixture(
    version: i64,
    contract: &str,
    access: StoreAccess,
    legacy_shape: bool,
) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(directory.path().join("remote-work-state.db"))
                .create_if_missing(true)
                .foreign_keys(false),
        )
        .await
        .unwrap();
    for statement in super::session_schema::initialization_plan() {
        sqlx::query(statement.sql).execute(&pool).await.unwrap();
    }
    sqlx::query(schema::CREATE_STORE_META_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(super::ledger::CREATE_OP_LEDGER_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO peri_store_meta VALUES (0, ?1, 'work-state-test', ?2, 'now')")
        .bind(version)
        .bind(contract)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO threads (id, cwd, created_at, updated_at, workspace_id)
         VALUES (?1, '/tmp', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'ws')",
    )
    .bind(SESSION)
    .execute(&pool)
    .await
    .unwrap();

    let transport = Arc::new(SqliteTransport::for_test(pool.clone()));
    let gate = Arc::new(ConnectionGate::default());
    let store = RemoteStore::new(transport.clone(), access, gate.mint(), gate.clone());
    let mut data = RemoteSessionData::with_connection_for_test(
        StoreId::mint(),
        store,
        Arc::new(WorkStateFactory {
            transport: transport.clone(),
            access,
            gate: gate.clone(),
        }),
        gate,
    );
    // 生产路径由打开时的身份/形状探测给出这一位；夹具直接用同一判定结果。
    data.legacy_shape = legacy_shape;
    Fixture {
        _directory: directory,
        pool,
        transport,
        data,
    }
}

/// 当前形状：写入 `Working` + 占用，读回一致；改成 `Stopped` + NULL 后再读回一致。
#[tokio::test]
async fn current_store_round_trips_work_state_and_context_usage() {
    let fixture = current_fixture(StoreAccess::ReadWrite).await;
    let id: ThreadId = SESSION.to_owned();

    fixture
        .data
        .set_work_state(&id, WorkState::Working)
        .await
        .unwrap();
    fixture
        .data
        .set_context_usage(&id, Some(4096))
        .await
        .unwrap();
    let meta = fixture.data.load_meta(&id).await.unwrap();
    assert_eq!(meta.work_state, WorkState::Working);
    assert_eq!(meta.context_usage_tokens, Some(4096));

    fixture
        .data
        .set_work_state(&id, WorkState::Stopped)
        .await
        .unwrap();
    fixture.data.set_context_usage(&id, None).await.unwrap();
    let meta = fixture.data.load_meta(&id).await.unwrap();
    assert_eq!(meta.work_state, WorkState::Stopped);
    assert_eq!(meta.context_usage_tokens, None);

    // 读回的不是解码端回填：列值本身就是写入的事实。
    let (state, tokens): (String, Option<i64>) =
        sqlx::query_as("SELECT work_state, context_usage_tokens FROM threads WHERE id = ?1")
            .bind(SESSION)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(state, WorkState::Stopped.as_str());
    assert_eq!(tokens, None);
}

/// 上一代只读库：两列不存在，经同形占位读作 `Idle` / `None`，且不发任何写入批次。
#[tokio::test]
async fn previous_generation_store_reads_idle_and_unknown_usage_without_writing() {
    let fixture = previous_generation_fixture().await;
    let id: ThreadId = SESSION.to_owned();

    let meta = fixture.data.load_meta(&id).await.unwrap();
    assert_eq!(meta.work_state, WorkState::Idle);
    assert_eq!(meta.context_usage_tokens, None);
    // 完整事实行（含绑定、frozen、继承区）也走同一份旧代投影。
    let snapshot = fixture.data.load_snapshot(&id).await.unwrap();
    assert_eq!(snapshot.meta.work_state, WorkState::Idle);
    assert_eq!(snapshot.meta.context_usage_tokens, None);

    assert_eq!(
        fixture.transport.writes.load(Ordering::SeqCst),
        0,
        "只读库的读取不得发写入批次"
    );
    // 只读访问模式在请求发出之前就拒绝写入。
    assert!(fixture
        .data
        .set_work_state(&id, WorkState::Working)
        .await
        .is_err());
    assert_eq!(
        fixture.transport.writes.load(Ordering::SeqCst),
        0,
        "被拒绝的写入同样不产生批次"
    );
}
