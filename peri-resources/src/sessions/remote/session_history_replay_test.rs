//! 远端 canonical 完整回放与 frozen 读取：同一个 `SqliteTransport` 夹具扮演远端服务端，
//! 走真实批次与账本。
//!
//! 覆盖 [`SessionDataPort::load_history_segment`] 的段内顺序、固定上界与失效分类，以及
//! frozen 快照的原始字节读取；与本机 adapter 的同一夹具比对。

use std::sync::Arc;

use peri_acp_types::history_page::{HistoryCursorError, HistorySegment};
use peri_acp_types::messages::{BaseMessage, MessageId};
use peri_acp_types::session_resources::{
    FrozenSnapshotBytes, FrozenState, NewSession, NewSessionMeta, SessionResourceErrorKind,
    SessionResourceResult,
};
use peri_acp_types::store::{MessageFlags, PersistedPayload};
use peri_acp_types::thread::{CancelPolicy, ThreadId};
use peri_acp_types::workspace::{ProjectId, ResolvedWorkspace, SessionBinding, WorkspaceId};
use sqlx::sqlite::SqlitePoolOptions;

use super::generation::{ConnectionFactory, ConnectionGate};
use super::mutation::{RemoteStore, StoreAccess};
use super::schema::{self, StoreId};
use super::schema_upgrade_tests::SqliteTransport;
use super::session_data::RemoteSessionData;
use crate::sessions::data::SessionDataPort;

/// 分页读取的字节预算在测试里的默认值：足够大，只验证顺序规则。
const NO_BUDGET_LIMIT: usize = usize::MAX;

const PAGE_SESSION: &str = "remote-history-page";

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
        cwd_relative_to_workspace: std::path::PathBuf::from("remote-history-page"),
    }
}

/// 与绑定同源的执行目录事实（新建会话时一并登记工作区，与门面的创建路径同形）。
fn workspace(binding: &SessionBinding) -> ResolvedWorkspace {
    ResolvedWorkspace {
        project_id: binding.project_id,
        workspace_id: binding.workspace_id,
        cwd: std::path::PathBuf::from("/tmp/remote-history-page"),
        root: std::path::PathBuf::from("/tmp"),
        relative_cwd: binding.cwd_relative_to_workspace.clone(),
        discovery_snapshot: Some(
            r#"{"root":"/tmp","common_dir":null,"private_dir":null}"#.to_owned(),
        ),
    }
}

/// 与生产同一条重建路径的工厂：连接换代时重新建到同一份夹具的连接。
struct PageFactory {
    transport: Arc<SqliteTransport>,
    gate: Arc<ConnectionGate>,
}

#[async_trait::async_trait]
impl ConnectionFactory for PageFactory {
    async fn connect(&self) -> SessionResourceResult<RemoteStore> {
        Ok(RemoteStore::new(
            self.transport.clone(),
            StoreAccess::ReadWrite,
            self.gate.mint(),
            self.gate.clone(),
        ))
    }
}

/// 夹具适配器：一个真 SQLite 池扮演远端服务端（当前契约的库，走真实批次与账本）。
fn remote_data(transport: &Arc<SqliteTransport>) -> RemoteSessionData {
    let gate = Arc::new(ConnectionGate::default());
    let store = RemoteStore::new(
        transport.clone(),
        StoreAccess::ReadWrite,
        gate.mint(),
        gate.clone(),
    );
    RemoteSessionData::with_connection_for_test(
        StoreId::mint(),
        store,
        Arc::new(PageFactory {
            transport: transport.clone(),
            gate: gate.clone(),
        }),
        gate,
    )
}

/// 建库 + 一条新会话（历史为空时不发空批）。返回临时目录，调用方须持有它。
async fn remote_with_history(
    history: &[PersistedPayload],
) -> (tempfile::TempDir, RemoteSessionData) {
    remote_fixture(Some("{}"), history).await
}

/// 同一条建库 + 建会话路径，但允许指定 `threads.frozen_context` 的原始内容。
///
/// `None` 表示 SQL NULL：创建 API 总是写非空值，这里在建会话后用 SQL 改成 NULL，
/// 模拟尚未持久化快照的 legacy 会话。
async fn remote_fixture(
    frozen: Option<&str>,
    history: &[PersistedPayload],
) -> (tempfile::TempDir, RemoteSessionData) {
    let directory = tempfile::tempdir().unwrap();
    // 单连接足够：每个测试自己顺序使用这条夹具，不必与全量测试集的并行度抢连接。
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(directory.path().join("remote-history-page.db"))
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
    sqlx::query("INSERT INTO peri_store_meta VALUES (0, ?1, 'history-page-test', ?2, 'now')")
        .bind(schema::REMOTE_SCHEMA_VERSION)
        .bind(schema::STORE_CONTRACT)
        .execute(&pool)
        .await
        .unwrap();
    // legacy 夹具在建会话后要直改列：先留一条连接，传输面仍拿到同一个池。
    let fixture_pool = pool.clone();
    let data = remote_data(&Arc::new(SqliteTransport::for_test(pool)));
    let id: ThreadId = PAGE_SESSION.to_owned();
    let binding = binding();
    data.save_new_session_in_workspace(
        &NewSession {
            thread_id: id.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            meta: NewSessionMeta {
                title: None,
                cwd: "/tmp/remote-history-page".to_owned(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: CancelPolicy::Cascade,
                snapshot_at_message_id: None,
            },
            binding: binding.clone(),
            frozen: FrozenSnapshotBytes::new(frozen.unwrap_or("{}")),
        },
        &workspace(&binding),
    )
    .await
    .unwrap();
    if frozen.is_none() {
        sqlx::query("UPDATE threads SET frozen_context = NULL WHERE id = ?1")
            .bind(PAGE_SESSION)
            .execute(&fixture_pool)
            .await
            .unwrap();
    }
    if !history.is_empty() {
        data.append_history(&id, history).await.unwrap();
    }
    (directory, data)
}

// ─── canonical 完整回放（向前段） ──────────────────────────────────────────────

fn segment_ids(segment: &HistorySegment) -> Vec<MessageId> {
    segment
        .payloads
        .iter()
        .map(|payload| payload.id())
        .collect()
}

/// 向前段的多段推进：首段固定上界，续段透传它；回放期间追加的行不进入本次遍历。
///
/// 与本机 adapter 的段语义逐条同形（同一份 [`peri_acp_types::history_page::HistorySegmentPlan`]），
/// 差别只在执行器：远端把上界、候选行（首段还多一列继承区）放进同一个只读批次。
#[tokio::test]
async fn remote_history_segment_replay_matches_the_local_backend() {
    let history = messages(7);
    let (_fixture, data) = remote_with_history(&history).await;
    let id: ThreadId = PAGE_SESSION.to_owned();

    let first = data
        .load_history_segment(&id, None, 2, NO_BUDGET_LIMIT)
        .await
        .unwrap();
    assert_eq!(
        segment_ids(&first),
        vec![history[0].id(), history[1].id()],
        "首段从最旧一条开始，段内是历史正序"
    );
    let cursor = first.next_cursor.clone().expect("更晚方向还有内容");
    assert_eq!(cursor.after, "2", "游标停在最后一条已推进的行上");
    assert_eq!(cursor.tail, "7", "上界 = 首段那一刻的最大存储序键");

    let second = data
        .load_history_segment(&id, Some(&cursor), 2, NO_BUDGET_LIMIT)
        .await
        .unwrap();
    assert_eq!(
        segment_ids(&second),
        vec![history[2].id(), history[3].id()],
        "续读无缺口、无重复（与本机 adapter 同一段）"
    );
    let cursor = second.next_cursor.clone().unwrap();

    // 上界在首段就固定了：回放期间追加的行不属于本次遍历。
    let appended = messages(1);
    data.append_history(&id, &appended).await.unwrap();
    let third = data
        .load_history_segment(&id, Some(&cursor), 2, NO_BUDGET_LIMIT)
        .await
        .unwrap();
    assert_eq!(segment_ids(&third), vec![history[4].id(), history[5].id()]);
    let last = data
        .load_history_segment(&id, Some(&third.next_cursor.unwrap()), 2, NO_BUDGET_LIMIT)
        .await
        .unwrap();
    assert_eq!(segment_ids(&last), vec![history[6].id()]);
    assert!(
        last.next_cursor.is_none(),
        "读到固定上界即收尾（追加的那条不在其中）"
    );
}

/// 完整回放**不过滤**条目：已排除的归档原文与 System 条目都在段里（与本机 adapter 同一口径）。
#[tokio::test]
async fn remote_history_segment_returns_archived_and_system_rows() {
    let mut history = messages(2);
    history.insert(
        1,
        PersistedPayload::Message(BaseMessage::system("tool notice")),
    );
    let (_fixture, data) = remote_with_history(&history).await;
    let id: ThreadId = PAGE_SESSION.to_owned();

    data.apply_message_projections(
        &id,
        &[(
            history[0].id(),
            MessageFlags {
                truncated: false,
                excluded: true,
                projection: None,
            },
        )],
    )
    .await
    .unwrap();

    let segment = data
        .load_history_segment(&id, None, 10, NO_BUDGET_LIMIT)
        .await
        .unwrap();
    assert_eq!(
        segment_ids(&segment),
        history.iter().map(PersistedPayload::id).collect::<Vec<_>>(),
        "完整历史连归档原文与 System 条目一起给出"
    );
    assert!(segment.next_cursor.is_none());
}

/// 续段锚点消失即明确失效：不回退成首段，也不用空段冒充回放结束。
#[tokio::test]
async fn remote_history_segment_fails_closed_when_the_after_row_is_gone() {
    let history = messages(4);
    let (_fixture, data) = remote_with_history(&history).await;
    let id: ThreadId = PAGE_SESSION.to_owned();

    let first = data
        .load_history_segment(&id, None, 2, NO_BUDGET_LIMIT)
        .await
        .unwrap();
    let cursor = first.next_cursor.clone().unwrap();
    data.remove_history_entries(&id, &[history[1].id()])
        .await
        .unwrap();
    let error = data
        .load_history_segment(&id, Some(&cursor), 2, NO_BUDGET_LIMIT)
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Cursor(HistoryCursorError::Expired { .. })
    ));

    // 会话不存在同样不回答一个空段。
    let error = data
        .load_history_segment(&"missing-session".to_owned(), None, 2, NO_BUDGET_LIMIT)
        .await
        .unwrap_err();
    assert!(matches!(error.kind(), SessionResourceErrorKind::NotFound));
}

// ─── frozen 三态 ───────────────────────────────────────────────────────────────

/// Present：存储层原样返回，不解析、不改写、不因版本未知丢弃字节。
#[tokio::test]
async fn remote_frozen_state_returns_present_bytes_verbatim() {
    let frozen = r#"{"v":1,"x":"原文"}"#;
    let (_fixture, data) = remote_fixture(Some(frozen), &[]).await;
    let id: ThreadId = PAGE_SESSION.to_owned();

    match data.load_frozen_state(&id).await.unwrap() {
        FrozenState::Present(bytes) => {
            assert_eq!(bytes.as_str(), frozen, "快照字节必须逐字节一致");
        }
        other => panic!("expected Present, got {other:?}"),
    }
}

/// LegacyAbsent：`frozen_context IS NULL` 按实现语义是 legacy 缺失，不是 NotFound。
#[tokio::test]
async fn remote_frozen_state_reports_sql_null_as_legacy_absent() {
    let (_fixture, data) = remote_fixture(None, &[]).await;
    let id: ThreadId = PAGE_SESSION.to_owned();

    assert_eq!(
        data.load_frozen_state(&id).await.unwrap(),
        FrozenState::LegacyAbsent
    );
}

/// NotFound：没有该线程行时明确失败，不把缺失误报成 LegacyAbsent。
#[tokio::test]
async fn remote_frozen_state_rejects_unknown_thread() {
    let (_fixture, data) = remote_fixture(Some("{}"), &[]).await;

    let error = data
        .load_frozen_state(&"missing-session".to_owned())
        .await
        .unwrap_err();
    assert!(matches!(error.kind(), SessionResourceErrorKind::NotFound));
}

#[path = "session_archive_test.rs"]
mod archive_tests;
