//! MessageQueue 持久门面语义：入队即事实、发送选择、原子移交、水位与唤醒。

use crate::identity::AttemptId;
use crate::messages::{BaseMessage, MessageId};
use crate::session::message_queue::message_queue::MessageQueue;
use crate::session::message_queue::queued_message::MessageKind;
use crate::session::message_queue::queued_message::MessageSource;
use crate::session::message_queue::queued_message::QueuedMessage;
#[cfg(test)]
use crate::session::mq::MqEnqueueOutcome;
#[cfg(test)]
use crate::session::mq::MqEntry;
#[cfg(test)]
use crate::session::mq::MqFilter;
#[cfg(test)]
use crate::session::mq::MqInput;
#[cfg(test)]
use crate::session::mq::MqReceiveDisposition;
#[cfg(test)]
use crate::session::mq::MqReceiveOutcome;
#[cfg(test)]
use crate::session::mq::MqReceiveSelection;
#[cfg(test)]
use crate::session::mq::ThreadMqStore;
use crate::session::mq_memory::InMemoryMqStore;
#[cfg(test)]
use crate::session::ExecutionBinding;
#[cfg(test)]
use crate::session::MessagePolicy;
use crate::session::SessionInbox;
use crate::session::{
    MqCancelOutcome, MqEnqueueDisposition, MqReceivePurpose, MqReceiveStatus, MqSendStatus, TurnId,
};
use crate::session::{ProducerQueue, QueueClosed, QueueOwner, QueuedPayload};
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
#[cfg(test)]
use std::sync::Arc;

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

include!("queue_mq_test_part2_test.rs");
