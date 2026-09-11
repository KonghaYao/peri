use super::*;
use peri_acp_types::session::MessageQueue;

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

#[test]
fn test_mailbox_running_enqueue_never_wakes_consumption_queue() {
    let (mailbox, inbox) = make_mailbox();
    mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let input = make_input(&mailbox, "等待自然完成");
    let receipt = mailbox.enqueue(&input).unwrap();
    assert_eq!(
        receipt.results[0].state,
        UserInputState::Queued,
        "运行中输入应保持待发"
    );
    assert!(!inbox.queue().has_wake_up(), "普通待办不参与 MQ 唤醒");
    assert!(
        !inbox.queue().needs_mq_continuation(),
        "普通待办不触发后台空转"
    );
}

#[test]
fn test_mailbox_idle_input_handoff_is_once_and_keeps_message_identity() {
    let (mailbox, inbox) = make_mailbox();
    let input = make_input(&mailbox, "空闲时立即处理");
    mailbox.enqueue(&input).unwrap();
    let ticket = mailbox.reserve_run().unwrap();
    assert!(
        mailbox.snapshot().active_request_id.is_none(),
        "预留阶段还没有客户端执行身份"
    );
    assert!(mailbox.reserve_run().is_none(), "执行预留只能成功一次");
    assert!(inbox.queue().is_empty(), "取得运行锁前不能交接");
    assert!(
        mailbox.attach_attempt(&ticket, CancellationToken::new()),
        "预留身份应允许执行"
    );
    assert_eq!(
        mailbox.snapshot().active_request_id.as_deref(),
        Some(ticket.id.as_str()),
        "恢复会话时能取得真实执行身份"
    );
    assert!(
        !mailbox.attach_attempt(&ticket, CancellationToken::new()),
        "相同 ticket 不能启动两次"
    );
    let messages = inbox.queue().drain_all();
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
    mailbox.enqueue(&input).unwrap();
    let ticket = mailbox.reserve_run().unwrap();
    let cancel = CancellationToken::new();
    mailbox.attach_attempt(&ticket, cancel.clone());
    assert!(
        mailbox.run_started_event(&ticket).is_some(),
        "已 attach 身份可建立客户端 lease"
    );
    mailbox.fail_reserved(&ticket);
    assert!(cancel.is_cancelled(), "早退清理取消真实 token");
    assert!(inbox.queue().is_empty(), "实际未领取项必须从 MQ 撤出");
    assert_eq!(
        mailbox.snapshot().items[0].state,
        UserInputState::Queued,
        "早退保留可编辑待办"
    );
    assert!(
        mailbox.snapshot().active_request_id.is_none(),
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
        .attach_external_attempt(cancel.clone(), false)
        .unwrap();
    let a = make_input(&mailbox, "A");
    let b = make_input(&mailbox, "B");
    let c = make_input(&mailbox, "C");
    for input in [&a, &b, &c] {
        mailbox.enqueue(input).unwrap();
    }
    mailbox.dispatch(&make_dispatch(&mailbox, &[&b])).unwrap();
    assert!(cancel.is_cancelled(), "单发请求中断真实执行");
    assert!(inbox.queue().is_empty(), "中断收尾期间不能提前交接");
    mailbox.finish_attempt(&first, UserInputAttemptOutcome::Interrupted);
    let next = mailbox.reserve_run().unwrap();
    mailbox.attach_attempt(&next, CancellationToken::new());
    let received = inbox.queue().drain_all();
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
    mailbox.mark_claimed(&ids);
    mailbox.mark_delivered(&ids);
    assert_eq!(
        mailbox
            .snapshot()
            .items
            .iter()
            .map(|item| item.state)
            .collect::<Vec<_>>(),
        [UserInputState::Queued, UserInputState::Queued],
        "内部中断不能释放 A/C"
    );
    mailbox.record_attempt_outcome(&next, UserInputAttemptOutcome::Completed);
    mailbox.finish_attempt(&next, UserInputAttemptOutcome::Failed);
    let last = mailbox.reserve_run().unwrap();
    mailbox.attach_attempt(&last, CancellationToken::new());
    assert_eq!(
        inbox
            .queue()
            .drain_all()
            .iter()
            .map(|message| message.message().unwrap().content())
            .collect::<Vec<_>>(),
        ["A", "C"],
        "自然成功按原顺序释放普通待办"
    );
}

#[test]
fn test_mailbox_dispatch_snapshot_and_consecutive_commands_preserve_acceptance_order() {
    let (mailbox, inbox) = make_mailbox();
    let current = mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let inputs: Vec<_> = ["A", "B", "C", "D"]
        .into_iter()
        .map(|text| make_input(&mailbox, text))
        .collect();
    for input in &inputs[..3] {
        mailbox.enqueue(input).unwrap();
    }
    mailbox
        .dispatch(&make_dispatch(&mailbox, &[&inputs[1]]))
        .unwrap();
    mailbox
        .dispatch(&make_dispatch(
            &mailbox,
            &[&inputs[2], &inputs[0], &inputs[1]],
        ))
        .unwrap();
    mailbox.enqueue(&inputs[3]).unwrap();
    mailbox.finish_attempt(&current, UserInputAttemptOutcome::Interrupted);
    let next = mailbox.reserve_run().unwrap();
    mailbox.attach_attempt(&next, CancellationToken::new());
    assert_eq!(
        inbox
            .queue()
            .drain_all()
            .iter()
            .map(|message| message.message().unwrap().content())
            .collect::<Vec<_>>(),
        ["B", "A", "C"],
        "跨命令按接受顺序、单批按服务端入队顺序"
    );
    assert_eq!(
        mailbox.snapshot().items.last().unwrap().state,
        UserInputState::Queued,
        "快照点击后新 D 不加入 ALL"
    );
}

#[test]
fn test_mailbox_takeback_restores_full_multimodal_input_and_blocks_dispatch() {
    let (mailbox, inbox) = make_mailbox();
    mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let mut input = make_input(&mailbox, "附件正文\n第二行");
    input.content = MessageContent::Blocks(vec![
        peri_acp_types::messages::ContentBlock::Text {
            text: "附件正文\n第二行".into(),
        },
        peri_acp_types::messages::ContentBlock::Image {
            source: peri_acp_types::messages::ImageSource::Base64 {
                media_type: "image/png".into(),
                data: "AQID".into(),
            },
        },
    ]);
    input.original_draft = "@image /tmp/图.png\n附件正文\n第二行".into();
    mailbox.enqueue(&input).unwrap();
    let takeback = TakeBackUserInputRequest {
        session_id: "session".into(),
        generation: mailbox.generation().into(),
        command_id: "takeback".into(),
        input_id: input.input_id.clone(),
    };
    let restored = mailbox.take_back(&takeback).unwrap().taken_back.unwrap();
    let sent = mailbox
        .dispatch(&make_dispatch(&mailbox, &[&input]))
        .unwrap();
    assert_eq!(
        restored.original_draft, input.original_draft,
        "完整草稿必须保留"
    );
    assert_eq!(
        serde_json::to_value(restored.content).unwrap(),
        serde_json::to_value(input.content).unwrap(),
        "附件完整载荷必须保留"
    );
    assert_eq!(
        sent.results[0].state,
        UserInputState::Withdrawn,
        "取回后不能重新发送旧输入"
    );
    assert!(inbox.queue().is_empty(), "已取回内容不进入消费路径");
}

#[test]
fn test_mailbox_dispatch_takeback_race_has_one_winner() {
    let (mailbox, _) = make_mailbox();
    mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let input = make_input(&mailbox, "并发竞争");
    mailbox.enqueue(&input).unwrap();
    let dispatch = make_dispatch(&mailbox, &[&input]);
    let takeback = TakeBackUserInputRequest {
        session_id: "session".into(),
        generation: mailbox.generation().into(),
        command_id: "takeback".into(),
        input_id: input.input_id.clone(),
    };
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let left_mailbox = mailbox.clone();
    let left_barrier = barrier.clone();
    let left = std::thread::spawn(move || {
        left_barrier.wait();
        left_mailbox.dispatch(&dispatch).unwrap()
    });
    barrier.wait();
    let recalled = mailbox.take_back(&takeback).unwrap();
    let sent = left.join().unwrap();
    assert_ne!(
        sent.results[0].state == UserInputState::Dispatching,
        recalled.taken_back.is_some(),
        "发送与取回只能一方成功"
    );
}

#[test]
fn test_mailbox_stop_reclaims_only_unclaimed_and_old_command_cannot_resend() {
    let (mailbox, inbox) = make_mailbox();
    let input = make_input(&mailbox, "可回收输入");
    mailbox.enqueue(&input).unwrap();
    let ticket = mailbox.reserve_run().unwrap();
    let cancel = CancellationToken::new();
    mailbox.attach_attempt(&ticket, cancel.clone());
    inbox.handle().push_defer(
        MessageSource::SubAgentComplete,
        BaseMessage::human("后台结果"),
    );
    mailbox.stop();
    mailbox.finish_attempt(&ticket, UserInputAttemptOutcome::Interrupted);
    mailbox.enqueue(&input).unwrap();
    assert!(cancel.is_cancelled(), "Stop 取消实际 token");
    assert_eq!(
        mailbox.snapshot().items[0].state,
        UserInputState::Queued,
        "未领取条目恢复可取回"
    );
    assert!(mailbox.reserve_run().is_none(), "旧命令重试不应恢复执行");
    let remaining = inbox.queue().drain_all();
    assert_eq!(remaining.len(), 1, "撤回不能清除后台消息");
    assert_eq!(
        remaining[0].source,
        MessageSource::SubAgentComplete,
        "来源必须保持"
    );
}

#[test]
fn test_mailbox_replayed_dispatch_after_stop_keeps_original_receipt_without_new_execution() {
    let (mailbox, inbox) = make_mailbox();
    let current = mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let input = make_input(&mailbox, "旧发送重试");
    mailbox.enqueue(&input).unwrap();
    let command = make_dispatch(&mailbox, &[&input]);
    let accepted = mailbox.dispatch(&command).unwrap();
    mailbox.stop();
    mailbox.finish_attempt(&current, UserInputAttemptOutcome::Interrupted);
    let retried = mailbox.dispatch(&command).unwrap();
    assert_eq!(
        retried.results[0].state, accepted.results[0].state,
        "重试返回最初接受回执"
    );
    assert_eq!(
        retried.snapshot.items[0].state,
        UserInputState::Queued,
        "最新快照保留停止后的真实状态"
    );
    assert!(
        mailbox.reserve_run().is_none(),
        "旧 dispatch 重试不能重新启动"
    );
    assert!(inbox.queue().is_empty(), "重试不能再次交接消息");
}

#[test]
fn test_mailbox_stop_cannot_reclaim_between_drain_and_claim_receipt() {
    let (mailbox, inbox) = make_mailbox();
    let input = make_input(&mailbox, "已领取输入");
    mailbox.enqueue(&input).unwrap();
    let ticket = mailbox.reserve_run().unwrap();
    mailbox.attach_attempt(&ticket, CancellationToken::new());
    let received = inbox.queue().drain_all();
    let ids = [received[0].message().unwrap().id()];
    mailbox.stop();
    assert_eq!(
        mailbox.snapshot().items[0].state,
        UserInputState::Dispatching,
        "领取回执尚未写回也不能恢复 queued"
    );
    mailbox.mark_claimed(&ids);
    assert_eq!(
        mailbox.mark_delivered(&ids),
        [input.input_id],
        "后续接纳只能发生一次"
    );
    assert!(
        mailbox.mark_delivered(&ids).is_empty(),
        "重复 receipt 不能重复聊天消息"
    );
    assert!(mailbox.snapshot().items.is_empty(), "接纳后移除待发投影");
}

#[test]
fn test_mailbox_stop_invalidates_reserved_ticket_without_affecting_next_attempt() {
    let (mailbox, inbox) = make_mailbox();
    let input = make_input(&mailbox, "旧预留");
    mailbox.enqueue(&input).unwrap();
    let old = mailbox.reserve_run().unwrap();
    mailbox.stop();
    mailbox
        .dispatch(&make_dispatch(&mailbox, &[&input]))
        .unwrap();
    let next = mailbox.reserve_run().unwrap();
    assert!(
        !mailbox.attach_attempt(&old, CancellationToken::new()),
        "迟到旧预留不能执行"
    );
    mailbox.finish_attempt(&old, UserInputAttemptOutcome::Failed);
    assert!(
        mailbox.attach_attempt(&next, CancellationToken::new()),
        "旧终态不能取消新执行"
    );
    assert_eq!(inbox.queue().len(), 1, "新执行只交接一次");
}

#[test]
fn test_mailbox_stale_stop_cannot_cancel_new_managed_attempt() {
    let (mailbox, _) = make_mailbox();
    let input = make_input(&mailbox, "延迟停止");
    mailbox.enqueue(&input).unwrap();
    let old = mailbox.reserve_run().unwrap();
    mailbox.attach_attempt(&old, CancellationToken::new());
    assert!(
        mailbox.stop_attempt(&old.id, mailbox.generation()),
        "当前身份可取消"
    );
    mailbox.finish_attempt(&old, UserInputAttemptOutcome::Interrupted);
    mailbox
        .dispatch(&make_dispatch(&mailbox, &[&input]))
        .unwrap();
    let next = mailbox.reserve_run().unwrap();
    let cancel = CancellationToken::new();
    mailbox.attach_attempt(&next, cancel.clone());
    assert!(
        !mailbox.stop_attempt(&old.id, mailbox.generation()),
        "旧 ticket 取消不得命中新执行"
    );
    assert!(
        !mailbox.stop_attempt(&next.id, "old-generation"),
        "旧会话实例不得命中当前 ticket"
    );
    assert!(!cancel.is_cancelled(), "新执行 token 必须保留");
    assert_eq!(mailbox.active_run_ticket(), Some(next), "新执行仍持有准入");
}

#[test]
fn test_mailbox_recorded_failure_overrides_host_success_and_keepgoing_resumes() {
    let (mailbox, inbox) = make_mailbox();
    let current = mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let input = make_input(&mailbox, "失败后保留");
    mailbox.enqueue(&input).unwrap();
    mailbox.record_attempt_outcome(&current, UserInputAttemptOutcome::Failed);
    mailbox.finish_attempt(&current, UserInputAttemptOutcome::Completed);
    assert!(
        mailbox.reserve_run().is_none(),
        "宿主成功不能覆盖 Agent 权威失败"
    );
    assert_eq!(
        mailbox.snapshot().items[0].state,
        UserInputState::Queued,
        "失败后保留待发"
    );
    mailbox
        .attach_external_attempt(CancellationToken::new(), true)
        .unwrap();
    assert_eq!(inbox.queue().len(), 1, "显式 keepgoing 才恢复普通待办");
}

#[test]
fn test_mailbox_generation_and_command_conflicts_are_rejected() {
    let (mailbox, _) = make_mailbox();
    let input = make_input(&mailbox, "原正文");
    mailbox.enqueue(&input).unwrap();
    let mut conflicting = input.clone();
    conflicting.content = MessageContent::text("新正文");
    assert_eq!(
        mailbox.enqueue(&conflicting).unwrap_err(),
        UserInputQueueError::IdentityConflict,
        "命令身份不允许换载荷"
    );
    conflicting.command_id = "different-command".into();
    assert_eq!(
        mailbox.enqueue(&conflicting).unwrap_err(),
        UserInputQueueError::IdentityConflict,
        "输入身份也不允许换载荷"
    );
    conflicting.generation = "old-generation".into();
    assert_eq!(
        mailbox.enqueue(&conflicting).unwrap_err(),
        UserInputQueueError::StaleSession,
        "旧会话实例必须拒绝"
    );
    mailbox.invalidate();
    assert_eq!(
        mailbox.enqueue(&input).unwrap_err(),
        UserInputQueueError::Closed,
        "已关闭 owner 不接受旧回执重试"
    );
}

#[test]
fn test_mailbox_capacity_rejects_without_evicting_and_deduplicates_input_id() {
    let (mailbox, _) = make_mailbox();
    mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let first = make_input(&mailbox, "最早输入");
    mailbox.enqueue(&first).unwrap();
    let mut duplicate = first.clone();
    duplicate.command_id = "retry-another-command".into();
    mailbox.enqueue(&duplicate).unwrap();
    for _ in 1..PENDING_CAPACITY {
        mailbox.enqueue(&make_input(&mailbox, "后续输入")).unwrap();
    }
    let rejected = mailbox
        .enqueue(&make_input(&mailbox, "超容量"))
        .unwrap_err();
    assert_eq!(rejected, UserInputQueueError::Capacity, "容量满应拒绝");
    assert_eq!(
        mailbox.snapshot().items.len(),
        PENDING_CAPACITY,
        "输入 ID 去重不能额外占位"
    );
    assert_eq!(
        mailbox.snapshot().items[0].input_id,
        first.input_id,
        "不能静默丢弃最早条目"
    );
}

#[test]
fn test_mailbox_empty_dispatch_does_not_interrupt_and_empty_content_is_rejected() {
    let (mailbox, _) = make_mailbox();
    let cancel = CancellationToken::new();
    mailbox
        .attach_external_attempt(cancel.clone(), false)
        .unwrap();
    mailbox.dispatch(&make_dispatch(&mailbox, &[])).unwrap();
    assert!(!cancel.is_cancelled(), "空集合不能额外中断");
    assert_eq!(
        mailbox.enqueue(&make_input(&mailbox, "")).unwrap_err(),
        UserInputQueueError::EmptyContent,
        "keepgoing 不能成为待发送条目"
    );
}

#[tokio::test]
async fn test_mailbox_suspended_input_handoff_wakes_same_attempt() {
    let (mailbox, inbox) = make_mailbox();
    let ticket = mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let waiting = make_input(&mailbox, "之前普通待办");
    mailbox.enqueue(&waiting).unwrap();
    mailbox.set_suspended(true);
    mailbox
        .enqueue(&make_input(&mailbox, "挂起时新输入"))
        .unwrap();
    inbox.await_wake().await;
    assert_eq!(inbox.queue().len(), 1, "挂起时新输入使用同一 inbox 唤醒");
    assert_eq!(
        mailbox.active_run_ticket(),
        Some(ticket),
        "挂起输入不能同时启动第二次执行"
    );
    assert_eq!(
        mailbox.snapshot().items[0].state,
        UserInputState::Queued,
        "之前普通待办不能被一并释放"
    );
}

#[test]
fn test_mailbox_suspended_steer_then_enqueue_preserves_selected_continuation() {
    let (mailbox, inbox) = make_mailbox();
    let cancel = CancellationToken::new();
    let current = mailbox
        .attach_external_attempt(cancel.clone(), false)
        .unwrap();
    let waiting = make_input(&mailbox, "A 普通待办");
    let selected = make_input(&mailbox, "B 立即发送");
    mailbox.enqueue(&waiting).unwrap();
    mailbox.enqueue(&selected).unwrap();
    mailbox.set_suspended(true);
    mailbox
        .dispatch(&make_dispatch(&mailbox, &[&selected]))
        .unwrap();
    assert!(cancel.is_cancelled(), "立即发送先请求旧执行收尾");

    let later = make_input(&mailbox, "C 收尾期间的新待办");
    let receipt = mailbox.enqueue(&later).unwrap();
    assert_eq!(
        receipt.results[0].state,
        UserInputState::Queued,
        "新输入不能加入已选 B 的批次"
    );
    assert!(
        inbox.queue().is_empty(),
        "旧挂起执行已取消，不能提前向其交接"
    );

    mailbox.finish_attempt(&current, UserInputAttemptOutcome::Interrupted);
    let continuation = mailbox.reserve_run().expect("B 仍须保留可启动的续跑");
    assert!(mailbox.attach_attempt(&continuation, CancellationToken::new()));
    let delivered = inbox.queue().drain_all();
    assert_eq!(delivered.len(), 1, "新执行只领取所选 B");
    assert_eq!(
        delivered[0].message().unwrap().id().as_uuid().to_string(),
        selected.input_id
    );
    let pending = mailbox.snapshot().items;
    assert_eq!(
        pending[0].state,
        UserInputState::Queued,
        "A 继续等待自然完成"
    );
    assert_eq!(
        pending[2].state,
        UserInputState::Queued,
        "C 继续等待自然完成"
    );
}

#[test]
fn test_mailbox_stop_then_late_suspend_waits_for_fresh_attempt() {
    let (mailbox, inbox) = make_mailbox();
    let current = mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let waiting = make_input(&mailbox, "停止前待办");
    mailbox.enqueue(&waiting).unwrap();
    mailbox.stop();
    // 旧 loop 可能已越过取消检查，迟到报告挂起。
    mailbox.set_suspended(true);
    let resumed = make_input(&mailbox, "用户明确重新提交");
    mailbox.enqueue(&resumed).unwrap();
    assert!(inbox.queue().is_empty(), "新输入不能唤醒已被 Stop 的旧执行");

    mailbox.finish_attempt(&current, UserInputAttemptOutcome::Interrupted);
    let next = mailbox.reserve_run().expect("明确恢复必须保留新执行的准入");
    assert!(mailbox.attach_attempt(&next, CancellationToken::new()));
    let delivered = inbox.queue().drain_all();
    assert_eq!(delivered.len(), 2, "显式恢复按顺序交接原待办及新输入");
    assert_eq!(
        delivered[0].message().unwrap().id().as_uuid().to_string(),
        waiting.input_id
    );
    assert_eq!(
        delivered[1].message().unwrap().id().as_uuid().to_string(),
        resumed.input_id
    );
}
