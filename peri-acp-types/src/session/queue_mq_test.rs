//! MessageQueue 持久门面语义：入队即事实、发送选择、原子移交、水位与唤醒。

use super::*;
use crate::identity::AttemptId;
use crate::messages::{BaseMessage, MessageId};
use crate::session::mq_memory::InMemoryMqStore;
use crate::session::SessionInbox;
use crate::session::{
    MqCancelOutcome, MqEnqueueDisposition, MqReceiveStatus, MqSendStatus, TurnId,
};
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
    let permit = queue
        .owner()
        .begin_receive(MqReceivePurpose::ModelLoop, execution());
    let items = queue.owner().select_for_receive(&permit).await.unwrap();
    queue.owner().receive(permit, items).await.unwrap()
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

#[tokio::test]
async fn mq_filter_kinds_is_applied_after_source_filter() {
    let queue = MessageQueue::new();
    queue.enqueue(prompt("prompt")).await.unwrap();
    queue
        .enqueue(QueuedMessage::defer(
            MessageSource::SubAgentComplete,
            BaseMessage::human("defer"),
        ))
        .await
        .unwrap();
    queue
        .enqueue(QueuedMessage::info(
            MessageSource::SystemInjected,
            BaseMessage::human("info"),
        ))
        .await
        .unwrap();

    let defers = queue
        .query(MqFilter {
            kinds: vec![MessageKind::Defer],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(defers.len(), 1);
    assert_eq!(defers[0].kind, MessageKind::Defer);

    let source_and_kind = queue
        .query(MqFilter {
            sources: vec![MessageSource::SystemInjected],
            kinds: vec![MessageKind::Defer, MessageKind::Prompt],
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        source_and_kind.is_empty(),
        "来源与 Kind 必须同时满足：{source_and_kind:?}"
    );
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

    let sent = queue
        .send_inputs_within(vec![second.id], std::time::Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(sent.accepted().len(), 1);
    assert_eq!(sent.accepted()[0].id, second.id);
    assert!(sent.missing().is_empty());

    let permit = queue
        .owner()
        .begin_receive(MqReceivePurpose::ModelLoop, execution());
    let items = queue.owner().select_for_receive(&permit).await.unwrap();
    let selected = items.iter().map(|item| item.entry.id).collect::<Vec<_>>();
    assert_eq!(
        selected,
        vec![second.id, done.id],
        "未选中的用户输入保留在 MQ，非用户项同批消费，顺序按水位"
    );
    assert_eq!(items[0].disposition, MqReceiveDisposition::Commit);

    let outcome = queue.owner().receive(permit, items).await.unwrap();
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
    queue
        .send_inputs_within(vec![entry.id], std::time::Duration::ZERO)
        .await
        .unwrap();
    receive_all(&queue).await;

    let unknown = MessageId::new();
    let sent = queue.send_inputs(vec![entry.id, unknown]).await.unwrap();
    assert!(sent.accepted().is_empty());
    assert_eq!(sent.missing(), vec![entry.id, unknown]);
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

        async fn mq_history_contains(
            &self,
            thread_id: &ThreadId,
            id: MessageId,
        ) -> SessionResourceResult<bool> {
            self.inner.mq_history_contains(thread_id, id).await
        }
    }

    let queue = MessageQueue::from_store(
        Arc::new(FailingReceiveStore::default()),
        "thread-x".to_owned(),
    );
    let entry = queue.enqueue(prompt("keep me")).await.unwrap().entry;
    queue
        .send_inputs_within(vec![entry.id], std::time::Duration::ZERO)
        .await
        .unwrap();
    let permit = queue
        .owner()
        .begin_receive(MqReceivePurpose::ModelLoop, execution());
    let items = queue.owner().select_for_receive(&permit).await.unwrap();
    assert!(queue.owner().receive(permit, items).await.is_err());
    assert_eq!(queue.take_send_selection(), vec![entry.id]);
    assert_eq!(queue.len().await.unwrap(), 1);
}

/// [回归] 移交已提交后，唤醒检查失败不能把成功变成错误：历史与 MQ 都已结算，
/// 调用方看不到 `Ok` 就会丢弃这次移交（Transcript 镜像与交付事件都不执行）。
#[tokio::test]
async fn post_receive_query_failure_keeps_the_committed_outcome() {
    #[derive(Default)]
    struct FailingAfterReceiveStore {
        inner: InMemoryMqStore,
        received: std::sync::atomic::AtomicBool,
    }

    #[async_trait]
    impl ThreadMqStore for FailingAfterReceiveStore {
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
            if self.received.load(std::sync::atomic::Ordering::Acquire) {
                return Err(SessionResourceError::new(
                    SessionResourceErrorKind::Unavailable {
                        detail: "injected post-receive query failure".to_owned(),
                    },
                ));
            }
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
            thread_id: &ThreadId,
            selection: MqReceiveSelection,
        ) -> SessionResourceResult<MqReceiveOutcome> {
            let outcome = self.inner.mq_receive(thread_id, selection).await;
            self.received
                .store(true, std::sync::atomic::Ordering::Release);
            outcome
        }

        async fn mq_clear(&self, thread_id: &ThreadId) -> SessionResourceResult<()> {
            self.inner.mq_clear(thread_id).await
        }

        async fn mq_history_contains(
            &self,
            thread_id: &ThreadId,
            id: MessageId,
        ) -> SessionResourceResult<bool> {
            self.inner.mq_history_contains(thread_id, id).await
        }
    }

    let queue = MessageQueue::from_store(
        Arc::new(FailingAfterReceiveStore::default()),
        "thread-x".to_owned(),
    );
    let entry = queue
        .enqueue(prompt("must stay committed"))
        .await
        .unwrap()
        .entry;
    queue
        .send_inputs_within(vec![entry.id], std::time::Duration::ZERO)
        .await
        .unwrap();
    let permit = queue
        .owner()
        .begin_receive(MqReceivePurpose::ModelLoop, execution());
    let items = queue.owner().select_for_receive(&permit).await.unwrap();
    let outcome = queue
        .owner()
        .receive(permit, items)
        .await
        .expect("已提交的移交必须返回成功结果");
    assert_eq!(outcome.items.len(), 1);
    assert_eq!(outcome.items[0].status, MqReceiveStatus::Committed);
    assert!(
        queue.take_send_selection().is_empty(),
        "已结算的选择必须清空"
    );
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

    let permit = queue
        .owner()
        .begin_receive(MqReceivePurpose::ModelLoop, execution());
    let items = queue.owner().select_for_receive(&permit).await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].disposition, MqReceiveDisposition::Suppress);
    let outcome = queue.owner().receive(permit, items).await.unwrap();
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
    queue
        .send_inputs_within(vec![entry.id], std::time::Duration::ZERO)
        .await
        .unwrap();
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

// ─── 发送结果契约：接纳 / 移交 / 历史 / 不存在 / 未知 ─────────────────────────

/// 无 Receive 消费时超时只报告「已接纳未移交」，不得伪称已移交。
#[tokio::test]
async fn send_inputs_timeout_reports_accepted_without_claiming_handoff() {
    let queue = MessageQueue::new();
    let entry = queue.enqueue(prompt("still queued")).await.unwrap().entry;

    let sent = queue
        .send_inputs_within(vec![entry.id], std::time::Duration::from_millis(20))
        .await
        .unwrap();

    assert_eq!(sent.status_of(&entry.id), Some(MqSendStatus::Accepted));
    assert_eq!(sent.accepted().len(), 1);
    assert_eq!(sent.accepted()[0].id, entry.id);
    assert!(sent.missing().is_empty());
    assert_eq!(queue.take_send_selection(), vec![entry.id]);
}

/// 已提交发送选择后，确认查询失败不得把整次调用变成 `Err`。
#[tokio::test]
async fn send_inputs_within_query_failure_keeps_accepted() {
    #[derive(Default)]
    struct FailingQueryStore {
        inner: InMemoryMqStore,
        queries: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl ThreadMqStore for FailingQueryStore {
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
            // 第一次查询属于接纳；之后的确认查询注入失败。
            if self
                .queries
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                > 0
            {
                return Err(SessionResourceError::new(
                    SessionResourceErrorKind::Unavailable {
                        detail: "injected confirmation query failure".into(),
                    },
                ));
            }
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
            thread_id: &ThreadId,
            selection: MqReceiveSelection,
        ) -> SessionResourceResult<MqReceiveOutcome> {
            self.inner.mq_receive(thread_id, selection).await
        }

        async fn mq_clear(&self, thread_id: &ThreadId) -> SessionResourceResult<()> {
            self.inner.mq_clear(thread_id).await
        }

        async fn mq_history_contains(
            &self,
            thread_id: &ThreadId,
            id: MessageId,
        ) -> SessionResourceResult<bool> {
            self.inner.mq_history_contains(thread_id, id).await
        }
    }

    let queue = MessageQueue::from_store(Arc::new(FailingQueryStore::default()), "thread-q".into());
    let entry = queue.enqueue(prompt("accepted")).await.unwrap().entry;

    let sent = queue
        .send_inputs_within(vec![entry.id], std::time::Duration::from_millis(20))
        .await
        .expect("确认查询失败不能把已接纳的发送变成错误");
    assert_eq!(sent.status_of(&entry.id), Some(MqSendStatus::Accepted));
    assert_eq!(queue.take_send_selection(), vec![entry.id]);
}

/// 已移交的确认必须来自存储：MQ 行消失且历史有该 ID 才是 `HandedOff`。
#[tokio::test]
async fn send_inputs_confirms_handoff_from_mq_and_history() {
    let queue = Arc::new(MessageQueue::new());
    let entry = queue.enqueue(prompt("handoff")).await.unwrap().entry;

    let worker = tokio::spawn({
        let queue = Arc::clone(&queue);
        async move {
            for _ in 0..200 {
                let owner = queue.owner();
                let permit = owner.begin_receive(MqReceivePurpose::ModelLoop, execution());
                let items = owner.select_for_receive(&permit).await.unwrap();
                if !items.is_empty() {
                    return owner.receive(permit, items).await.unwrap();
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            panic!("send selection never became visible to Receive");
        }
    });

    let sent = queue
        .send_inputs_within(vec![entry.id], std::time::Duration::from_secs(2))
        .await
        .unwrap();
    worker.await.expect("receive worker failed");

    assert_eq!(sent.status_of(&entry.id), Some(MqSendStatus::HandedOff));
    assert_eq!(sent.accepted().len(), 1, "移交后仍保留接纳时的行投影");
    let owner = queue.owner();
    let permit = owner.begin_receive(MqReceivePurpose::ModelLoop, execution());
    assert!(
        owner.select_for_receive(&permit).await.unwrap().is_empty(),
        "移交后 MQ 不得再出现该行"
    );
    assert!(queue.snapshot().await.unwrap().is_empty());
}

/// 发送前历史已有该 ID：不重复接纳，也不制造第二次投递。
#[tokio::test]
async fn send_inputs_reports_already_in_history_without_accepting() {
    let queue = MessageQueue::new();
    let entry = queue
        .enqueue(prompt("delivered earlier"))
        .await
        .unwrap()
        .entry;
    queue
        .send_inputs_within(vec![entry.id], std::time::Duration::ZERO)
        .await
        .unwrap();
    {
        let owner = queue.owner();
        let permit = owner.begin_receive(MqReceivePurpose::ModelLoop, execution());
        let items = owner.select_for_receive(&permit).await.unwrap();
        owner.receive(permit, items).await.unwrap();
    }

    let sent = queue
        .send_inputs_within(vec![entry.id], std::time::Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(
        sent.status_of(&entry.id),
        Some(MqSendStatus::AlreadyInHistory)
    );
    assert!(sent.accepted().is_empty());
    assert!(queue.take_send_selection().is_empty());
}

/// 历史与 MQ 都没有该 ID：如实报告不存在。
#[tokio::test]
async fn send_inputs_reports_not_found_for_unknown_id() {
    let queue = MessageQueue::new();
    let unknown = MessageId::new();
    let sent = queue
        .send_inputs_within(vec![unknown], std::time::Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(sent.status_of(&unknown), Some(MqSendStatus::NotFound));
    assert_eq!(sent.missing(), vec![unknown]);
}

/// 历史查询无法回答时报告 `Unknown`，不得退化成「不存在」。
#[tokio::test]
async fn send_inputs_reports_unknown_when_history_lookup_is_unavailable() {
    #[derive(Default)]
    struct NoHistoryStore {
        inner: InMemoryMqStore,
    }

    #[async_trait]
    impl ThreadMqStore for NoHistoryStore {
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
            thread_id: &ThreadId,
            selection: MqReceiveSelection,
        ) -> SessionResourceResult<MqReceiveOutcome> {
            self.inner.mq_receive(thread_id, selection).await
        }

        async fn mq_clear(&self, thread_id: &ThreadId) -> SessionResourceResult<()> {
            self.inner.mq_clear(thread_id).await
        }

        async fn mq_history_contains(
            &self,
            _thread_id: &ThreadId,
            _id: MessageId,
        ) -> SessionResourceResult<bool> {
            Err(SessionResourceError::new(
                SessionResourceErrorKind::Unsupported,
            ))
        }
    }

    let queue = MessageQueue::from_store(Arc::new(NoHistoryStore::default()), "thread-x".into());
    let unknown = MessageId::new();
    let sent = queue
        .send_inputs_within(vec![unknown], std::time::Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(sent.status_of(&unknown), Some(MqSendStatus::Unknown));
    assert!(sent.missing().is_empty(), "Unknown 不是确定的不存在");
    assert_eq!(sent.unconfirmed(), vec![unknown]);
}

// ─── 操作权限边界：生产者视图 / 生命周期 owner 视图 ──────────────────────────

/// [接口层] 生产者只能拿到 `ProducerQueue`（入队、查询、发送选择）；
/// `begin_receive` / `select_for_receive` / `receive` / `clear` 只存在于
/// `QueueOwner`，不能从生产者视图直接调用。`MessageQueue::owner()` 仍是同一
/// mailbox 上的显式升级（owner 本来就能拿到 `MessageQueue`），不构成编译期边界。
#[tokio::test]
async fn producer_view_exposes_enqueue_and_reads_but_not_lifecycle_capabilities() {
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(Arc::clone(&queue));
    let handle = inbox.handle();

    let producer: ProducerQueue<'_> = handle.queue();
    let entry = producer
        .enqueue(prompt("via producer"))
        .await
        .unwrap()
        .entry;
    let sent = producer
        .send_inputs_within(vec![entry.id], std::time::Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(sent.status_of(&entry.id), Some(MqSendStatus::Accepted));
    assert_eq!(producer.len().await.unwrap(), 1);

    // 生命周期 owner：同一 mailbox 上的消费与清空。
    let owner: QueueOwner<'_> = queue.owner();
    let permit = owner.begin_receive(MqReceivePurpose::ModelLoop, execution());
    let items = owner.select_for_receive(&permit).await.unwrap();
    assert_eq!(items.len(), 1);
    owner.receive(permit, items).await.unwrap();
    assert!(producer.is_empty().await.unwrap());

    // 两种视图不可互相替换：类型签名本身就是能力边界。
    fn takes_owner(_: QueueOwner<'_>) {}
    fn takes_producer(_: ProducerQueue<'_>) {}
    takes_owner(queue.owner());
    takes_producer(handle.queue());
}
