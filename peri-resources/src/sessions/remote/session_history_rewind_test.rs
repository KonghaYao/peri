//! 远端 rewind 的完整性与并发守卫：边界行在「批次外读取」与「整批提交」之间被改动时，
//! 整批不生效并如实回报，而不是静默删掉一条本次操作从未看见的条目。清零侧同样钉住载荷
//! 语义：被清零的行整条 `projection` 一并清掉，`truncated` 与未清零行的 flags 都原样。
//!
//! 夹具与 [`super::session_history_page_tests`] 同形：一个真 SQLite 池扮演远端服务端，走真实
//! 批次与账本（传输面的并发注入点 `before_batch` 见 `schema_upgrade_test.rs`）。四个观察点：
//!
//! | 用例 | 断言的事实 |
//! | --- | --- |
//! | 未知边界 | 无变更 + [`RewindOutcome::BoundaryMissing`]（读与写都不动，清零也不发生） |
//! | 并发改动边界行 | 批内守卫让整批回滚：返回 `Conflict`，并发追加的行与保留前缀的 flags 都还在 |
//! | 只读存储 + 未知边界 | 先于边界读取拒绝写入（`ReadOnlyStore`），与本机 adapter 同一分类 |
//! | 清零投影载荷 | 清零行的 `projection` 列置 NULL（`truncated` 原样），未清零行逐位不变 |

use std::sync::Arc;

use peri_acp_types::messages::{BaseMessage, MessageId};
use peri_acp_types::projection::MessageProjectionDirective;
use peri_acp_types::session_resources::{
    FrozenSnapshotBytes, NewSession, NewSessionMeta, RewindBoundary, RewindOutcome,
    SessionResourceErrorKind, SessionResourceResult,
};
use peri_acp_types::store::{CompactionChange, MessageFlags, PersistedPayload};
use peri_acp_types::thread::{CancelPolicy, ThreadId};
use peri_acp_types::workspace::{ProjectId, ResolvedWorkspace, SessionBinding, WorkspaceId};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};

use super::generation::{ConnectionFactory, ConnectionGate};
use super::mutation::{RemoteStore, StoreAccess};
use super::schema::{self, StoreId};
use super::schema_upgrade_tests::SqliteTransport;
use super::session_data::RemoteSessionData;
use crate::sessions::data::SessionDataPort;

const REWIND_SESSION: &str = "remote-history-rewind";

const SELECT_ROWID_SQL: &str =
    "SELECT rowid FROM messages WHERE thread_id = ?1 AND message_id = ?2";
const COUNT_ROWS_SQL: &str = "SELECT COUNT(*) FROM messages WHERE thread_id = ?1";
const COUNT_MESSAGE_SQL: &str =
    "SELECT COUNT(*) FROM messages WHERE thread_id = ?1 AND message_id = ?2";
const EXCLUDED_OF_SQL: &str =
    "SELECT excluded FROM messages WHERE thread_id = ?1 AND message_id = ?2";
/// 行级 flags 三列：清零是列级事实，直接读列才能证明 `projection` 真的变成 NULL。
const FLAGS_OF_SQL: &str = "SELECT truncated, excluded, projection FROM messages
    WHERE thread_id = ?1 AND message_id = ?2";

/// 夹具消费历史行：按顺序给出可观察的消息。
fn messages(count: usize) -> Vec<PersistedPayload> {
    (0..count)
        .map(|index| PersistedPayload::Message(BaseMessage::human(format!("message {index}"))))
        .collect()
}

/// 远端只写 meta + 不可变绑定 + frozen：绑定事实在测试里手写，不需要工作区登记。
fn binding() -> SessionBinding {
    SessionBinding {
        schema_version: 1,
        revision: 1,
        project_id: ProjectId::new(),
        workspace_id: WorkspaceId::new(),
        cwd_relative_to_workspace: std::path::PathBuf::from(REWIND_SESSION),
    }
}

/// 与绑定同源的执行目录事实（新建会话时一并登记工作区，与门面的创建路径同形）。
fn workspace(binding: &SessionBinding) -> ResolvedWorkspace {
    ResolvedWorkspace {
        project_id: binding.project_id,
        workspace_id: binding.workspace_id,
        cwd: std::path::PathBuf::from("/tmp").join(REWIND_SESSION),
        root: std::path::PathBuf::from("/tmp"),
        relative_cwd: binding.cwd_relative_to_workspace.clone(),
        discovery_snapshot: Some(
            r#"{"root":"/tmp","common_dir":null,"private_dir":null}"#.to_owned(),
        ),
    }
}

/// 与生产同一条重建路径的工厂：换代时重建到访问意图相同的一条连接。
struct RewindFactory {
    transport: Arc<SqliteTransport>,
    access: StoreAccess,
    gate: Arc<ConnectionGate>,
}

#[async_trait::async_trait]
impl ConnectionFactory for RewindFactory {
    async fn connect(&self) -> SessionResourceResult<RemoteStore> {
        Ok(RemoteStore::new(
            self.transport.clone(),
            self.access,
            self.gate.mint(),
            self.gate.clone(),
        ))
    }
}

/// 夹具适配器：一个真 SQLite 池扮演远端服务端（当前契约的库，走真实批次与账本）。
fn rewind_data(transport: &Arc<SqliteTransport>, access: StoreAccess) -> RemoteSessionData {
    let gate = Arc::new(ConnectionGate::default());
    let store = RemoteStore::new(transport.clone(), access, gate.mint(), gate.clone());
    RemoteSessionData::with_connection_for_test(
        StoreId::mint(),
        store,
        Arc::new(RewindFactory {
            transport: transport.clone(),
            access,
            gate: gate.clone(),
        }),
        gate,
    )
}

/// 建库 + 一条新会话（历史为空时不发空批）。
async fn remote_with_history(
    history: &[PersistedPayload],
) -> (tempfile::TempDir, Arc<SqliteTransport>, RemoteSessionData) {
    let directory = tempfile::tempdir().unwrap();
    // 单连接足够：每个测试自己顺序使用这条夹具，不必与全量测试集的并行度抢连接。
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(directory.path().join("remote-history-rewind.db"))
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
    sqlx::query("INSERT INTO peri_store_meta VALUES (0, ?1, 'history-rewind-test', ?2, 'now')")
        .bind(schema::REMOTE_SCHEMA_VERSION)
        .bind(schema::STORE_CONTRACT)
        .execute(&pool)
        .await
        .unwrap();
    let transport = Arc::new(SqliteTransport::for_test(pool));
    let data = rewind_data(&transport, StoreAccess::ReadWrite);
    let id: ThreadId = REWIND_SESSION.to_owned();
    let binding = binding();
    data.save_new_session_in_workspace(
        &NewSession {
            thread_id: id.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            meta: NewSessionMeta {
                title: None,
                cwd: "/tmp/remote-history-rewind".to_owned(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: CancelPolicy::Cascade,
                snapshot_at_message_id: None,
            },
            binding: binding.clone(),
            frozen: FrozenSnapshotBytes::new("{}"),
        },
        &workspace(&binding),
    )
    .await
    .unwrap();
    if !history.is_empty() {
        data.append_history(&id, history).await.unwrap();
    }
    (directory, transport, data)
}

async fn rowid_of(pool: &SqlitePool, id: &ThreadId, message: MessageId) -> i64 {
    sqlx::query_scalar(SELECT_ROWID_SQL)
        .bind(id.as_str())
        .bind(message.as_uuid().to_string())
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn count_rows(pool: &SqlitePool, id: &ThreadId) -> i64 {
    sqlx::query_scalar(COUNT_ROWS_SQL)
        .bind(id.as_str())
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn count_message(pool: &SqlitePool, id: &ThreadId, message: MessageId) -> i64 {
    sqlx::query_scalar(COUNT_MESSAGE_SQL)
        .bind(id.as_str())
        .bind(message.as_uuid().to_string())
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn excluded_of(pool: &SqlitePool, id: &ThreadId, message: MessageId) -> i64 {
    sqlx::query_scalar(EXCLUDED_OF_SQL)
        .bind(id.as_str())
        .bind(message.as_uuid().to_string())
        .fetch_one(pool)
        .await
        .unwrap()
}

/// 行级 flags：`(truncated, excluded, projection JSON)`。载荷列必须是 `NULL`，而不是「解析
/// 不出来」——本机与远端读路径都会把不可解析的载荷判成损坏，所以清不清得看列值本身。
async fn row_flags(
    pool: &SqlitePool,
    id: &ThreadId,
    message: MessageId,
) -> (i64, i64, Option<String>) {
    sqlx::query_as(FLAGS_OF_SQL)
        .bind(id.as_str())
        .bind(message.as_uuid().to_string())
        .fetch_one(pool)
        .await
        .unwrap()
}

/// 未知边界：不删除、不清零、不报成功——本机与远端必须是同一个可观察结果。
#[tokio::test]
async fn remote_rewind_reports_a_missing_boundary_without_touching_history() {
    let history = messages(3);
    let (_fixture, transport, data) = remote_with_history(&history).await;
    let id: ThreadId = REWIND_SESSION.to_owned();
    let unknown = BaseMessage::human("never stored").id();
    // 保留前缀里的排除必须活过这次调用：清零集合与删除同批，批没生效就一条都不能落。
    data.apply_message_projections(
        &id,
        &[(
            history[0].id(),
            MessageFlags {
                excluded: true,
                ..Default::default()
            },
        )],
    )
    .await
    .unwrap();

    let outcome = data
        .rewind_history(&id, RewindBoundary::RemoveFrom(unknown), &[history[0].id()])
        .await
        .unwrap();

    assert_eq!(
        outcome,
        RewindOutcome::BoundaryMissing,
        "未知边界必须如实回报「本次没有生效」，不能以成功收场"
    );
    assert_eq!(count_rows(&transport.pool, &id).await, 3, "历史一行未动");
    assert_eq!(
        excluded_of(&transport.pool, &id, history[0].id()).await,
        1,
        "清零不得发生"
    );
}

/// F2 回归：边界 rowid 在批次之外读出，批次之前另一条连接删掉了边界行并追加了一条复用该
/// rowid 的条目。没有批内守卫时，`rowid >= 边界` 的删除会命中这条**本次操作从未看见**的新
/// 条目；有守卫时整批回滚，调用方拿到可区分的冲突（先重读再决定），而不是静默成功。
#[tokio::test]
async fn remote_rewind_aborts_when_the_boundary_row_is_replaced_mid_flight() {
    let history = messages(3);
    let (_fixture, transport, data) = remote_with_history(&history).await;
    let id: ThreadId = REWIND_SESSION.to_owned();
    let boundary = history[2].id();
    let retained = history[0].id();
    data.apply_message_projections(
        &id,
        &[(
            retained,
            MessageFlags {
                excluded: true,
                ..Default::default()
            },
        )],
    )
    .await
    .unwrap();
    let boundary_rowid = rowid_of(&transport.pool, &id, boundary).await;

    // 并发改动：删掉边界行（尾部行 → 它的 rowid 会被下一次插入复用），追加一条新条目。
    // 语句文本是测试夹具自己拼的，不含外部输入。
    let replacement = MessageId::new();
    let session = id.as_str().replace('\'', "''");
    let boundary_text = boundary.as_uuid().to_string().replace('\'', "''");
    let retained_text = retained.as_uuid().to_string().replace('\'', "''");
    let replacement_text = replacement.as_uuid().to_string().replace('\'', "''");
    *transport.before_batch.lock().unwrap() = Some(format!(
        "DELETE FROM messages WHERE thread_id = '{session}' AND message_id = '{boundary_text}';
         INSERT INTO messages (message_id, thread_id, role, content, truncated, excluded, projection)
         SELECT '{replacement_text}', thread_id, role, content, 0, 0, NULL FROM messages
         WHERE thread_id = '{session}' AND message_id = '{retained_text}';"
    ));

    let error = data
        .rewind_history(&id, RewindBoundary::RemoveFrom(boundary), &[retained])
        .await
        .unwrap_err();

    assert!(
        matches!(error.kind(), SessionResourceErrorKind::Conflict { .. }),
        "并发改动越过删除下界必须给出可区分的冲突: {error}"
    );
    assert_eq!(
        count_message(&transport.pool, &id, replacement).await,
        1,
        "整批回滚：并发追加的条目不得被这次 rewind 删掉"
    );
    assert_eq!(
        count_message(&transport.pool, &id, boundary).await,
        0,
        "边界行是并发删除的，不是本次 rewind 删的"
    );
    assert_eq!(
        excluded_of(&transport.pool, &id, retained).await,
        1,
        "整批回滚：清零不得单独落地"
    );
    assert_eq!(
        rowid_of(&transport.pool, &id, replacement).await,
        boundary_rowid,
        "复用 rowid 正是这条守卫要拦住的并发形态"
    );
}

/// 清零的远端语义：被清零的行**整条 `projection` 载荷一并清掉**（`superseded_by` 只在
/// `excluded` 为真时被读取，正文恢复后留着它就是「已生效的条目仍带着『我被某摘要取代』」的
/// 矛盾记录），`truncated` 原样保留；未被本次清零的行连载荷一起逐位不变。
///
/// 清载荷不丢有效渲染信息：生产里 `excluded: true` 的唯一写入点是 Full Compact
/// （`peri-agent/src/agent/compact_v2/full.rs`），写 flags 时用 `..Default::default()`
/// 整体覆盖，被排除的行本来就不带渲染指令。
#[tokio::test]
async fn remote_rewind_clears_the_whole_projection_payload_of_cleared_rows() {
    // 历史形状：`A B | S1 | P | E | S2 | C`（A、B 由第一次 Full 排除，E 由第二次排除，
    // P 是第二次之前追加的普通条目，C 是最后一次追加）。
    let covered = messages(2);
    let (_fixture, transport, data) = remote_with_history(&covered).await;
    let id: ThreadId = REWIND_SESSION.to_owned();

    let supersede = |summary: MessageId, truncated: bool| MessageFlags {
        truncated,
        excluded: true,
        projection: Some(MessageProjectionDirective {
            policy_version: 2,
            entries: Vec::new(),
            superseded_by: Some(summary),
        }),
    };
    let first_summary = BaseMessage::human("summary one");
    data.apply_compaction(
        &id,
        &CompactionChange {
            flag_updates: vec![
                (covered[0].id(), supersede(first_summary.id(), true)),
                (covered[1].id(), supersede(first_summary.id(), false)),
            ],
            appended_messages: vec![first_summary.clone()],
        },
    )
    .await
    .unwrap();

    let plain = messages(1);
    data.append_history(&id, &plain).await.unwrap();
    let between = messages(1);
    data.append_history(&id, &between).await.unwrap();
    let second_summary = BaseMessage::human("summary two");
    data.apply_compaction(
        &id,
        &CompactionChange {
            // E 同时带 `truncated`：它来自 Micro compact，与排除无关，清零必须原样保留。
            flag_updates: vec![(between[0].id(), supersede(second_summary.id(), true))],
            appended_messages: vec![second_summary.clone()],
        },
    )
    .await
    .unwrap();

    let before_covered = row_flags(&transport.pool, &id, covered[0].id()).await;
    let before_between = row_flags(&transport.pool, &id, between[0].id()).await;
    let before_plain = row_flags(&transport.pool, &id, plain[0].id()).await;
    let before_flags = data.load_snapshot(&id).await.unwrap().flags;
    // 夹具自证：清零前 A 的取代记录、E 的两项标记都真的写进去了，否则下面的「未被清零」与
    // 「保留 truncated」断言会空转。
    assert_eq!(
        before_flags
            .get(&covered[0].id())
            .cloned()
            .unwrap_or_default()
            .superseded_by(),
        Some(first_summary.id())
    );
    assert_eq!((before_between.0, before_between.1), (1, 1));
    assert!(before_between.2.is_some());

    // 边界落在 E 上：S2 被删 ⇒ E 的排除失去依据（清零集合恰好是 `[E]`）；A、B 的 S1 仍在保留
    // 前缀里，排除依旧有依据，P 本来就没有标记。
    let outcome = data
        .rewind_history(
            &id,
            RewindBoundary::KeepThrough(between[0].id()),
            &[between[0].id()],
        )
        .await
        .unwrap();
    assert_eq!(outcome, RewindOutcome::Applied);
    assert_eq!(
        count_rows(&transport.pool, &id).await,
        5,
        "S2 与它之后的条目被删掉"
    );

    // ① 被清零的行：`excluded` 与整条载荷一起消失，`truncated` 位原样。
    assert_eq!(
        row_flags(&transport.pool, &id, between[0].id()).await,
        (before_between.0, 0, None),
        "清零必须把 projection 列置 NULL，且不动 truncated"
    );
    let cleared = data
        .load_snapshot(&id)
        .await
        .unwrap()
        .flags
        .get(&between[0].id())
        .cloned()
        .unwrap_or_default();
    assert!(!cleared.excluded);
    assert!(cleared.projection.is_none(), "读回来的 flags 不得再有载荷");
    assert!(
        cleared.superseded_by().is_none(),
        "正文已恢复的远端条目不得残留取代记录"
    );
    assert!(
        cleared.truncated,
        "truncated 位不是排除的产物，清零不得动它"
    );

    // ② 未清零的行：仍被存活摘要覆盖的排除、以及从未被排除的条目都逐位不变。
    assert_eq!(
        row_flags(&transport.pool, &id, covered[0].id()).await,
        before_covered,
        "仍被 S1 覆盖的条目不因这次清零改动"
    );
    assert_eq!(
        row_flags(&transport.pool, &id, plain[0].id()).await,
        before_plain
    );
    let mut expected = before_flags.clone();
    // E 只剩它本来就有的 `truncated` 位：排除与整条载荷一起清零，其余条目连键都不动。
    expected.insert(
        between[0].id(),
        MessageFlags {
            truncated: true,
            ..Default::default()
        },
    );
    assert_eq!(
        data.load_snapshot(&id).await.unwrap().flags,
        expected,
        "只有本次清零的条目改变，未触及的条目连载荷一起原样保留"
    );
}

/// F3 回归：只读存储 + 未知边界是**写请求被拒**，不是「无变更的成功」。
///
/// 本机 adapter 的同类路径先 `writable()` 再读边界（`sqlite_store/session_history_test.rs`
/// 的只读用例断言同一个分类），远端必须同序，否则只读打开时一次写请求会以成功收场。
#[tokio::test]
async fn remote_read_only_store_refuses_rewind_with_an_unknown_boundary() {
    let history = messages(2);
    let (_fixture, transport, _writer) = remote_with_history(&history).await;
    let reader = rewind_data(&transport, StoreAccess::ReadOnly);
    let id: ThreadId = REWIND_SESSION.to_owned();
    let unknown = BaseMessage::human("never stored").id();

    let error = reader
        .rewind_history(&id, RewindBoundary::RemoveFrom(unknown), &[])
        .await
        .unwrap_err();

    assert!(
        matches!(error.kind(), SessionResourceErrorKind::ReadOnlyStore),
        "只读 + 未知边界必须失败，而不是提前返回成功: {error}"
    );
    assert_eq!(
        count_rows(&transport.pool, &id).await,
        2,
        "只读路径什么都不写"
    );
}
