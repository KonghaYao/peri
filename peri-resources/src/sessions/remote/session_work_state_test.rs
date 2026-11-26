//! 远端工作快照读取面：当前形状写入后经远端读取读回，旧代只读库读作默认值且不写。
//!
//! 夹具让隔离的 PostgreSQL 测试库（与其他远端用例共用 [`PgTestTransport`]），
//! 因此走的是真实语句、真实托管批与真实账本；只读库不缩小到「只断言 SQL 文本」。

use std::sync::Arc;

use peri_acp_types::session_resources::SessionResourceErrorKind;
use peri_acp_types::thread::{AgentStatus, ThreadId, WorkState};
use sqlx::PgPool;

use super::generation::{ConnectionFactory, ConnectionGate};
use super::mutation::{RemoteStore, StoreAccess};
use super::postgres_fixture::PgTestTransport;
use super::schema::{self, StoreId};
use super::session_data::RemoteSessionData;
use crate::sessions::data::{ChildResumeRecord, SessionDataPort};

const SESSION: &str = "work-state-session";
const CHILD: &str = "work-state-child";

struct Fixture {
    pool: PgPool,
    data: RemoteSessionData,
}

struct WorkStateFactory {
    transport: Arc<PgTestTransport>,
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

async fn current_fixture(access: StoreAccess) -> Fixture {
    let pool = crate::sessions::remote::postgres_fixture::test_pool().await;
    for statement in super::session_schema::initialization_plan() {
        sqlx::query(statement.sql).execute(&pool).await.unwrap();
    }
    sqlx::raw_sql("INSERT INTO machines VALUES ('host', 'host', 'known');
         INSERT INTO workspaces(id, machine_id, path, path_source) VALUES ('ws','host','/tmp','unverified')")
        .execute(&pool).await.unwrap();
    sqlx::query(schema::CREATE_STORE_META_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(super::ledger::CREATE_OP_LEDGER_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO peri_store_meta VALUES (0, $1, 'work-state-test', $2, 'now')")
        .bind(schema::REMOTE_SCHEMA_VERSION)
        .bind(schema::STORE_CONTRACT)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO threads (id, cwd, created_at, updated_at, workspace_id)
         VALUES ($1, '/tmp', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'ws')",
    )
    .bind(SESSION)
    .execute(&pool)
    .await
    .unwrap();

    let transport = Arc::new(PgTestTransport::for_test(pool.clone()));
    let gate = Arc::new(ConnectionGate::default());
    let store = RemoteStore::new(transport.clone(), access, gate.mint(), gate.clone());
    let data = RemoteSessionData::with_connection_for_test(
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
    Fixture { pool, data }
}

/// 当前形状：写入 `Working` + 占用，读回一致；改成 `Stopped` + NULL 后再读回一致。
#[tokio::test]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
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
        sqlx::query_as("SELECT work_state, context_usage_tokens FROM threads WHERE id = $1")
            .bind(SESSION)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(state, WorkState::Stopped.as_str());
    assert_eq!(tokens, None);
}

// ─── 认领收尾：状态与工作意图同批 ──────────────────────────────────────────────

/// 子会话行：认领中（`active` + `working`），供收尾批次改写。
async fn seed_claimed_child(fixture: &Fixture) {
    sqlx::query(
        "INSERT INTO threads
            (id, cwd, created_at, updated_at, workspace_id, agent_status, work_state)
         VALUES ($1, '/tmp', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'ws', 'active', 'working')",
    )
    .bind(CHILD)
    .execute(&fixture.pool)
    .await
    .unwrap();
}

/// 直接读列值：断言的是落库事实，不是解码端的回填。
async fn child_facts(fixture: &Fixture) -> (String, String) {
    sqlx::query_as("SELECT agent_status, work_state FROM threads WHERE id = $1")
        .bind(CHILD)
        .fetch_one(&fixture.pool)
        .await
        .unwrap()
}

fn previous() -> ChildResumeRecord {
    ChildResumeRecord {
        status: AgentStatus::Cancelled,
        claimed: false,
    }
}

/// 成功路径：终态与工作意图在同一次提交里写入。
#[tokio::test]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn settle_child_resume_writes_terminal_status_and_work_state_together() {
    let fixture = current_fixture(StoreAccess::ReadWrite).await;
    seed_claimed_child(&fixture).await;
    let child: ThreadId = CHILD.to_owned();

    fixture
        .data
        .settle_child_resume(
            &child,
            &previous(),
            Some(WorkState::Stopped),
            Some(AgentStatus::Done),
        )
        .await
        .unwrap();

    assert_eq!(
        child_facts(&fixture).await,
        ("done".to_owned(), "stopped".to_owned())
    );
    assert_eq!(
        fixture.data.load_child_resume_record(&child).await.unwrap(),
        ChildResumeRecord {
            status: AgentStatus::Done,
            claimed: false,
        }
    );
}

/// 工作意图语句失败时终态也必须一起回滚：不能留下状态已改、意图未改的部分写入。
#[tokio::test]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn settle_child_resume_rolls_back_status_when_work_state_statement_fails() {
    let fixture = current_fixture(StoreAccess::ReadWrite).await;
    seed_claimed_child(&fixture).await;
    sqlx::raw_sql(
        "CREATE TRIGGER reject_work_state_settle BEFORE UPDATE OF work_state ON threads
         FOR EACH ROW WHEN (NEW.work_state = 'stopped') EXECUTE FUNCTION pg_temp.reject_fixture_write()",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();

    let error = fixture
        .data
        .settle_child_resume(
            &CHILD.to_owned(),
            &previous(),
            Some(WorkState::Stopped),
            Some(AgentStatus::Done),
        )
        .await
        .unwrap_err();
    // 触发器中止的 SDK 分类随驱动而定：这里只要求「确定未生效」，回滚由列值断言覆盖。
    assert!(
        matches!(
            error.kind(),
            SessionResourceErrorKind::Unavailable { .. }
                | SessionResourceErrorKind::InvalidInput { .. }
        ),
        "{error:?}"
    );
    assert_eq!(
        child_facts(&fixture).await,
        ("active".to_owned(), "working".to_owned()),
        "工作意图语句失败不得让状态列单独提交"
    );
}

/// `work_state = None` 时不发出该列写入：任何改写工作意图的语句都会撞触发器。
#[tokio::test]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn settle_child_resume_without_work_state_leaves_the_column_untouched() {
    let fixture = current_fixture(StoreAccess::ReadWrite).await;
    seed_claimed_child(&fixture).await;
    sqlx::raw_sql(
        "CREATE TRIGGER reject_work_state_write BEFORE UPDATE OF work_state ON threads
         FOR EACH ROW EXECUTE FUNCTION pg_temp.reject_fixture_write()",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();

    fixture
        .data
        .settle_child_resume(
            &CHILD.to_owned(),
            &previous(),
            None,
            Some(AgentStatus::Done),
        )
        .await
        .unwrap();

    assert_eq!(
        child_facts(&fixture).await,
        ("done".to_owned(), "working".to_owned()),
        "未给出工作意图时该列必须保持原值"
    );
}

/// `terminal = None` 时恢复认领前状态与工作意图。
#[tokio::test]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn settle_child_resume_without_terminal_restores_previous_status_and_work_state() {
    let fixture = current_fixture(StoreAccess::ReadWrite).await;
    seed_claimed_child(&fixture).await;
    let child: ThreadId = CHILD.to_owned();

    fixture
        .data
        .settle_child_resume(&child, &previous(), Some(WorkState::Stopped), None)
        .await
        .unwrap();

    assert_eq!(
        child_facts(&fixture).await,
        ("cancelled".to_owned(), "stopped".to_owned())
    );
    assert_eq!(
        fixture.data.load_child_resume_record(&child).await.unwrap(),
        previous()
    );
}
