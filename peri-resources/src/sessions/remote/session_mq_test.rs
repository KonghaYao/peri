//! 远端 Thread MQ：条件批处理、状态判定与契约升级。
//!
//! 夹具让一个真 SQLite 池扮演远端服务端（同一个 [`SqliteTransport`]），因此走的是真实
//! 托管批次与账本：条件插入/删除的影响行数就是生产路径读到的证据。

use std::sync::Arc;

use peri_acp_types::messages::{BaseMessage, MessageId};
use peri_acp_types::session::{
    MessageKind, MessagePolicy, MessageSource, MqCancelOutcome, MqEnqueueDisposition, MqFilter,
    MqInput, MqReceiveDisposition, MqReceiveItem, MqReceivePurpose, MqReceiveSelection,
    MqReceiveStatus, TurnId,
};
use peri_acp_types::session_resources::SessionResourceErrorKind;
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::thread::ThreadId;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

use super::generation::{ConnectionFactory, ConnectionGate};
use super::mutation::{RemoteStore, StoreAccess};
use super::schema::{self, StoreId};
use super::schema_upgrade_tests::SqliteTransport;
use super::session_data::RemoteSessionData;
use crate::sessions::data::SessionDataPort;

struct Fixture {
    _directory: tempfile::TempDir,
    pool: SqlitePool,
    data: RemoteSessionData,
}

struct MqFactory {
    transport: Arc<SqliteTransport>,
    gate: Arc<ConnectionGate>,
}

#[async_trait::async_trait]
impl ConnectionFactory for MqFactory {
    async fn connect(
        &self,
    ) -> peri_acp_types::session_resources::SessionResourceResult<RemoteStore> {
        Ok(RemoteStore::new(
            self.transport.clone(),
            StoreAccess::ReadWrite,
            self.gate.mint(),
            self.gate.clone(),
        ))
    }
}

/// 当前契约的远端库 + 两条会话行（外键不强制，与真实远端同一条件）。
async fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(directory.path().join("remote-mq.db"))
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
    sqlx::query("INSERT INTO peri_store_meta VALUES (0, ?1, 'mq-test', ?2, 'now')")
        .bind(schema::REMOTE_SCHEMA_VERSION)
        .bind(schema::STORE_CONTRACT)
        .execute(&pool)
        .await
        .unwrap();
    for thread in ["mq-thread", "other-thread"] {
        sqlx::query(
            "INSERT INTO threads (id, cwd, created_at, updated_at, workspace_id)
             VALUES (?1, '/tmp', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'ws')",
        )
        .bind(thread)
        .execute(&pool)
        .await
        .unwrap();
    }
    let transport = Arc::new(SqliteTransport::for_test(pool.clone()));
    let gate = Arc::new(ConnectionGate::default());
    let store = RemoteStore::new(
        transport.clone(),
        StoreAccess::ReadWrite,
        gate.mint(),
        gate.clone(),
    );
    let data = RemoteSessionData::with_connection_for_test(
        StoreId::mint(),
        store,
        Arc::new(MqFactory {
            transport,
            gate: gate.clone(),
        }),
        gate,
    );
    Fixture {
        _directory: directory,
        pool,
        data,
    }
}

fn thread(name: &str) -> ThreadId {
    name.to_owned()
}

fn human(id: MessageId, text: &str) -> BaseMessage {
    BaseMessage::Human {
        id,
        content: text.into(),
    }
}

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
            turn_id: TurnId::new(),
            attempt_id: peri_acp_types::identity::AttemptId::new(),
        },
    }
}

async fn mq_rows(fixture: &Fixture, thread: &ThreadId) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM thread_mq WHERE thread_id = ?1")
        .bind(thread.as_str())
        .fetch_one(&fixture.pool)
        .await
        .unwrap()
}

async fn history_rows(fixture: &Fixture, thread: &ThreadId) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE thread_id = ?1")
        .bind(thread.as_str())
        .fetch_one(&fixture.pool)
        .await
        .unwrap()
}

/// 契约与版本代数是本代：`v7` + 13。
#[test]
fn the_mq_generation_is_the_current_contract() {
    assert_eq!(schema::STORE_CONTRACT, "peri.session.store/v7");
    assert_eq!(schema::REMOTE_SCHEMA_VERSION, 13);
    assert_eq!(
        schema::PREVIOUS_CURRENT_STORE_CONTRACT,
        "peri.session.store/v6"
    );
}

/// 条件插入：第一次接纳，第二次复用原水位；不同内容与跨 thread 都是身份冲突。
#[tokio::test]
async fn enqueue_is_conditional_and_conflicts_on_identity() {
    let fixture = fixture().await;
    let main = thread("mq-thread");
    let other = thread("other-thread");
    let id = MessageId::new();

    let first = fixture
        .data
        .mq_enqueue(&main, input(id, "same"))
        .await
        .unwrap();
    assert_eq!(first.disposition, MqEnqueueDisposition::Inserted);
    let again = fixture
        .data
        .mq_enqueue(&main, input(id, "same"))
        .await
        .unwrap();
    assert_eq!(again.disposition, MqEnqueueDisposition::Existing);
    assert_eq!(again.entry.enqueued_at, first.entry.enqueued_at);
    assert_eq!(mq_rows(&fixture, &main).await, 1);

    let conflict = fixture
        .data
        .mq_enqueue(&main, input(id, "different"))
        .await
        .unwrap_err();
    assert!(matches!(
        conflict.kind(),
        SessionResourceErrorKind::Conflict { .. }
    ));

    let cross = fixture
        .data
        .mq_enqueue(&other, input(id, "same"))
        .await
        .unwrap_err();
    assert!(matches!(
        cross.kind(),
        SessionResourceErrorKind::Conflict { .. }
    ));
    assert_eq!(mq_rows(&fixture, &other).await, 0);
}

/// 移交的状态判定完全由批次影响行数给出：Committed / Missing / Suppressed。
#[tokio::test]
async fn receive_classifies_from_batch_counts() {
    let fixture = fixture().await;
    let main = thread("mq-thread");
    let entry = fixture
        .data
        .mq_enqueue(&main, input(MessageId::new(), "ship it"))
        .await
        .unwrap();

    let committed = fixture
        .data
        .mq_receive(&main, selection(&entry.entry, MqReceiveDisposition::Commit))
        .await
        .unwrap();
    assert_eq!(committed.items[0].status, MqReceiveStatus::Committed);
    assert_eq!(history_rows(&fixture, &main).await, 1);
    assert_eq!(mq_rows(&fixture, &main).await, 0);
    let (title, count): (Option<String>, i64) =
        sqlx::query_as("SELECT title, message_count FROM threads WHERE id = ?1")
            .bind(main.as_str())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(title.as_deref(), Some("ship it"));
    assert_eq!(count, 1);

    // 同一选择再来一次：MQ 行已不在，什么都没写。
    let missing = fixture
        .data
        .mq_receive(&main, selection(&entry.entry, MqReceiveDisposition::Commit))
        .await
        .unwrap();
    assert_eq!(missing.items[0].status, MqReceiveStatus::Missing);
    assert_eq!(history_rows(&fixture, &main).await, 1);

    let suppressed = fixture
        .data
        .mq_enqueue(
            &main,
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
            &main,
            selection(&suppressed.entry, MqReceiveDisposition::Suppress),
        )
        .await
        .unwrap();
    assert_eq!(outcome.items[0].status, MqReceiveStatus::Suppressed);
    assert_eq!(history_rows(&fixture, &main).await, 1, "Suppress 不写历史");
    assert_eq!(mq_rows(&fixture, &main).await, 0);
}

/// 历史已有同 ID 不同内容：Conflict，MQ 行删除、历史不被覆盖。
#[tokio::test]
async fn receive_conflict_keeps_history_and_drops_the_pending_row() {
    let fixture = fixture().await;
    let main = thread("mq-thread");
    let id = MessageId::new();
    let entry = fixture
        .data
        .mq_enqueue(&main, input(id, "pending"))
        .await
        .unwrap();
    fixture
        .data
        .append_history(&main, &[PersistedPayload::Message(human(id, "history"))])
        .await
        .unwrap();

    let outcome = fixture
        .data
        .mq_receive(&main, selection(&entry.entry, MqReceiveDisposition::Commit))
        .await
        .unwrap();
    assert_eq!(outcome.items[0].status, MqReceiveStatus::Conflict);
    assert_eq!(mq_rows(&fixture, &main).await, 0);
    let (content,): (String,) =
        sqlx::query_as("SELECT content FROM messages WHERE thread_id = ?1 AND message_id = ?2")
            .bind(main.as_str())
            .bind(id.as_uuid().to_string())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert!(content.contains("history"), "{content}");
    assert_eq!(history_rows(&fixture, &main).await, 1);
}

/// 历史已有同 ID 同内容：AlreadyPresent，只删 MQ 行，不重复写历史。
#[tokio::test]
async fn receive_reports_already_present_for_the_same_content() {
    let fixture = fixture().await;
    let main = thread("mq-thread");
    let id = MessageId::new();
    let requested = input(id, "same content");
    let entry = fixture
        .data
        .mq_enqueue(&main, requested.clone())
        .await
        .unwrap();
    fixture
        .data
        .append_history(&main, &[requested.payload])
        .await
        .unwrap();

    let outcome = fixture
        .data
        .mq_receive(&main, selection(&entry.entry, MqReceiveDisposition::Commit))
        .await
        .unwrap();
    assert_eq!(outcome.items[0].status, MqReceiveStatus::AlreadyPresent);
    assert_eq!(history_rows(&fixture, &main).await, 1);
    assert_eq!(mq_rows(&fixture, &main).await, 0);
}

/// 取消按 `(thread_id, id, enqueued_at)` 删除并有条件地报告结果。
#[tokio::test]
async fn cancel_deletes_by_watermark_and_reports_state() {
    let fixture = fixture().await;
    let main = thread("mq-thread");
    let id = MessageId::new();
    fixture
        .data
        .mq_enqueue(&main, input(id, "cancel me"))
        .await
        .unwrap();
    let outcome = fixture.data.mq_cancel(&main, id).await.unwrap();
    let MqCancelOutcome::Cancelled(entry) = outcome else {
        panic!("expected a cancelled row");
    };
    assert_eq!(entry.original_draft.as_deref(), Some("cancel me"));
    assert_eq!(mq_rows(&fixture, &main).await, 0);
    assert!(matches!(
        fixture.data.mq_cancel(&main, id).await.unwrap(),
        MqCancelOutcome::NotFound
    ));

    let deferred = MessageId::new();
    fixture
        .data
        .mq_enqueue(
            &main,
            MqInput {
                kind: MessageKind::Defer,
                source: MessageSource::SubAgentComplete,
                ..input(deferred, "cannot cancel")
            },
        )
        .await
        .unwrap();
    let error = fixture.data.mq_cancel(&main, deferred).await.unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::InvalidInput { .. }
    ));
    assert_eq!(mq_rows(&fixture, &main).await, 1);
}

/// 查询走一致读取并按 `(enqueued_at, id)` 升序；清理与删除树都显式删行。
#[tokio::test]
async fn query_clear_and_tree_deletion_follow_the_thread_scope() {
    let fixture = fixture().await;
    let main = thread("mq-thread");
    let other = thread("other-thread");
    let ids = [MessageId::new(), MessageId::new()];
    for (index, id) in ids.iter().enumerate() {
        fixture
            .data
            .mq_enqueue(&main, input(*id, &format!("message {index}")))
            .await
            .unwrap();
    }
    let entries = fixture
        .data
        .mq_query(&main, MqFilter::default())
        .await
        .unwrap();
    assert_eq!(
        entries.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        ids
    );
    assert!(
        fixture
            .data
            .mq_query(&other, MqFilter::default())
            .await
            .unwrap()
            .is_empty(),
        "MQ 只按 thread 作用域读取"
    );

    fixture.data.mq_clear(&main).await.unwrap();
    assert_eq!(mq_rows(&fixture, &main).await, 0);

    fixture
        .data
        .mq_enqueue(&main, input(MessageId::new(), "pending"))
        .await
        .unwrap();
    fixture.data.delete_tree(&main).await.unwrap();
    let (rows,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM thread_mq")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
}
