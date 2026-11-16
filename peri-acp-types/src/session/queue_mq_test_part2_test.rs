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
        .has_ensure_processing_after(Some(&watermark))
        .await
        .unwrap());
    assert!(!queue.has_ensure_processing_after(None).await.unwrap());

    let defer = queue
        .enqueue(QueuedMessage::defer(
            MessageSource::SubAgentComplete,
            BaseMessage::human("done"),
        ))
        .await
        .unwrap()
        .entry;
    assert!(queue
        .has_ensure_processing_after(Some(&watermark))
        .await
        .unwrap());
    assert_eq!(
        queue.admission_watermark().await.unwrap(),
        Some(defer.watermark())
    );
    assert!(queue.has_ensure_processing_after(None).await.unwrap());
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

// ─── 关闭闸门 ────────────────────────────────────────────────────────────────

/// 关闸后单发与批量入队都明确拒绝，且不写存储；clone 共享同一闸门。
#[tokio::test]
async fn close_admission_rejects_single_and_batch_enqueue_without_writing() {
    let queue = MessageQueue::new();
    queue.enqueue(prompt("before close")).await.unwrap();

    let clone = queue.clone();
    clone.close_admission();
    assert!(queue.is_admission_closed(), "闸门必须被 clone 共享");

    let error = queue.enqueue(prompt("late")).await.unwrap_err();
    assert!(
        error.downcast_ref::<QueueClosed>().is_some(),
        "关闭后必须返回可识别错误: {error}"
    );
    let error = queue
        .enqueue_batch(vec![prompt("late-1"), prompt("late-2")])
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<QueueClosed>().is_some());

    assert_eq!(queue.len().await.unwrap(), 1, "只有关闸前的消息被写入");
}

/// 关闸不是丢弃在途写入：close 等计数归零之后才取得稳定快照。
#[tokio::test]
async fn close_admission_waits_for_in_flight_enqueue() {
    let queue = MessageQueue::new();
    let pending = queue.begin_detached_enqueue();
    queue.close_admission();

    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let waiting = {
        let queue = queue.clone();
        tokio::spawn(async move {
            let _ = entered_tx.send(());
            queue.await_pending_enqueues().await;
        })
    };
    entered_rx.await.unwrap();
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished(), "在途写入结束前不得放行关闭");

    drop(pending);
    waiting.await.unwrap();
}

/// 闸门只属于门面，不写入存储：同一 thread 的新 bind（resume / 新 owner）
/// 重新开放入队。
#[tokio::test]
async fn a_new_bind_for_the_same_thread_admits_enqueue_after_close() {
    let resources = Arc::new(FakeMqResources::default());
    let thread: ThreadId = "thread-reopen".into();
    let closed = MessageQueue::bind(resources.clone(), thread.clone());
    closed.enqueue(prompt("before close")).await.unwrap();
    closed.close_admission();
    assert!(closed.enqueue(prompt("late")).await.is_err());

    let reopened = MessageQueue::bind(resources, thread);
    assert!(!reopened.is_admission_closed());
    let outcome = reopened.enqueue(prompt("resume")).await.unwrap();
    assert_eq!(outcome.disposition, MqEnqueueDisposition::Inserted);
}
