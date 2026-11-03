//! Thread MQ 的本机行为：真实 SQLite 文件上的入队、投影、取消、原子移交、清理与迁移。
//!
//! 断言以**可观察事实**为准：重开数据库后的行、历史表与 MQ 表的实际内容、以及两种竞争结果
//! 的互斥。夹具用真实临时文件而不是 `:memory:`——「提交前后重开」只有落盘才可验证。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use peri_acp_types::messages::{BaseMessage, MessageId};
use peri_acp_types::session::{
    MessageKind, MessagePolicy, MessageSource, MqCancelOutcome, MqEnqueueDisposition, MqFilter,
    MqInput, MqReceiveDisposition, MqReceiveItem, MqReceivePurpose, MqReceiveSelection,
    MqReceiveStatus,
};
use peri_acp_types::session_resources::{RewindBoundary, SessionResourceErrorKind};
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::thread::{ThreadId, ThreadMeta};

use super::*;
use crate::sessions::canonical;
use crate::sessions::data::SessionDataPort;

struct Fixture {
    directory: tempfile::TempDir,
    store: SqliteThreadStore,
    data: SqliteSessionData,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let store = SqliteThreadStore::new(directory.path().join("threads.db"))
            .await
            .unwrap();
        let data = SqliteSessionData::new(Arc::clone(&store.database));
        Self {
            directory,
            store,
            data,
        }
    }

    fn path(&self) -> PathBuf {
        self.directory.path().join("threads.db")
    }

    async fn thread(&self, name: &str) -> ThreadId {
        self.store
            .create_thread(ThreadMeta::new_at(
                self.directory.path().to_string_lossy().into_owned(),
                peri_time::now_wall(),
            ))
            .await
            .unwrap_or_else(|error| panic!("{name}: {error}"))
    }
}

/// 一条用户输入：payload 携带**同一个**稳定 ID（MQ 与最终历史共用身份）。
fn input(id: MessageId, text: &str) -> MqInput {
    MqInput {
        id,
        kind: MessageKind::Prompt,
        source: MessageSource::UserInput,
        payload: PersistedPayload::Message(human(id, text)),
        policy: MessagePolicy::ensure_processing(),
        original_draft: Some(text.to_owned()),
    }
}

fn human(id: MessageId, text: &str) -> BaseMessage {
    BaseMessage::Human {
        id,
        content: text.into(),
    }
}

fn selection(
    entry: &peri_acp_types::session::MqEntry,
    disposition: MqReceiveDisposition,
) -> MqReceiveSelection {
    MqReceiveSelection {
        items: vec![MqReceiveItem {
            entry: entry.clone(),
            disposition,
        }],
        purpose: MqReceivePurpose::ModelLoop,
        execution: peri_acp_types::session::ExecutionBinding {
            turn_id: peri_acp_types::session::TurnId::new(),
            attempt_id: peri_acp_types::identity::AttemptId::new(),
        },
    }
}

async fn count(pool: &sqlx::SqlitePool, sql: &str, thread: &ThreadId) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .bind(thread.as_str())
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn mq_count(fixture: &Fixture, thread: &ThreadId) -> i64 {
    count(
        &fixture.store.database.pool,
        "SELECT COUNT(*) FROM thread_mq WHERE thread_id = ?1",
        thread,
    )
    .await
}

async fn history_len(fixture: &Fixture, thread: &ThreadId) -> i64 {
    count(
        &fixture.store.database.pool,
        "SELECT COUNT(*) FROM messages WHERE thread_id = ?1",
        thread,
    )
    .await
}

/// 入队后的完整事实在重开后逐项一致（内容、时间、原稿、策略、目标 thread）。
#[tokio::test]
async fn enqueue_survives_reopen_with_every_fact() {
    let fixture = Fixture::new().await;
    let thread = fixture.thread("mq").await;
    let id = MessageId::new();
    let requested = MqInput {
        id,
        kind: MessageKind::Defer,
        source: MessageSource::SubAgentComplete,
        payload: PersistedPayload::Message(human(id, "child done")),
        policy: MessagePolicy::passive(),
        original_draft: Some("draft text".to_owned()),
    };
    let outcome = fixture
        .data
        .mq_enqueue(&thread, requested.clone())
        .await
        .unwrap();
    assert_eq!(outcome.disposition, MqEnqueueDisposition::Inserted);
    assert_eq!(outcome.entry.id, id);
    assert_eq!(outcome.entry.original_draft.as_deref(), Some("draft text"));
    assert_micros_utc(&outcome.entry.enqueued_at);

    let path = fixture.path();
    fixture.store.close().await;
    let reopened = SqliteThreadStore::new(&path).await.unwrap();
    let data = SqliteSessionData::new(Arc::clone(&reopened.database));
    let entries = data.mq_query(&thread, MqFilter::default()).await.unwrap();
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(
        entry.enqueued_at, outcome.entry.enqueued_at,
        "重开不得改写入队时间"
    );
    assert_eq!(entry.kind, MessageKind::Defer);
    assert_eq!(entry.source, MessageSource::SubAgentComplete);
    assert_eq!(entry.policy, MessagePolicy::passive());
    assert_eq!(entry.original_draft.as_deref(), Some("draft text"));
    assert_eq!(
        peri_acp_types::store::serialize_persisted_payload(&entry.payload).unwrap(),
        peri_acp_types::store::serialize_persisted_payload(&requested.payload).unwrap()
    );
    reopened.close().await;
}

fn assert_micros_utc(value: &str) {
    // `YYYY-MM-DDTHH:MM:SS.ffffffZ`：固定六位小数 + Z。
    assert_eq!(value.len(), 27, "{value}");
    assert!(value.ends_with('Z'), "{value}");
    let fraction = &value[20..26];
    assert!(
        fraction.chars().all(|c| c.is_ascii_digit()),
        "六位小数: {value}"
    );
    assert_eq!(&value[19..20], ".", "{value}");
}

/// 查询按 `(enqueued_at, id)` 升序，四种过滤各按自己的口径收敛。
#[tokio::test]
async fn query_orders_and_applies_each_filter() {
    let fixture = Fixture::new().await;
    let thread = fixture.thread("mq").await;
    let first = MessageId::new();
    let second = MessageId::new();
    let third = MessageId::new();
    fixture
        .data
        .mq_enqueue(&thread, input(first, "first"))
        .await
        .unwrap();
    fixture
        .data
        .mq_enqueue(
            &thread,
            MqInput {
                kind: MessageKind::Defer,
                source: MessageSource::ShellComplete,
                ..input(second, "second")
            },
        )
        .await
        .unwrap();
    fixture
        .data
        .mq_enqueue(
            &thread,
            MqInput {
                kind: MessageKind::Info,
                source: MessageSource::SystemInjected,
                original_draft: None,
                ..input(third, "third")
            },
        )
        .await
        .unwrap();

    let all = fixture
        .data
        .mq_query(&thread, MqFilter::default())
        .await
        .unwrap();
    assert_eq!(
        all.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        vec![first, second, third],
        "入队顺序即 (enqueued_at, id) 升序"
    );

    let user_only = fixture
        .data
        .mq_query(
            &thread,
            MqFilter {
                user_input_only: true,
                ..MqFilter::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(user_only.len(), 1);
    assert_eq!(user_only[0].id, first);

    let shells = fixture
        .data
        .mq_query(
            &thread,
            MqFilter {
                sources: vec![MessageSource::ShellComplete, MessageSource::SystemInjected],
                ..MqFilter::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        shells.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        vec![second, third]
    );

    let after = fixture
        .data
        .mq_query(
            &thread,
            MqFilter {
                after: Some(all[0].watermark()),
                ..MqFilter::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        after.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        vec![second, third],
        "水位是严格晚于"
    );

    let limited = fixture
        .data
        .mq_query(
            &thread,
            MqFilter {
                limit: Some(2),
                ..MqFilter::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        limited.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        vec![first, second],
        "上限按升序取前 N 条"
    );
    assert_eq!(mq_count(&fixture, &thread).await, 3, "查询不消费");
}

/// 同 ID 同内容复用原接纳结果；不同内容或跨 thread 是身份冲突。
#[tokio::test]
async fn same_id_is_idempotent_but_a_different_message_conflicts() {
    let fixture = Fixture::new().await;
    let thread = fixture.thread("mq").await;
    let other = fixture.thread("other").await;
    let id = MessageId::new();
    let first = fixture
        .data
        .mq_enqueue(&thread, input(id, "same"))
        .await
        .unwrap();
    let again = fixture
        .data
        .mq_enqueue(&thread, input(id, "same"))
        .await
        .unwrap();
    assert_eq!(again.disposition, MqEnqueueDisposition::Existing);
    assert_eq!(again.entry.enqueued_at, first.entry.enqueued_at);
    assert_eq!(mq_count(&fixture, &thread).await, 1);

    let conflict = fixture
        .data
        .mq_enqueue(&thread, input(id, "different"))
        .await
        .unwrap_err();
    assert!(
        matches!(conflict.kind(), SessionResourceErrorKind::Conflict { .. }),
        "{conflict:?}"
    );

    let cross = fixture
        .data
        .mq_enqueue(&other, input(id, "same"))
        .await
        .unwrap_err();
    assert!(
        matches!(cross.kind(), SessionResourceErrorKind::Conflict { .. }),
        "同一 ID 属于别的 thread: {cross:?}"
    );
    assert_eq!(mq_count(&fixture, &other).await, 0);
}

/// 已移交的 ID 再入队只报告 AlreadyHandedOff，不产生第二条待发副本。
#[tokio::test]
async fn enqueue_after_receive_reports_already_handed_off() {
    let fixture = Fixture::new().await;
    let thread = fixture.thread("mq").await;
    let other = fixture.thread("other").await;
    let id = MessageId::new();
    let entry = fixture
        .data
        .mq_enqueue(&thread, input(id, "hand off"))
        .await
        .unwrap();
    let received = fixture
        .data
        .mq_receive(
            &thread,
            selection(&entry.entry, MqReceiveDisposition::Commit),
        )
        .await
        .unwrap();
    assert_eq!(received.items[0].status, MqReceiveStatus::Committed);
    assert_eq!(history_len(&fixture, &thread).await, 1);
    assert_eq!(mq_count(&fixture, &thread).await, 0);

    let again = fixture
        .data
        .mq_enqueue(&thread, input(id, "hand off"))
        .await
        .unwrap();
    assert_eq!(again.disposition, MqEnqueueDisposition::AlreadyHandedOff);
    assert_eq!(mq_count(&fixture, &thread).await, 0);

    let cross = fixture
        .data
        .mq_enqueue(&other, input(id, "hand off"))
        .await
        .unwrap_err();
    assert!(matches!(
        cross.kind(),
        SessionResourceErrorKind::Conflict { .. }
    ));
}

/// 取消只接受用户输入；移交过的 ID 报 AlreadyHandedOff，空 ID 报 NotFound。
#[tokio::test]
async fn cancel_returns_the_row_and_refuses_non_user_input() {
    let fixture = Fixture::new().await;
    let thread = fixture.thread("mq").await;
    let id = MessageId::new();
    fixture
        .data
        .mq_enqueue(&thread, input(id, "cancel me"))
        .await
        .unwrap();
    let outcome = fixture.data.mq_cancel(&thread, id).await.unwrap();
    let MqCancelOutcome::Cancelled(entry) = outcome else {
        panic!("expected a cancelled row");
    };
    assert_eq!(entry.original_draft.as_deref(), Some("cancel me"));
    assert_eq!(mq_count(&fixture, &thread).await, 0);
    assert_eq!(history_len(&fixture, &thread).await, 0, "取消不写历史");
    assert!(matches!(
        fixture.data.mq_cancel(&thread, id).await.unwrap(),
        MqCancelOutcome::NotFound
    ));

    let deferred = MessageId::new();
    fixture
        .data
        .mq_enqueue(
            &thread,
            MqInput {
                kind: MessageKind::Defer,
                source: MessageSource::SubAgentComplete,
                ..input(deferred, "cannot cancel")
            },
        )
        .await
        .unwrap();
    let error = fixture.data.mq_cancel(&thread, deferred).await.unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::InvalidInput { .. }
    ));
    assert_eq!(mq_count(&fixture, &thread).await, 1, "拒绝不得删行");
}

/// 取消与 Receive 竞争：串行化下只有一个结果成立，且不会出现「删了待发项却报告已发送」。
#[tokio::test]
async fn cancel_and_receive_have_exactly_one_winner() {
    let fixture = Fixture::new().await;
    let thread = fixture.thread("mq").await;
    let id = MessageId::new();
    let entry = fixture
        .data
        .mq_enqueue(&thread, input(id, "race"))
        .await
        .unwrap();
    let (cancelled, received) = tokio::join!(
        fixture.data.mq_cancel(&thread, id),
        fixture.data.mq_receive(
            &thread,
            selection(&entry.entry, MqReceiveDisposition::Commit)
        ),
    );
    let cancel = cancelled.unwrap();
    let receive = received.unwrap();
    let committed = receive.items[0].status == MqReceiveStatus::Committed;
    let cancelled_now = matches!(cancel, MqCancelOutcome::Cancelled(_));
    assert!(
        committed ^ cancelled_now,
        "只有一个结果成立：cancel={cancel:?} receive={:?}",
        receive.items[0].status
    );
    if committed {
        assert!(matches!(cancel, MqCancelOutcome::AlreadyHandedOff));
        assert_eq!(history_len(&fixture, &thread).await, 1);
    } else {
        assert_eq!(receive.items[0].status, MqReceiveStatus::Missing);
        assert_eq!(history_len(&fixture, &thread).await, 0);
    }
    assert_eq!(mq_count(&fixture, &thread).await, 0);
}

/// 历史已有同 ID 不同内容：Conflict 删 MQ 行、绝不写历史（整批仍是提交，但历史不动）。
#[tokio::test]
async fn receive_conflict_removes_the_pending_row_without_writing_history() {
    let fixture = Fixture::new().await;
    let thread = fixture.thread("mq").await;
    let id = MessageId::new();
    let entry = fixture
        .data
        .mq_enqueue(&thread, input(id, "pending"))
        .await
        .unwrap();
    let history = PersistedPayload::Message(human(id, "already different"));
    fixture
        .data
        .append_history(&thread, &[history])
        .await
        .unwrap();

    let outcome = fixture
        .data
        .mq_receive(
            &thread,
            selection(&entry.entry, MqReceiveDisposition::Commit),
        )
        .await
        .unwrap();
    assert_eq!(outcome.items[0].status, MqReceiveStatus::Conflict);
    assert_eq!(mq_count(&fixture, &thread).await, 0);
    let (content,): (String,) =
        sqlx::query_as("SELECT content FROM messages WHERE thread_id = ?1 AND message_id = ?2")
            .bind(thread.as_str())
            .bind(id.as_uuid().to_string())
            .fetch_one(&fixture.store.database.pool)
            .await
            .unwrap();
    assert!(
        content.contains("already different"),
        "历史不能被待发内容覆盖: {content}"
    );
}

/// 同 ID 同内容已在历史：AlreadyPresent，只删 MQ 行。
#[tokio::test]
async fn receive_reports_already_present_for_the_same_content() {
    let fixture = Fixture::new().await;
    let thread = fixture.thread("mq").await;
    let id = MessageId::new();
    let requested = input(id, "same content");
    let entry = fixture
        .data
        .mq_enqueue(&thread, requested.clone())
        .await
        .unwrap();
    fixture
        .data
        .append_history(&thread, &[requested.payload])
        .await
        .unwrap();
    let outcome = fixture
        .data
        .mq_receive(
            &thread,
            selection(&entry.entry, MqReceiveDisposition::Commit),
        )
        .await
        .unwrap();
    assert_eq!(outcome.items[0].status, MqReceiveStatus::AlreadyPresent);
    assert_eq!(history_len(&fixture, &thread).await, 1);
    assert_eq!(mq_count(&fixture, &thread).await, 0);
}

/// 选择基于旧快照（水位不符）是 Missing：什么都不写。
#[tokio::test]
async fn receive_missing_selection_writes_nothing() {
    let fixture = Fixture::new().await;
    let thread = fixture.thread("mq").await;
    let id = MessageId::new();
    let entry = fixture
        .data
        .mq_enqueue(&thread, input(id, "pending"))
        .await
        .unwrap();
    let mut stale = entry.entry.clone();
    stale.enqueued_at = "2000-01-01T00:00:00.000000Z".to_owned();
    let outcome = fixture
        .data
        .mq_receive(&thread, selection(&stale, MqReceiveDisposition::Commit))
        .await
        .unwrap();
    assert_eq!(outcome.items[0].status, MqReceiveStatus::Missing);
    assert_eq!(history_len(&fixture, &thread).await, 0);
    assert_eq!(
        mq_count(&fixture, &thread).await,
        1,
        "不匹配的行必须原样保留"
    );
}

/// 批次内失败必须整批回滚：损坏行让 Receive 失败时，历史与其它 MQ 行都不动。
#[tokio::test]
async fn receive_rolls_back_the_whole_batch_on_a_corrupt_row() {
    let fixture = Fixture::new().await;
    let thread = fixture.thread("mq").await;
    let good = fixture
        .data
        .mq_enqueue(&thread, input(MessageId::new(), "good"))
        .await
        .unwrap();
    let corrupt = MessageId::new();
    sqlx::query(
        "INSERT INTO thread_mq (thread_id, id, enqueued_at, kind, source, policy_json,
            payload_json, original_draft) VALUES (?1, ?2, ?3, 'prompt', 'user_input', ?4, ?5, NULL)",
    )
    .bind(thread.as_str())
    .bind(corrupt.as_uuid().to_string())
    .bind("2026-11-01T00:00:00.000000Z")
    .bind(serde_json::to_string(&MessagePolicy::ensure_processing()).unwrap())
    .bind("not a persisted payload")
    .execute(&fixture.store.database.pool)
    .await
    .unwrap();
    let broken = peri_acp_types::session::MqEntry {
        id: corrupt,
        enqueued_at: "2026-11-01T00:00:00.000000Z".to_owned(),
        kind: MessageKind::Prompt,
        source: MessageSource::UserInput,
        payload: good.entry.payload.clone(),
        policy: MessagePolicy::ensure_processing(),
        original_draft: None,
    };
    let selection = MqReceiveSelection {
        items: vec![
            MqReceiveItem {
                entry: good.entry.clone(),
                disposition: MqReceiveDisposition::Commit,
            },
            MqReceiveItem {
                entry: broken,
                disposition: MqReceiveDisposition::Commit,
            },
        ],
        purpose: MqReceivePurpose::ModelLoop,
        execution: selection(&good.entry, MqReceiveDisposition::Commit).execution,
    };
    let error = fixture
        .data
        .mq_receive(&thread, selection)
        .await
        .unwrap_err();
    assert!(
        matches!(error.kind(), SessionResourceErrorKind::Corrupt { .. }),
        "{error:?}"
    );
    assert_eq!(history_len(&fixture, &thread).await, 0, "前一项也不得写入");
    assert_eq!(mq_count(&fixture, &thread).await, 2, "两个待发行都原样保留");
}

/// Suppress 只删 MQ 行，不写历史。
#[tokio::test]
async fn receive_suppress_removes_only_the_pending_row() {
    let fixture = Fixture::new().await;
    let thread = fixture.thread("mq").await;
    let entry = fixture
        .data
        .mq_enqueue(
            &thread,
            MqInput {
                kind: MessageKind::Info,
                source: MessageSource::HookStopIntent,
                original_draft: None,
                ..input(MessageId::new(), "stale control")
            },
        )
        .await
        .unwrap();
    let outcome = fixture
        .data
        .mq_receive(
            &thread,
            selection(&entry.entry, MqReceiveDisposition::Suppress),
        )
        .await
        .unwrap();
    assert_eq!(outcome.items[0].status, MqReceiveStatus::Suppressed);
    assert_eq!(history_len(&fixture, &thread).await, 0);
    assert_eq!(mq_count(&fixture, &thread).await, 0);
}

/// 移交同时维护计数与首条用户消息标题。
#[tokio::test]
async fn receive_maintains_count_and_derived_title() {
    let fixture = Fixture::new().await;
    let thread = fixture.thread("mq").await;
    let entry = fixture
        .data
        .mq_enqueue(&thread, input(MessageId::new(), "derived title"))
        .await
        .unwrap();
    fixture
        .data
        .mq_receive(
            &thread,
            selection(&entry.entry, MqReceiveDisposition::Commit),
        )
        .await
        .unwrap();
    let (title, count): (Option<String>, i64) =
        sqlx::query_as("SELECT title, message_count FROM threads WHERE id = ?1")
            .bind(thread.as_str())
            .fetch_one(&fixture.store.database.pool)
            .await
            .unwrap();
    assert_eq!(title.as_deref(), Some("derived title"));
    assert_eq!(count, 1);
}

/// 提交前重开：MQ 行在、历史不在；移交后重开：历史在、MQ 不在。
#[tokio::test]
async fn reopen_on_both_sides_of_the_receive_commit() {
    let fixture = Fixture::new().await;
    let thread = fixture.thread("mq").await;
    let entry = fixture
        .data
        .mq_enqueue(&thread, input(MessageId::new(), "survives"))
        .await
        .unwrap();
    let path = fixture.path();
    fixture.store.close().await;
    let reopened = SqliteThreadStore::new(&path).await.unwrap();
    let data = SqliteSessionData::new(Arc::clone(&reopened.database));
    assert_eq!(
        data.mq_query(&thread, MqFilter::default())
            .await
            .unwrap()
            .len(),
        1,
        "提交前重开：MQ 行还在"
    );
    let history: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE thread_id = ?1")
        .bind(thread.as_str())
        .fetch_one(&reopened.database.pool)
        .await
        .unwrap();
    assert_eq!(history, 0, "提交前重开：历史不存在");

    let received = data
        .mq_receive(
            &thread,
            selection(&entry.entry, MqReceiveDisposition::Commit),
        )
        .await
        .unwrap();
    assert_eq!(received.items[0].status, MqReceiveStatus::Committed);
    reopened.close().await;
    let third = SqliteThreadStore::new(&path).await.unwrap();
    let data = SqliteSessionData::new(Arc::clone(&third.database));
    assert!(data
        .mq_query(&thread, MqFilter::default())
        .await
        .unwrap()
        .is_empty());
    let history: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE thread_id = ?1")
        .bind(thread.as_str())
        .fetch_one(&third.database.pool)
        .await
        .unwrap();
    assert_eq!(history, 1);
    third.close().await;
}

/// rewind 与 mq_clear 都清理该 thread 的待发行，且不碰历史保留前缀。
#[tokio::test]
async fn rewind_and_clear_drop_pending_rows() {
    let fixture = Fixture::new().await;
    let thread = fixture.thread("mq").await;
    let first = fixture
        .data
        .mq_enqueue(&thread, input(MessageId::new(), "first"))
        .await
        .unwrap();
    fixture
        .data
        .mq_receive(
            &thread,
            selection(&first.entry, MqReceiveDisposition::Commit),
        )
        .await
        .unwrap();
    fixture
        .data
        .mq_enqueue(&thread, input(MessageId::new(), "pending"))
        .await
        .unwrap();
    let outcome = fixture
        .data
        .rewind_history(&thread, RewindBoundary::KeepThrough(first.entry.id), &[])
        .await
        .unwrap();
    assert_eq!(
        outcome,
        peri_acp_types::session_resources::RewindOutcome::Applied
    );
    assert_eq!(mq_count(&fixture, &thread).await, 0, "rewind 清空待发行");
    assert_eq!(history_len(&fixture, &thread).await, 1, "保留前缀不动");

    fixture
        .data
        .mq_enqueue(&thread, input(MessageId::new(), "again"))
        .await
        .unwrap();
    fixture.data.mq_clear(&thread).await.unwrap();
    assert_eq!(mq_count(&fixture, &thread).await, 0);
}

/// thread 删除清理 MQ（显式删除，不依赖级联）。
#[tokio::test]
async fn deleting_the_thread_drops_pending_rows() {
    let fixture = Fixture::new().await;
    let thread = fixture.thread("mq").await;
    let id = MessageId::new();
    fixture
        .data
        .mq_enqueue(&thread, input(id, "pending"))
        .await
        .unwrap();
    fixture.data.delete_tree(&thread).await.unwrap();
    let (rows,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM thread_mq WHERE id = ?1")
        .bind(id.as_uuid().to_string())
        .fetch_one(&fixture.store.database.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
}

// ─── 迁移：v12 → v13 与 v11 → v13 ─────────────────────────────────────────────

/// 把库降级成 v12 形状（删 `thread_mq`）后重开：幂等补齐并推进到当前版本。
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

    let reopened = SqliteThreadStore::new(&path).await.unwrap();
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

    let reopened = SqliteThreadStore::new(&path).await.unwrap();
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
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(false),
    )
    .await
    .unwrap();
    sqlx::raw_sql(
        "DROP TABLE thread_mq;
         DROP TABLE IF EXISTS inherited_payload_index;
         DROP TABLE IF EXISTS inherited_snapshot_index;",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "PRAGMA user_version = {version}"
    )))
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
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
