use super::*;
use crate::session::test_resources::mock::MockSessionResources;
use peri_acp_types::identity::{AttemptId, SessionSeq};
use peri_acp_types::session::{ExecutionBinding, MessageQueue, MqReceivePurpose};

#[path = "user_input_mailbox_terminal_test.rs"]
mod terminal_tests;

#[path = "user_input_mailbox_control_test.rs"]
mod control_tests;

fn make_mailbox() -> (Arc<UserInputMailbox>, Arc<SessionInbox>) {
    let inbox = Arc::new(SessionInbox::new(Arc::new(MessageQueue::new())));
    (
        UserInputMailbox::new("session".into(), inbox.clone(), Arc::new(|_| {})),
        inbox,
    )
}

fn make_input(mailbox: &UserInputMailbox, text: &str) -> EnqueueUserInputRequest {
    EnqueueUserInputRequest {
        session_id: "session".into(),
        generation: mailbox.generation().into(),
        command_id: uuid::Uuid::now_v7().to_string(),
        input_id: uuid::Uuid::now_v7().to_string(),
        content: MessageContent::text(text),
        original_draft: text.into(),
    }
}

fn make_dispatch(
    mailbox: &UserInputMailbox,
    inputs: &[&EnqueueUserInputRequest],
) -> DispatchUserInputsRequest {
    DispatchUserInputsRequest {
        session_id: "session".into(),
        generation: mailbox.generation().into(),
        command_id: uuid::Uuid::now_v7().to_string(),
        input_ids: inputs.iter().map(|input| input.input_id.clone()).collect(),
    }
}

/// 真实 SessionResources 替身绑定的 mailbox：可注入历史查询失败。
fn make_mock_mailbox(
    resources: Arc<MockSessionResources>,
) -> (Arc<UserInputMailbox>, MessageQueue) {
    let queue = MessageQueue::bind(resources, "thread-x".into());
    let inbox = Arc::new(SessionInbox::new(Arc::new(queue.clone())));
    (
        UserInputMailbox::new("session".into(), inbox, Arc::new(|_| {})),
        queue,
    )
}

fn record_state(mailbox: &UserInputMailbox, input_id: &str) -> Option<(UserInputState, bool)> {
    mailbox
        .state
        .lock()
        .records
        .iter()
        .find(|record| record.input_id == input_id)
        .map(|record| (record.state, record.handed_off))
}

#[test]
fn test_mailbox_running_enqueue_never_wakes_consumption_queue() {
    let (mailbox, inbox) = make_mailbox();
    mailbox
        .attach_external_attempt_sync(CancellationToken::new(), false)
        .unwrap();
    let input = make_input(&mailbox, "等待下一次 idle");
    let receipt = mailbox.enqueue_sync(&input).unwrap();
    assert_eq!(
        receipt.results[0].state,
        UserInputState::Queued,
        "运行中输入应保持待发"
    );
    assert!(
        !inbox.queue_for_test().has_wake_up_sync(),
        "普通待办不参与 MQ 唤醒"
    );
    assert!(
        !inbox.queue_for_test().needs_mq_continuation_sync(),
        "普通待办不触发后台空转"
    );
}

#[test]
fn test_mailbox_idle_input_handoff_is_once_and_keeps_message_identity() {
    let (mailbox, inbox) = make_mailbox();
    let input = make_input(&mailbox, "空闲时立即处理");
    mailbox.enqueue_sync(&input).unwrap();
    let second = make_input(&mailbox, "预留前的后续输入");
    let receipt = mailbox.enqueue_sync(&second).unwrap();
    assert_eq!(
        receipt.results[0].state,
        UserInputState::Queued,
        "尚未 reserve 也不能批量释放第二条普通输入"
    );
    let ticket = mailbox.reserve_run().unwrap();
    assert!(
        mailbox.snapshot_sync().active_request_id.is_none(),
        "预留阶段还没有客户端执行身份"
    );
    assert!(mailbox.reserve_run().is_none(), "执行预留只能成功一次");
    assert!(
        inbox.queue_for_test().is_empty_sync(),
        "取得运行锁前不能交接"
    );
    assert!(
        mailbox.attach_attempt_sync(&ticket, CancellationToken::new()),
        "预留身份应允许执行"
    );
    assert_eq!(
        mailbox.snapshot_sync().active_request_id.as_deref(),
        Some(ticket.id.as_str()),
        "恢复会话时能取得真实执行身份"
    );
    assert!(
        !mailbox.attach_attempt_sync(&ticket, CancellationToken::new()),
        "相同 ticket 不能启动两次"
    );
    let messages = inbox.queue_for_test().drain_all_sync();
    assert_eq!(messages.len(), 1, "相同输入只交接一次");
    assert_eq!(
        messages[0].message().unwrap().id().as_uuid().to_string(),
        input.input_id,
        "canonical 消息保留准入身份"
    );
}

#[test]
fn test_mailbox_failed_attached_attempt_recovers_unclaimed_and_clears_active_identity() {
    let (mailbox, inbox) = make_mailbox();
    let input = make_input(&mailbox, "装配失败");
    mailbox.enqueue_sync(&input).unwrap();
    let ticket = mailbox.reserve_run().unwrap();
    let cancel = CancellationToken::new();
    mailbox.attach_attempt_sync(&ticket, cancel.clone());
    assert!(
        mailbox.run_started_event(&ticket).is_some(),
        "已 attach 身份可建立客户端 lease"
    );
    mailbox.fail_reserved(&ticket);
    assert!(cancel.is_cancelled(), "早退清理取消真实 token");
    assert!(
        inbox.queue_for_test().is_empty_sync(),
        "实际未领取项必须从 MQ 撤出"
    );
    assert_eq!(
        mailbox.snapshot_sync().items[0].state,
        UserInputState::Queued,
        "早退保留可编辑待办"
    );
    assert!(
        mailbox.snapshot_sync().active_request_id.is_none(),
        "早退清理执行投影"
    );
    assert!(
        mailbox.run_started_event(&ticket).is_none(),
        "旧 ticket 不能重新建立客户端 lease"
    );
}

#[test]
fn test_mailbox_single_dispatch_preserves_ordinary_queue_until_natural_completion() {
    let (mailbox, inbox) = make_mailbox();
    let cancel = CancellationToken::new();
    let first = mailbox
        .attach_external_attempt_sync(cancel.clone(), false)
        .unwrap();
    let a = make_input(&mailbox, "A");
    let b = make_input(&mailbox, "B");
    let c = make_input(&mailbox, "C");
    for input in [&a, &b, &c] {
        mailbox.enqueue_sync(input).unwrap();
    }
    mailbox
        .dispatch_sync(&make_dispatch(&mailbox, &[&b]))
        .unwrap();
    assert!(cancel.is_cancelled(), "单发请求中断真实执行");
    assert_eq!(
        mailbox
            .snapshot_sync()
            .items
            .iter()
            .find(|item| item.input_id == b.input_id)
            .map(|item| item.state),
        Some(UserInputState::Dispatching),
        "发送选择已提交，但 Receive 尚未领取"
    );
    mailbox.finish_attempt_sync(&first, UserInputAttemptOutcome::Interrupted);
    let next = mailbox.reserve_run().unwrap();
    mailbox.attach_attempt_sync(&next, CancellationToken::new());
    let received = inbox.queue_for_test().drain_all_sync();
    assert_eq!(
        received
            .iter()
            .map(|message| message.message().unwrap().content())
            .collect::<Vec<_>>(),
        ["B"],
        "单发只交接选中 B"
    );
    let ids: Vec<_> = received
        .iter()
        .map(|message| message.message().unwrap().id())
        .collect();
    mailbox.mark_claimed_sync(&ids);
    mailbox.mark_delivered_sync(&ids);
    assert_eq!(
        mailbox
            .snapshot_sync()
            .items
            .iter()
            .map(|item| item.state)
            .collect::<Vec<_>>(),
        [UserInputState::Queued, UserInputState::Queued],
        "内部中断不能释放 A/C"
    );
    mailbox.record_attempt_outcome(&next, UserInputAttemptOutcome::Completed);
    mailbox.finish_attempt_sync(&next, UserInputAttemptOutcome::Failed);
    let last = mailbox.reserve_run().unwrap();
    mailbox.attach_attempt_sync(&last, CancellationToken::new());
    let received = inbox.queue_for_test().drain_all_sync();
    assert_eq!(
        received
            .iter()
            .map(|message| message.message().unwrap().content())
            .collect::<Vec<_>>(),
        ["A"],
        "自然成功只释放第一条普通待办"
    );
    let id = received[0].message().unwrap().id();
    mailbox.mark_claimed_sync(&[id]);
    mailbox.mark_delivered_sync(&[id]);
    assert_eq!(
        mailbox.snapshot_sync().items[0].state,
        UserInputState::Queued
    );
    mailbox.finish_attempt_sync(&last, UserInputAttemptOutcome::Completed);
    let next = mailbox.reserve_run().unwrap();
    mailbox.attach_attempt_sync(&next, CancellationToken::new());
    let received = inbox.queue_for_test().drain_all_sync();
    assert_eq!(received.len(), 1);
    assert_eq!(
        received[0].message().unwrap().content(),
        "C",
        "A 完成后才自动发送 C"
    );
}

#[test]
fn test_mailbox_dispatch_snapshot_and_consecutive_commands_preserve_acceptance_order() {
    let (mailbox, inbox) = make_mailbox();
    let current = mailbox
        .attach_external_attempt_sync(CancellationToken::new(), false)
        .unwrap();
    let inputs: Vec<_> = ["A", "B", "C", "D"]
        .into_iter()
        .map(|text| make_input(&mailbox, text))
        .collect();
    for input in &inputs[..3] {
        mailbox.enqueue_sync(input).unwrap();
    }
    mailbox
        .dispatch_sync(&make_dispatch(&mailbox, &[&inputs[1]]))
        .unwrap();
    mailbox
        .dispatch_sync(&make_dispatch(
            &mailbox,
            &[&inputs[2], &inputs[0], &inputs[1]],
        ))
        .unwrap();
    mailbox.enqueue_sync(&inputs[3]).unwrap();
    mailbox.finish_attempt_sync(&current, UserInputAttemptOutcome::Interrupted);
    let next = mailbox.reserve_run().unwrap();
    mailbox.attach_attempt_sync(&next, CancellationToken::new());
    assert_eq!(
        inbox
            .queue_for_test()
            .drain_all_sync()
            .iter()
            .map(|message| message.message().unwrap().content())
            .collect::<Vec<_>>(),
        ["A", "B", "C"],
        "Receive 按服务端 (enqueued_at,id) 排序，发送选择不另建排序"
    );
    assert_eq!(
        mailbox.snapshot_sync().items.last().unwrap().state,
        UserInputState::Queued,
        "快照点击后新 D 不加入 ALL"
    );
}

/// [回归测试] Delivered / Withdrawn 是可查询的终态，不能只从 pending 列表消失；
/// 重复确认必须幂等。
#[test]
fn test_mailbox_snapshot_exposes_terminal_states_and_deduplicates_delivery() {
    let (mailbox, inbox) = make_mailbox();
    mailbox
        .attach_external_attempt_sync(CancellationToken::new(), false)
        .unwrap();
    let delivered_input = make_input(&mailbox, "已交付");
    mailbox.enqueue_sync(&delivered_input).unwrap();
    mailbox.enter_idle_sync();
    let messages = inbox.queue_for_test().drain_all_sync();
    let delivered_id = messages[0].message().unwrap().id();
    mailbox.mark_claimed_sync(&[delivered_id]);
    assert_eq!(mailbox.mark_delivered_sync(&[delivered_id]).len(), 1);
    assert!(
        mailbox.mark_delivered_sync(&[delivered_id]).is_empty(),
        "重复交付确认不得重复通知"
    );

    let withdrawn_input = make_input(&mailbox, "已撤回");
    mailbox.enqueue_sync(&withdrawn_input).unwrap();
    mailbox
        .take_back_sync(&TakeBackUserInputRequest {
            session_id: "session".into(),
            generation: mailbox.generation().into(),
            command_id: "withdraw-terminal".into(),
            input_id: withdrawn_input.input_id.clone(),
        })
        .unwrap();

    let snapshot = mailbox.snapshot_sync();
    assert!(snapshot.items.is_empty(), "终态不应继续出现在待发列表");
    assert!(snapshot.terminal_items.iter().any(|item| {
        item.input_id == delivered_input.input_id && item.state == UserInputState::Delivered
    }));
    assert!(snapshot.terminal_items.iter().any(|item| {
        item.input_id == withdrawn_input.input_id && item.state == UserInputState::Withdrawn
    }));
}

/// [回归测试] 交付日志条目在发布时获得事件序号；重复绑定保留首个序号。
#[test]
fn test_bind_delivery_sequence_orders_journal_and_is_idempotent() {
    let (mailbox, inbox) = make_mailbox();
    mailbox
        .attach_external_attempt_sync(CancellationToken::new(), false)
        .unwrap();
    let input = make_input(&mailbox, "带序号交付");
    mailbox.enqueue_sync(&input).unwrap();
    mailbox.enter_idle_sync();
    let messages = inbox.queue_for_test().drain_all_sync();
    let id = messages[0].message().unwrap().id();
    mailbox.mark_claimed_sync(&[id]);
    mailbox.mark_delivered_sync(&[id]);

    let pending = mailbox.pending_delivery_notifications();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].session_seq, None,
        "mark_delivered 时尚未发布事件，序号必须显式为 None"
    );

    let first = SessionSeq::initial().next();
    mailbox.bind_delivery_sequence(&input.input_id, first);
    assert_eq!(
        mailbox.pending_delivery_notifications()[0].session_seq,
        Some(first)
    );

    mailbox.bind_delivery_sequence(&input.input_id, first.next());
    assert_eq!(
        mailbox.pending_delivery_notifications()[0].session_seq,
        Some(first),
        "重复绑定不得覆盖首个序号"
    );
}

/// [回归测试] 终态快照必须有界，避免长会话无限增长。
#[test]
fn test_mailbox_terminal_snapshot_is_bounded() {
    let (mailbox, inbox) = make_mailbox();
    mailbox
        .attach_external_attempt_sync(CancellationToken::new(), false)
        .unwrap();
    for index in 0..(TERMINAL_CAPACITY + 2) {
        let input = make_input(&mailbox, &format!("终态输入 {index}"));
        mailbox.enqueue_sync(&input).unwrap();
        mailbox.enter_idle_sync();
        let messages = inbox.queue_for_test().drain_all_sync();
        assert_eq!(messages.len(), 1);
        let id = messages[0].message().unwrap().id();
        mailbox.mark_claimed_sync(&[id]);
        mailbox.mark_delivered_sync(&[id]);
        // 本测试只验证终态展示有界；模拟事件泵立即确认交付日志。
        mailbox.ack_delivery_notification(&input.input_id);
    }

    let snapshot = mailbox.snapshot_sync();
    assert!(snapshot.items.is_empty());
    assert_eq!(snapshot.terminal_items.len(), TERMINAL_CAPACITY);
    assert!(snapshot
        .terminal_items
        .iter()
        .all(|item| item.state == UserInputState::Delivered));
}

/// [回归] 对账查询进行期间入队：旧查询结果不得清理新记录，快照 revision 必须覆盖其内容。
#[tokio::test]
async fn refresh_keeps_records_enqueued_while_its_mq_query_is_in_flight() {
    let (mailbox, inbox) = make_mailbox();
    mailbox
        .attach_external_attempt_sync(CancellationToken::new(), false)
        .unwrap();
    let (entered, release) = inbox.queue_for_test().gate_next_query();

    let refresh = tokio::spawn({
        let mailbox = mailbox.clone();
        async move { mailbox.refresh_from_store().await }
    });
    entered.await.expect("refresh MQ query must start");
    let input = make_input(&mailbox, "查询期间入队");
    let receipt = mailbox.enqueue(&input).await.unwrap();
    release.send(()).expect("release refresh query");

    refresh.await.unwrap().expect("refresh must succeed");
    let fresh = mailbox.snapshot().await;
    let item = fresh
        .items
        .iter()
        .find(|item| item.input_id == input.input_id)
        .expect("查询期间入队的记录必须保留在投影里");
    assert_eq!(
        item.content,
        MessageContent::text("查询期间入队"),
        "revision 与内容必须取自同一线性化点"
    );
    assert!(
        fresh.revision >= receipt.snapshot.revision,
        "快照 revision 不得低于其内容已包含的入队变更"
    );
}

/// [回归] `MessageQueue::bind()` 必须把查询门转发到 store：否则 `gate_next_query`
/// 在绑定资源的路径上静默失效，调用方会在 `entered` 上一直等待。
#[tokio::test]
async fn bound_queue_forwards_query_gate_to_the_store() {
    let resources = MockSessionResources::new();
    let queue = MessageQueue::bind(resources, "thread-gate".into());
    let (entered, release) = queue.gate_next_query();
    let query = tokio::spawn({
        let queue = queue.clone();
        async move { queue.snapshot().await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), entered)
        .await
        .expect("query gate must reach the bound store")
        .expect("query gate sender dropped");
    release.send(()).expect("release bound query");
    query.await.unwrap().unwrap();
}

/// [回归] 历史查询无法回答时，`Unknown` 不是「不存在」：待发记录必须保留。
#[tokio::test]
async fn unknown_send_result_keeps_the_input_queued() {
    let resources = MockSessionResources::new();
    let (mailbox, queue) = make_mock_mailbox(Arc::clone(&resources));
    mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .await
        .unwrap();
    let input = make_input(&mailbox, "无法确认是否移交");
    mailbox.enqueue(&input).await.unwrap();
    let id = MessageId::from(uuid::Uuid::parse_str(&input.input_id).unwrap());
    assert!(matches!(
        queue.cancel_user_input(&id).await.unwrap(),
        MqCancelOutcome::Cancelled(_)
    ));
    resources.fail_mq_history_lookup();

    let error = mailbox
        .dispatch(&make_dispatch(&mailbox, &[&input]))
        .await
        .expect_err("无法确认移交时必须返回错误");
    assert!(matches!(error, UserInputQueueError::Store(_)));
    let (state, handed_off) = record_state(&mailbox, &input.input_id).expect("记录必须保留");
    assert_eq!(state, UserInputState::Queued);
    assert!(!handed_off, "Unknown 不能宣称已移交");
}

/// [回归] `Unknown` 的错误响应只能读投影：记录保留，恢复 MQ 行后同一 ID 可再次发送。
#[tokio::test]
async fn unknown_error_response_keeps_record_and_retry_dispatches() {
    let resources = MockSessionResources::new();
    let (mailbox, queue) = make_mock_mailbox(Arc::clone(&resources));
    mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .await
        .unwrap();
    let input = make_input(&mailbox, "错误响应后重试");
    mailbox.enqueue(&input).await.unwrap();
    let id = MessageId::from(uuid::Uuid::parse_str(&input.input_id).unwrap());
    assert!(matches!(
        queue.cancel_user_input(&id).await.unwrap(),
        MqCancelOutcome::Cancelled(_)
    ));
    resources.fail_mq_history_lookup();

    mailbox
        .dispatch(&make_dispatch(&mailbox, &[&input]))
        .await
        .expect_err("无法确认移交时必须返回错误");
    let (state, handed_off) = record_state(&mailbox, &input.input_id).expect("记录必须保留");
    assert_eq!(state, UserInputState::Queued);
    assert!(!handed_off, "Unknown 不能宣称已移交");

    // ACP 错误响应只附加纯投影；这里模拟它不得清理记录或推进 revision。
    let revision = mailbox.snapshot().await.revision;
    let projected = mailbox.snapshot().await;
    assert_eq!(projected.revision, revision, "错误响应不得推进 revision");
    assert!(
        projected
            .items
            .iter()
            .any(|item| item.input_id == input.input_id),
        "错误响应不得把 Unknown 记录从投影中删除"
    );

    // 恢复 MQ 行后重试同一 input_id：记录仍在，dispatch 必须能真正发出。
    let message = peri_acp_types::session::QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::Human {
            id,
            content: input.content.clone(),
        },
    )
    .with_original_draft(input.original_draft.clone())
    .with_policy(peri_acp_types::session::MessagePolicy::ensure_processing());
    queue.enqueue(message).await.unwrap();

    let receipt = mailbox
        .dispatch(&make_dispatch(&mailbox, &[&input]))
        .await
        .expect("恢复待发行后重试必须成功");
    assert_eq!(receipt.results[0].state, UserInputState::Dispatching);
    let (state, handed_off) = record_state(&mailbox, &input.input_id).expect("记录仍然存在");
    assert_eq!(state, UserInputState::Dispatching);
    assert!(handed_off, "确认接纳后必须标记已移交");
}

/// [回归] 对账只在查询成功并确认 MQ 行消失时清理 Queued。
#[tokio::test]
async fn refresh_prunes_only_confirmed_mq_absence() {
    let resources = MockSessionResources::new();
    let (mailbox, queue) = make_mock_mailbox(Arc::clone(&resources));
    mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .await
        .unwrap();
    let kept = make_input(&mailbox, "仍在 MQ");
    let removed = make_input(&mailbox, "已从 MQ 消失");
    mailbox.enqueue(&kept).await.unwrap();
    mailbox.enqueue(&removed).await.unwrap();
    let removed_id = MessageId::from(uuid::Uuid::parse_str(&removed.input_id).unwrap());
    assert!(matches!(
        queue.cancel_user_input(&removed_id).await.unwrap(),
        MqCancelOutcome::Cancelled(_)
    ));

    mailbox
        .refresh_from_store()
        .await
        .expect("成功查询必须完成对账");
    let snapshot = mailbox.snapshot().await;
    assert!(
        snapshot
            .items
            .iter()
            .any(|item| item.input_id == kept.input_id),
        "查询结果中的记录必须保留"
    );
    assert!(
        snapshot
            .items
            .iter()
            .all(|item| item.input_id != removed.input_id),
        "成功查询确认不存在的 Queued 才允许清理"
    );
}

/// [回归] 对账查询失败时返回错误且不修改记录；最后一次成功状态保留。
#[tokio::test]
async fn refresh_query_failure_keeps_state_and_returns_error() {
    let resources = MockSessionResources::new();
    let (mailbox, _queue) = make_mock_mailbox(Arc::clone(&resources));
    mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .await
        .unwrap();
    let input = make_input(&mailbox, "查询失败");
    mailbox.enqueue(&input).await.unwrap();
    let before = mailbox.snapshot().await;

    resources.fail_next_mq_queries(1);
    let error = mailbox
        .refresh_from_store()
        .await
        .expect_err("查询失败必须返回错误");
    assert!(matches!(error, UserInputQueueError::Store(_)));
    let after = mailbox.snapshot().await;
    assert_eq!(after.revision, before.revision, "失败不得推进 revision");
    assert!(
        after
            .items
            .iter()
            .any(|item| item.input_id == input.input_id),
        "失败不得删除最后已知记录"
    );

    mailbox
        .refresh_from_store()
        .await
        .expect("恢复后的查询必须成功");
    assert!(
        mailbox
            .snapshot()
            .await
            .items
            .iter()
            .any(|item| item.input_id == input.input_id),
        "成功查询仍能看到 MQ 中的记录"
    );
}

/// [回归] `send_ready_ids` 不得把 `Unknown` 记录标成已移交。
#[tokio::test]
async fn send_ready_ids_does_not_claim_handoff_for_unknown() {
    let resources = MockSessionResources::new();
    let (mailbox, queue) = make_mock_mailbox(Arc::clone(&resources));
    mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .await
        .unwrap();
    let input = make_input(&mailbox, "idle 无法确认");
    mailbox.enqueue(&input).await.unwrap();
    let id = MessageId::from(uuid::Uuid::parse_str(&input.input_id).unwrap());
    assert!(matches!(
        queue.cancel_user_input(&id).await.unwrap(),
        MqCancelOutcome::Cancelled(_)
    ));
    resources.fail_mq_history_lookup();

    mailbox.enter_idle().await;

    let (state, handed_off) = record_state(&mailbox, &input.input_id).expect("记录必须保留");
    assert_eq!(state, UserInputState::Dispatching);
    assert!(!handed_off, "Unknown 不是移交结论");
    assert!(
        mailbox.state.lock().ready.contains(&input.input_id),
        "Unknown 必须保留重试资格"
    );
}

/// 空闲会话在 dispatch 的有界窗口内完成真实 Receive 时，本次调用应确认移交。
#[tokio::test]
async fn dispatch_confirms_handoff_on_an_idle_session() {
    let queue = Arc::new(MessageQueue::new());
    let inbox = Arc::new(SessionInbox::new(Arc::clone(&queue)));
    let mailbox = UserInputMailbox::new("session".into(), inbox, Arc::new(|_| {}));
    let input = make_input(&mailbox, "空闲确认");
    mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .await
        .unwrap();
    mailbox.enqueue(&input).await.unwrap();
    mailbox.stop().await;

    // 真实 Receive：选中后停在提交门，由独立任务放行，模拟空闲会话的移交。
    let (entered, release) = queue.gate_next_receive();
    let worker_queue = Arc::clone(&queue);
    let worker = tokio::spawn(async move {
        loop {
            let owner = worker_queue.owner();
            let permit = owner.begin_receive(
                MqReceivePurpose::ModelLoop,
                ExecutionBinding {
                    turn_id: TurnId::new(),
                    attempt_id: AttemptId::new(),
                },
            );
            let items = owner.select_for_receive(&permit).await.unwrap();
            if !items.is_empty() {
                return owner.receive(permit, items).await.unwrap();
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    });
    let releaser = tokio::spawn(async move {
        entered.await.expect("receive must reach the commit gate");
        tokio::time::sleep(Duration::from_millis(5)).await;
        release.send(()).expect("release receive gate");
    });

    let receipt = mailbox
        .dispatch(&make_dispatch(&mailbox, &[&input]))
        .await
        .unwrap();
    assert_eq!(receipt.results[0].state, UserInputState::Dispatching);
    assert!(
        queue.snapshot().await.unwrap().is_empty(),
        "dispatch 返回时必须已完成移交"
    );
    assert!(
        record_state(&mailbox, &input.input_id).unwrap().1,
        "确认移交后记录必须标记 handed_off"
    );

    tokio::time::timeout(Duration::from_secs(2), worker)
        .await
        .expect("receive worker must finish")
        .expect("receive worker panicked");
    releaser.await.unwrap();
}
