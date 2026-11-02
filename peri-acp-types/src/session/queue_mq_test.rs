//! MessageQueue 持久门面语义：入队即事实、发送选择、原子移交、水位与唤醒。

use super::*;
use crate::identity::AttemptId;
use crate::messages::{BaseMessage, MessageId};
use crate::session::mq_memory::InMemoryMqStore;
use crate::session::{MqCancelOutcome, MqEnqueueDisposition, MqReceiveStatus, TurnId};
use crate::session_resources::{
    BindingRecheck, BindingState, ChildResumeClaim, ChildSnapshot, CloseSettlement, ForkSnapshot,
    FrozenSnapshotBytes, FrozenState, NewSession, NewSessionDraft, PersistenceRecovery,
    RewindBoundary, RewindOutcome, SessionAvailability, SessionInitialization, SessionMetaPatch,
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult, SessionResources,
    SessionSnapshot,
};
use crate::store::{CompactionChange, MessageFlags, PersistedPayload};
use crate::system_reminder::{
    ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource as CanonicalSource, SystemReminder, TrustedSystemReminder,
    TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};
use crate::thread::{ThreadId, ThreadMeta};
use crate::workspace::{ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery};
use async_trait::async_trait;
use serde_json::json;

// ─── helpers ────────────────────────────────────────────────────────────────

fn execution() -> ExecutionBinding {
    ExecutionBinding {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
    }
}

fn prompt(text: &str) -> QueuedMessage {
    QueuedMessage::prompt(MessageSource::UserInput, BaseMessage::human(text))
}

fn reminder(body: &str) -> TrustedSystemReminder {
    TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: CanonicalSource("test".into()),
            kind: "completed".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![crate::system_reminder::ReminderAudience::Model]),
            body: body.into(),
            summary: None,
            metadata: json!({}),
        })
        .unwrap()
}

async fn receive_all(queue: &MessageQueue) -> MqReceiveOutcome {
    let permit = queue.begin_receive(MqReceivePurpose::ModelLoop, execution());
    let items = queue.select_for_receive(&permit).await.unwrap();
    queue.receive(permit, items).await.unwrap()
}

// ─── bind：门面不复制事实 ────────────────────────────────────────────────────

/// 真实 `SessionResources` 上的替身：只有 MQ 行为被实现，其余行为显式失败。
#[derive(Default)]
struct FakeMqResources {
    store: InMemoryMqStore,
}

fn unsupported(behavior: &str) -> SessionResourceError {
    let _ = behavior;
    SessionResourceError::new(SessionResourceErrorKind::Unsupported)
}

macro_rules! forward_mq_only {
    ($( $method:ident ( $( $arg:ident : $ty:ty ),* ) -> $out:ty; )*) => {
        #[async_trait]
        impl SessionResources for FakeMqResources {
            $(async fn $method(&self, $($arg: $ty),*) -> SessionResourceResult<$out> {
                let _ = ($($arg),*);
                Err(unsupported(stringify!($method)))
            })*

            async fn mq_enqueue(
                &self,
                thread_id: &ThreadId,
                input: MqInput,
            ) -> SessionResourceResult<MqEnqueueOutcome> {
                ThreadMqStore::mq_enqueue(&self.store, thread_id, input).await
            }

            async fn mq_query(
                &self,
                thread_id: &ThreadId,
                filter: MqFilter,
            ) -> SessionResourceResult<Vec<MqEntry>> {
                ThreadMqStore::mq_query(&self.store, thread_id, filter).await
            }

            async fn mq_cancel(
                &self,
                thread_id: &ThreadId,
                id: MessageId,
            ) -> SessionResourceResult<MqCancelOutcome> {
                ThreadMqStore::mq_cancel(&self.store, thread_id, id).await
            }

            async fn mq_receive(
                &self,
                thread_id: &ThreadId,
                selection: MqReceiveSelection,
            ) -> SessionResourceResult<MqReceiveOutcome> {
                ThreadMqStore::mq_receive(&self.store, thread_id, selection).await
            }

            async fn mq_clear(&self, thread_id: &ThreadId) -> SessionResourceResult<()> {
                ThreadMqStore::mq_clear(&self.store, thread_id).await
            }
        }
    };
}

forward_mq_only! {
    inspect_availability(session: Option<&ThreadId>) -> SessionAvailability;
    resolve_workspace(cwd: &std::path::Path) -> ResolvedWorkspace;
    validate_session(id: &ThreadId, workspace: &ResolvedWorkspace) -> ();
    finish_close(id: &ThreadId) -> ();
    close_settlement(id: &ThreadId) -> CloseSettlement;
    create_session(input: &NewSession) -> ();
    abandon_initialization(id: &ThreadId) -> ();
    begin_initialization(draft: &NewSessionDraft) -> Arc<dyn SessionInitialization>;
    discard_incomplete_initialization(id: &ThreadId) -> ();
    adopt_legacy_session(id: &ThreadId, saved_cwd: &str, workspace: &ResolvedWorkspace, frozen: &FrozenSnapshotBytes) -> ();
    load_session_snapshot(id: &ThreadId) -> SessionSnapshot;
    load_session_history(id: &ThreadId) -> Vec<PersistedPayload>;
    load_frozen_state(id: &ThreadId) -> FrozenState;
    load_session_binding(id: &ThreadId) -> BindingState;
    validate_bound_workspace(id: &ThreadId, check: BindingRecheck) -> ResolvedWorkspace;
    load_session_meta(id: &ThreadId) -> ThreadMeta;
    list_sessions(query: &ScopedThreadQuery) -> ScopedThreadPage;
    list_children(parent: &ThreadId) -> Vec<ThreadMeta>;
    list_session_tree(root: &ThreadId) -> Vec<ThreadMeta>;
    append_history(id: &ThreadId, payloads: &[PersistedPayload]) -> ();
    append_reminder_if_absent(id: &ThreadId, message_id: MessageId, reminder: &TrustedSystemReminder) -> bool;
    mark_session_closing(id: &ThreadId) -> ();
    is_session_closing(id: &ThreadId) -> bool;
    save_fork(fork: &ForkSnapshot) -> ();
    save_child(child: &ChildSnapshot) -> ();
    claim_child_resume(child: &ThreadId, root: &ThreadId) -> Box<dyn ChildResumeClaim>;
    apply_compaction(id: &ThreadId, change: &CompactionChange) -> ();
    apply_message_projections(id: &ThreadId, updates: &[(MessageId, MessageFlags)]) -> ();
    rewind_history(id: &ThreadId, boundary: RewindBoundary, clear_excluded: &[MessageId]) -> RewindOutcome;
    remove_history_entries(id: &ThreadId, ids: &[MessageId]) -> ();
    update_session_meta(id: &ThreadId, patch: &SessionMetaPatch) -> ();
    delete_session_tree(id: &ThreadId) -> ();
    recover_session_persistence(id: &ThreadId) -> PersistenceRecovery;
    drain_persistence(id: &ThreadId) -> ();
}

#[tokio::test]
async fn enqueue_is_visible_through_bound_session_resources() {
    let thread: ThreadId = "thread-1".to_owned();
    let resources = Arc::new(FakeMqResources::default());
    let queue = MessageQueue::bind(resources.clone(), thread.clone());
    assert_eq!(queue.thread_id(), &thread);

    let outcome = queue
        .enqueue(prompt("hello").with_original_draft("hello raw"))
        .await
        .unwrap();
    assert_eq!(outcome.disposition, MqEnqueueDisposition::Inserted);

    let stored = resources
        .mq_query(&thread, MqFilter::default())
        .await
        .unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].id, outcome.entry.id);
    assert_eq!(stored[0].enqueued_at, outcome.entry.enqueued_at);
    assert_eq!(stored[0].original_draft.as_deref(), Some("hello raw"));
    assert!(resources
        .mq_query(&"other".to_owned(), MqFilter::default())
        .await
        .unwrap()
        .is_empty());
}

// ─── 发送选择与 Receive ──────────────────────────────────────────────────────

#[tokio::test]
async fn send_selection_consumes_only_selected_user_inputs() {
    let queue = MessageQueue::new();
    let first = queue.enqueue(prompt("first")).await.unwrap().entry;
    let second = queue.enqueue(prompt("second")).await.unwrap().entry;
    let done = queue
        .enqueue(QueuedMessage::defer(
            MessageSource::SubAgentComplete,
            BaseMessage::human("done"),
        ))
        .await
        .unwrap()
        .entry;

    let sent = queue.send_inputs(vec![second.id]).await.unwrap();
    assert_eq!(sent.accepted.len(), 1);
    assert_eq!(sent.accepted[0].id, second.id);
    assert!(sent.missing.is_empty());

    let permit = queue.begin_receive(MqReceivePurpose::ModelLoop, execution());
    let items = queue.select_for_receive(&permit).await.unwrap();
    let selected = items.iter().map(|item| item.entry.id).collect::<Vec<_>>();
    assert_eq!(
        selected,
        vec![second.id, done.id],
        "未选中的用户输入保留在 MQ，非用户项同批消费，顺序按水位"
    );
    assert_eq!(items[0].disposition, MqReceiveDisposition::Commit);

    let outcome = queue.receive(permit, items).await.unwrap();
    assert!(outcome
        .items
        .iter()
        .all(|item| item.status == MqReceiveStatus::Committed));
    assert_eq!(queue.len().await.unwrap(), 1);
    assert_eq!(queue.snapshot().await.unwrap()[0].id(), first.id);
    assert!(
        queue.take_send_selection().is_empty(),
        "移交成功后发送选择必须结清"
    );
}

#[tokio::test]
async fn send_inputs_reports_missing_for_handed_off_or_unknown_ids() {
    let queue = MessageQueue::new();
    let entry = queue.enqueue(prompt("already sent")).await.unwrap().entry;
    queue.send_inputs(vec![entry.id]).await.unwrap();
    receive_all(&queue).await;

    let unknown = MessageId::new();
    let sent = queue.send_inputs(vec![entry.id, unknown]).await.unwrap();
    assert!(sent.accepted.is_empty());
    assert_eq!(sent.missing, vec![entry.id, unknown]);
}

#[tokio::test]
async fn failed_receive_keeps_send_selection() {
    #[derive(Default)]
    struct FailingReceiveStore {
        inner: InMemoryMqStore,
    }

    #[async_trait]
    impl ThreadMqStore for FailingReceiveStore {
        async fn mq_enqueue(
            &self,
            thread_id: &ThreadId,
            input: MqInput,
        ) -> SessionResourceResult<MqEnqueueOutcome> {
            self.inner.mq_enqueue(thread_id, input).await
        }

        async fn mq_query(
            &self,
            thread_id: &ThreadId,
            filter: MqFilter,
        ) -> SessionResourceResult<Vec<MqEntry>> {
            self.inner.mq_query(thread_id, filter).await
        }

        async fn mq_cancel(
            &self,
            thread_id: &ThreadId,
            id: MessageId,
        ) -> SessionResourceResult<MqCancelOutcome> {
            self.inner.mq_cancel(thread_id, id).await
        }

        async fn mq_receive(
            &self,
            _thread_id: &ThreadId,
            _selection: MqReceiveSelection,
        ) -> SessionResourceResult<MqReceiveOutcome> {
            Err(SessionResourceError::new(
                SessionResourceErrorKind::Unavailable {
                    detail: "injected MQ failure".to_owned(),
                },
            ))
        }

        async fn mq_clear(&self, thread_id: &ThreadId) -> SessionResourceResult<()> {
            self.inner.mq_clear(thread_id).await
        }
    }

    let queue = MessageQueue::from_store(
        Arc::new(FailingReceiveStore::default()),
        "thread-x".to_owned(),
    );
    let entry = queue.enqueue(prompt("keep me")).await.unwrap().entry;
    queue.send_inputs(vec![entry.id]).await.unwrap();
    let permit = queue.begin_receive(MqReceivePurpose::ModelLoop, execution());
    let items = queue.select_for_receive(&permit).await.unwrap();
    assert!(queue.receive(permit, items).await.is_err());
    assert_eq!(queue.take_send_selection(), vec![entry.id]);
    assert_eq!(queue.len().await.unwrap(), 1);
}

#[tokio::test]
async fn expired_run_binding_is_suppressed_without_writing_history() {
    let queue = MessageQueue::new();
    let stale = execution();
    queue
        .enqueue(
            QueuedMessage::info(MessageSource::SystemInjected, BaseMessage::human("stale"))
                .with_policy(MessagePolicy::continue_current_run(stale)),
        )
        .await
        .unwrap();

    let permit = queue.begin_receive(MqReceivePurpose::ModelLoop, execution());
    let items = queue.select_for_receive(&permit).await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].disposition, MqReceiveDisposition::Suppress);
    let outcome = queue.receive(permit, items).await.unwrap();
    assert_eq!(outcome.items[0].status, MqReceiveStatus::Suppressed);
    assert!(queue.is_empty().await.unwrap());
}

// ─── 身份、原始稿与幂等 ──────────────────────────────────────────────────────

#[tokio::test]
async fn system_reminder_delivery_id_is_fixed_on_enqueue() {
    let queue = MessageQueue::new();
    let body = reminder("done");
    let outcome = queue
        .enqueue(QueuedMessage::system_reminder(
            MessageKind::Defer,
            MessageSource::SubAgentComplete,
            body.clone(),
        ))
        .await
        .unwrap();
    let id = outcome.entry.id;
    assert_eq!(outcome.entry.payload.id(), id);

    let snapshot = queue.snapshot().await.unwrap();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].delivery_id, Some(id));
    assert_eq!(
        snapshot[0].enqueued_at,
        Some(outcome.entry.enqueued_at.clone())
    );
    assert!(matches!(
        &snapshot[0].payload,
        QueuedPayload::SystemReminder(actual) if actual == &body
    ));
}

#[tokio::test]
async fn same_delivery_id_is_idempotent_and_conflicts_are_rejected() {
    let queue = MessageQueue::new();
    let id = MessageId::new();
    let first = QueuedMessage::system_reminder_with_delivery_id(
        MessageKind::Defer,
        MessageSource::SubAgentComplete,
        reminder("done"),
        id,
    );
    let inserted = queue.enqueue(first.clone()).await.unwrap();
    assert_eq!(inserted.disposition, MqEnqueueDisposition::Inserted);
    let again = queue.enqueue(first).await.unwrap();
    assert_eq!(again.disposition, MqEnqueueDisposition::Existing);
    assert_eq!(again.entry.id, inserted.entry.id);
    assert_eq!(again.entry.enqueued_at, inserted.entry.enqueued_at);

    let conflicting = QueuedMessage::system_reminder_with_delivery_id(
        MessageKind::Defer,
        MessageSource::SubAgentComplete,
        reminder("different"),
        id,
    );
    assert!(queue.enqueue(conflicting).await.is_err());
    assert_eq!(queue.len().await.unwrap(), 1);
}

#[tokio::test]
async fn cancel_returns_the_original_draft() {
    let queue = MessageQueue::new();
    let entry = queue
        .enqueue(prompt("draft text").with_original_draft("raw draft"))
        .await
        .unwrap()
        .entry;
    match queue.cancel_user_input(&entry.id).await.unwrap() {
        MqCancelOutcome::Cancelled(row) => {
            assert_eq!(row.id, entry.id);
            assert_eq!(row.original_draft.as_deref(), Some("raw draft"));
        }
        other => panic!("expected a cancelled row, got {other:?}"),
    }
    assert!(queue.is_empty().await.unwrap());
    assert!(matches!(
        queue.cancel_user_input(&entry.id).await.unwrap(),
        MqCancelOutcome::NotFound
    ));
}

// ─── 水位 ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn watermark_and_ensure_processing_after_follow_enqueue_order() {
    let queue = MessageQueue::new();
    assert_eq!(queue.admission_watermark().await.unwrap(), None);

    let info = queue
        .enqueue(QueuedMessage::info(
            MessageSource::SystemInjected,
            BaseMessage::human("info"),
        ))
        .await
        .unwrap()
        .entry;
    let watermark = queue.admission_watermark().await.unwrap().unwrap();
    assert_eq!(watermark, info.watermark());
    assert!(!queue
        .has_ensure_processing_after(&Some(watermark.clone()))
        .await
        .unwrap());
    assert!(!queue.has_ensure_processing_after(&None).await.unwrap());

    let defer = queue
        .enqueue(QueuedMessage::defer(
            MessageSource::SubAgentComplete,
            BaseMessage::human("done"),
        ))
        .await
        .unwrap()
        .entry;
    assert!(queue
        .has_ensure_processing_after(&Some(watermark))
        .await
        .unwrap());
    assert_eq!(
        queue.admission_watermark().await.unwrap(),
        Some(defer.watermark())
    );
    assert!(queue.has_ensure_processing_after(&None).await.unwrap());
}

// ─── 唤醒 ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn send_inputs_publishes_a_wake_hint() {
    let queue = MessageQueue::new();
    let mut changes = queue.subscribe_wake();
    let entry = queue
        .enqueue(
            QueuedMessage::prompt(MessageSource::UserInput, BaseMessage::human("later"))
                .with_policy(MessagePolicy::passive()),
        )
        .await
        .unwrap()
        .entry;
    assert_eq!(*changes.borrow_and_update(), 0, "Passive 入队不发布唤醒");
    queue.send_inputs(vec![entry.id]).await.unwrap();
    assert_eq!(*changes.borrow_and_update(), 1);
}

#[tokio::test]
async fn await_wake_rechecks_mq_after_waking() {
    let queue = Arc::new(MessageQueue::new());
    let waiting = {
        let queue = Arc::clone(&queue);
        tokio::spawn(async move {
            queue.await_wake().await;
            queue.snapshot().await.unwrap()
        })
    };
    // 让等待者先注册通知再入队。
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let entry = queue
        .enqueue(QueuedMessage::defer(
            MessageSource::SubAgentComplete,
            BaseMessage::human("done"),
        ))
        .await
        .unwrap()
        .entry;
    let observed = tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
        .await
        .expect("enqueue must wake await_wake")
        .unwrap();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].id(), entry.id);
}
