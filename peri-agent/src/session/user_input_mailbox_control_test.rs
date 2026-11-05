use super::*;

#[test]
fn test_mailbox_takeback_restores_full_multimodal_input_and_blocks_dispatch() {
    let (mailbox, inbox) = make_mailbox();
    mailbox
        .attach_external_attempt_sync(CancellationToken::new(), false)
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
    mailbox.enqueue_sync(&input).unwrap();
    let takeback = TakeBackUserInputRequest {
        session_id: "session".into(),
        generation: mailbox.generation().into(),
        command_id: "takeback".into(),
        input_id: input.input_id.clone(),
    };
    let restored = mailbox
        .take_back_sync(&takeback)
        .unwrap()
        .taken_back
        .unwrap();
    let sent = mailbox
        .dispatch_sync(&make_dispatch(&mailbox, &[&input]))
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
    assert!(
        inbox.queue_for_test().is_empty_sync(),
        "已取回内容不进入消费路径"
    );
}

#[test]
fn test_mailbox_dispatch_takeback_race_has_one_winner() {
    let (mailbox, _) = make_mailbox();
    mailbox
        .attach_external_attempt_sync(CancellationToken::new(), false)
        .unwrap();
    let input = make_input(&mailbox, "并发竞争");
    mailbox.enqueue_sync(&input).unwrap();
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
        left_mailbox.dispatch_sync(&dispatch).unwrap()
    });
    barrier.wait();
    let recalled = mailbox.take_back_sync(&takeback).unwrap();
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
    mailbox.enqueue_sync(&input).unwrap();
    let ticket = mailbox.reserve_run().unwrap();
    let cancel = CancellationToken::new();
    mailbox.attach_attempt_sync(&ticket, cancel.clone());
    inbox.handle().push_defer_sync(
        MessageSource::SubAgentComplete,
        BaseMessage::human("后台结果"),
    );
    mailbox.stop_sync();
    mailbox.finish_attempt_sync(&ticket, UserInputAttemptOutcome::Interrupted);
    mailbox.enqueue_sync(&input).unwrap();
    assert!(cancel.is_cancelled(), "Stop 取消实际 token");
    assert_eq!(
        mailbox.snapshot_sync().items[0].state,
        UserInputState::Queued,
        "未领取条目恢复可取回"
    );
    assert!(mailbox.reserve_run().is_none(), "旧命令重试不应恢复执行");
    let remaining = inbox.queue_for_test().drain_all_sync();
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
        .attach_external_attempt_sync(CancellationToken::new(), false)
        .unwrap();
    let input = make_input(&mailbox, "旧发送重试");
    mailbox.enqueue_sync(&input).unwrap();
    let command = make_dispatch(&mailbox, &[&input]);
    let accepted = mailbox.dispatch_sync(&command).unwrap();
    mailbox.stop_sync();
    mailbox.finish_attempt_sync(&current, UserInputAttemptOutcome::Interrupted);
    let retried = mailbox.dispatch_sync(&command).unwrap();
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
    assert!(
        inbox.queue_for_test().is_empty_sync(),
        "重试不能再次交接消息"
    );
}

#[test]
fn test_mailbox_stop_cannot_reclaim_between_drain_and_claim_receipt() {
    let (mailbox, inbox) = make_mailbox();
    let input = make_input(&mailbox, "已领取输入");
    mailbox.enqueue_sync(&input).unwrap();
    let ticket = mailbox.reserve_run().unwrap();
    mailbox.attach_attempt_sync(&ticket, CancellationToken::new());
    let received = inbox.queue_for_test().drain_all_sync();
    let ids = [received[0].message().unwrap().id()];
    mailbox.mark_claimed_sync(&ids);
    mailbox.stop_sync();
    assert!(
        mailbox.snapshot_sync().items.is_empty(),
        "已从 MQ 领取的输入不再显示为待发"
    );
    assert_eq!(
        mailbox.mark_delivered_sync(&ids),
        [input.input_id],
        "后续接纳只能发生一次"
    );
    assert!(
        mailbox.mark_delivered_sync(&ids).is_empty(),
        "重复 receipt 不能重复聊天消息"
    );
    assert!(
        mailbox.snapshot_sync().items.is_empty(),
        "接纳后移除待发投影"
    );
}

#[test]
fn test_mailbox_stop_invalidates_reserved_ticket_without_affecting_next_attempt() {
    let (mailbox, inbox) = make_mailbox();
    let input = make_input(&mailbox, "旧预留");
    mailbox.enqueue_sync(&input).unwrap();
    let old = mailbox.reserve_run().unwrap();
    mailbox.stop_sync();
    mailbox
        .dispatch_sync(&make_dispatch(&mailbox, &[&input]))
        .unwrap();
    let next = mailbox.reserve_run().unwrap();
    assert!(
        !mailbox.attach_attempt_sync(&old, CancellationToken::new()),
        "迟到旧预留不能执行"
    );
    mailbox.finish_attempt_sync(&old, UserInputAttemptOutcome::Failed);
    assert!(
        mailbox.attach_attempt_sync(&next, CancellationToken::new()),
        "旧终态不能取消新执行"
    );
    assert_eq!(inbox.queue_for_test().len_sync(), 1, "新执行只交接一次");
}

#[test]
fn test_mailbox_late_fail_reserved_keeps_next_attempt_runnable() {
    let (mailbox, _) = make_mailbox();
    let input = make_input(&mailbox, "旧预留");
    mailbox.enqueue_sync(&input).unwrap();
    let old = mailbox.reserve_run().unwrap();
    mailbox.stop_sync();
    mailbox
        .dispatch_sync(&make_dispatch(&mailbox, &[&input]))
        .unwrap();
    let next = mailbox.reserve_run().unwrap();

    // 旧 turn 的外层 guard 此时才 Drop：它已不是当前执行，不得复位队列。
    mailbox.fail_reserved(&old);

    assert!(
        mailbox.is_active_ticket(&next),
        "过期清理不能移除新预留的执行身份"
    );
    assert!(
        mailbox.attach_attempt_sync(&next, CancellationToken::new()),
        "过期清理暂停队列会让新输入永远无法启动"
    );
}

#[test]
fn test_mailbox_stale_stop_cannot_cancel_new_managed_attempt() {
    let (mailbox, _) = make_mailbox();
    let input = make_input(&mailbox, "延迟停止");
    mailbox.enqueue_sync(&input).unwrap();
    let old = mailbox.reserve_run().unwrap();
    mailbox.attach_attempt_sync(&old, CancellationToken::new());
    assert!(
        mailbox.stop_attempt_sync(&old.id, mailbox.generation()),
        "当前身份可取消"
    );
    mailbox.finish_attempt_sync(&old, UserInputAttemptOutcome::Interrupted);
    mailbox
        .dispatch_sync(&make_dispatch(&mailbox, &[&input]))
        .unwrap();
    let next = mailbox.reserve_run().unwrap();
    let cancel = CancellationToken::new();
    mailbox.attach_attempt_sync(&next, cancel.clone());
    assert!(
        !mailbox.stop_attempt_sync(&old.id, mailbox.generation()),
        "旧 ticket 取消不得命中新执行"
    );
    assert!(
        !mailbox.stop_attempt_sync(&next.id, "old-generation"),
        "旧会话实例不得命中当前 ticket"
    );
    assert!(!cancel.is_cancelled(), "新执行 token 必须保留");
    assert_eq!(mailbox.active_run_ticket(), Some(next), "新执行仍持有准入");
}

#[test]
fn test_mailbox_recorded_failure_overrides_host_success_and_keepgoing_resumes() {
    let (mailbox, inbox) = make_mailbox();
    let current = mailbox
        .attach_external_attempt_sync(CancellationToken::new(), false)
        .unwrap();
    let input = make_input(&mailbox, "失败后保留");
    mailbox.enqueue_sync(&input).unwrap();
    mailbox.record_attempt_outcome(&current, UserInputAttemptOutcome::Failed);
    mailbox.finish_attempt_sync(&current, UserInputAttemptOutcome::Completed);
    assert!(
        mailbox.reserve_run().is_none(),
        "宿主成功不能覆盖 Agent 权威失败"
    );
    assert_eq!(
        mailbox.snapshot_sync().items[0].state,
        UserInputState::Queued,
        "失败后保留待发"
    );
    mailbox
        .attach_external_attempt_sync(CancellationToken::new(), true)
        .unwrap();
    assert_eq!(
        inbox.queue_for_test().len_sync(),
        1,
        "显式 keepgoing 才恢复普通待办"
    );
}

#[test]
fn test_mailbox_generation_and_command_conflicts_are_rejected() {
    let (mailbox, _) = make_mailbox();
    let input = make_input(&mailbox, "原正文");
    mailbox.enqueue_sync(&input).unwrap();
    let mut conflicting = input.clone();
    conflicting.content = MessageContent::text("新正文");
    assert_eq!(
        mailbox.enqueue_sync(&conflicting).unwrap_err(),
        UserInputQueueError::IdentityConflict,
        "命令身份不允许换载荷"
    );
    conflicting.command_id = "different-command".into();
    assert_eq!(
        mailbox.enqueue_sync(&conflicting).unwrap_err(),
        UserInputQueueError::IdentityConflict,
        "输入身份也不允许换载荷"
    );
    conflicting.generation = "old-generation".into();
    assert_eq!(
        mailbox.enqueue_sync(&conflicting).unwrap_err(),
        UserInputQueueError::StaleSession,
        "旧会话实例必须拒绝"
    );
    mailbox.invalidate();
    assert_eq!(
        mailbox.enqueue_sync(&input).unwrap_err(),
        UserInputQueueError::Closed,
        "已关闭 owner 不接受旧回执重试"
    );
}

#[test]
fn test_mailbox_capacity_rejects_without_evicting_and_deduplicates_input_id() {
    let (mailbox, _) = make_mailbox();
    mailbox
        .attach_external_attempt_sync(CancellationToken::new(), false)
        .unwrap();
    let first = make_input(&mailbox, "最早输入");
    mailbox.enqueue_sync(&first).unwrap();
    let mut duplicate = first.clone();
    duplicate.command_id = "retry-another-command".into();
    mailbox.enqueue_sync(&duplicate).unwrap();
    for _ in 1..PENDING_CAPACITY {
        mailbox
            .enqueue_sync(&make_input(&mailbox, "后续输入"))
            .unwrap();
    }
    let rejected = mailbox
        .enqueue_sync(&make_input(&mailbox, "超容量"))
        .unwrap_err();
    assert_eq!(rejected, UserInputQueueError::Capacity, "容量满应拒绝");
    assert_eq!(
        mailbox.snapshot_sync().items.len(),
        PENDING_CAPACITY,
        "输入 ID 去重不能额外占位"
    );
    assert_eq!(
        mailbox.snapshot_sync().items[0].input_id,
        first.input_id,
        "不能静默丢弃最早条目"
    );
}

#[test]
fn test_mailbox_empty_dispatch_does_not_interrupt_and_empty_content_is_rejected() {
    let (mailbox, _) = make_mailbox();
    let cancel = CancellationToken::new();
    mailbox
        .attach_external_attempt_sync(cancel.clone(), false)
        .unwrap();
    mailbox
        .dispatch_sync(&make_dispatch(&mailbox, &[]))
        .unwrap();
    assert!(!cancel.is_cancelled(), "空集合不能额外中断");
    assert_eq!(
        mailbox.enqueue_sync(&make_input(&mailbox, "")).unwrap_err(),
        UserInputQueueError::EmptyContent,
        "keepgoing 不能成为待发送条目"
    );
}

#[tokio::test]
async fn test_mailbox_suspended_input_handoff_wakes_same_attempt() {
    let (mailbox, inbox) = make_mailbox();
    let ticket = mailbox
        .attach_external_attempt_sync(CancellationToken::new(), false)
        .unwrap();
    let waiting = make_input(&mailbox, "之前普通待办");
    mailbox.enqueue_sync(&waiting).unwrap();
    mailbox.enter_idle_sync();
    mailbox
        .enqueue_sync(&make_input(&mailbox, "挂起时新输入"))
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), inbox.await_wake())
        .await
        .expect("已有待办必须唤醒同一 inbox");
    let received = inbox.queue_for_test().drain_all_sync();
    assert_eq!(received.len(), 1, "进入 idle 自动交接第一条已有待办");
    assert_eq!(
        received[0].message().unwrap().id().as_uuid().to_string(),
        waiting.input_id,
        "后来输入不能越过旧待办"
    );
    let received_id = received[0].message().unwrap().id();
    mailbox.mark_claimed_sync(&[received_id]);
    mailbox.mark_delivered_sync(&[received_id]);
    assert_eq!(
        mailbox.active_run_ticket(),
        Some(ticket),
        "挂起输入不能同时启动第二次执行"
    );
    assert_eq!(
        mailbox.snapshot_sync().items[0].state,
        UserInputState::Queued,
        "新输入等待下一次 idle"
    );
}

/// [回归测试] loading 期间排队的输入应在进入 idle 时逐条交接，跳过已撤回项。
#[test]
fn test_mailbox_idle_transition_dispatches_first_queued_input() {
    let (mailbox, inbox) = make_mailbox();
    mailbox
        .attach_external_attempt_sync(CancellationToken::new(), false)
        .unwrap();
    let withdrawn = make_input(&mailbox, "已撤回");
    let first = make_input(&mailbox, "第一条");
    let second = make_input(&mailbox, "第二条");
    for input in [&withdrawn, &first, &second] {
        mailbox.enqueue_sync(input).unwrap();
    }
    mailbox
        .take_back_sync(&TakeBackUserInputRequest {
            session_id: "session".into(),
            generation: mailbox.generation().into(),
            command_id: "withdraw-first".into(),
            input_id: withdrawn.input_id,
        })
        .unwrap();
    mailbox.enter_idle_sync();
    let delivered = inbox.queue_for_test().drain_all_sync();
    assert_eq!(delivered.len(), 1, "进入 idle 应只交接第一条可执行输入");
    let id = delivered[0].message().unwrap().id();
    assert_eq!(id.as_uuid().to_string(), first.input_id);
    mailbox.mark_claimed_sync(&[id]);
    mailbox.mark_delivered_sync(&[id]);
    assert_eq!(
        mailbox.snapshot_sync().items[0].state,
        UserInputState::Queued
    );
    mailbox.mark_claimed_sync(&[id]);
    mailbox.mark_delivered_sync(&[id]);
    mailbox.enter_idle_sync();
    let delivered = inbox.queue_for_test().drain_all_sync();
    assert_eq!(delivered.len(), 1, "下一次 idle 才交接下一条");
    assert_eq!(
        delivered[0].message().unwrap().id().as_uuid().to_string(),
        second.input_id
    );
}

/// [回归测试] idle 唤醒后、Receive 尚未开始前的连续提交不能合并为同一批。
#[test]
fn test_mailbox_suspended_burst_hands_off_only_one_input() {
    let (mailbox, inbox) = make_mailbox();
    mailbox
        .attach_external_attempt_sync(CancellationToken::new(), false)
        .unwrap();
    mailbox.enter_idle_sync();
    let first = make_input(&mailbox, "唤醒输入");
    let second = make_input(&mailbox, "继续排队");
    mailbox.enqueue_sync(&first).unwrap();
    let receipt = mailbox.enqueue_sync(&second).unwrap();
    assert_eq!(receipt.results[0].state, UserInputState::Queued);
    let delivered = inbox.queue_for_test().drain_all_sync();
    assert_eq!(delivered.len(), 1, "一次 idle 只能自动交接一条");
    assert_eq!(
        delivered[0].message().unwrap().id().as_uuid().to_string(),
        first.input_id
    );
}

/// [回归测试] 外部取消与 idle 边界交错时，不向已取消的 attempt 交接输入。
#[test]
fn test_mailbox_cancelled_attempt_does_not_dispatch_on_idle() {
    let (mailbox, inbox) = make_mailbox();
    let cancel = CancellationToken::new();
    mailbox
        .attach_external_attempt_sync(cancel.clone(), false)
        .unwrap();
    mailbox
        .enqueue_sync(&make_input(&mailbox, "取消后保留"))
        .unwrap();
    cancel.cancel();
    mailbox.enter_idle_sync();
    assert!(
        inbox.queue_for_test().is_empty_sync(),
        "取消后的旧执行不能领取待办"
    );
    assert_eq!(
        mailbox.snapshot_sync().items[0].state,
        UserInputState::Queued
    );
}

#[test]
fn test_mailbox_steer_then_late_idle_preserves_selected_continuation() {
    let (mailbox, inbox) = make_mailbox();
    let cancel = CancellationToken::new();
    let current = mailbox
        .attach_external_attempt_sync(cancel.clone(), false)
        .unwrap();
    let waiting = make_input(&mailbox, "A 普通待办");
    let selected = make_input(&mailbox, "B 立即发送");
    mailbox.enqueue_sync(&waiting).unwrap();
    mailbox.enqueue_sync(&selected).unwrap();
    mailbox
        .dispatch_sync(&make_dispatch(&mailbox, &[&selected]))
        .unwrap();
    mailbox.enter_idle_sync();
    assert!(cancel.is_cancelled(), "立即发送先请求旧执行收尾");

    let later = make_input(&mailbox, "C 收尾期间的新待办");
    let receipt = mailbox.enqueue_sync(&later).unwrap();
    assert_eq!(
        receipt.results[0].state,
        UserInputState::Queued,
        "新输入不能加入已选 B 的批次"
    );
    assert!(
        mailbox
            .snapshot_sync()
            .items
            .iter()
            .find(|item| item.input_id == selected.input_id)
            .is_some_and(|item| item.state == UserInputState::Dispatching),
        "旧挂起执行已取消，所选输入仍在等待新执行"
    );

    mailbox.finish_attempt_sync(&current, UserInputAttemptOutcome::Interrupted);
    let continuation = mailbox.reserve_run().expect("B 仍须保留可启动的续跑");
    assert!(mailbox.attach_attempt_sync(&continuation, CancellationToken::new()));
    let delivered = inbox.queue_for_test().drain_all_sync();
    assert_eq!(delivered.len(), 1, "新执行只领取所选 B");
    assert_eq!(
        delivered[0].message().unwrap().id().as_uuid().to_string(),
        selected.input_id
    );
    // 真实 Receive 在写入历史后立即标记领取；纯投影不再靠 MQ 查询隐藏已领取项。
    mailbox.mark_claimed_sync(&[delivered[0].message().unwrap().id()]);
    let pending = mailbox.snapshot_sync().items;
    assert_eq!(
        pending[0].state,
        UserInputState::Queued,
        "A 等待新执行 idle"
    );
    assert_eq!(
        pending[1].state,
        UserInputState::Queued,
        "C 等待新执行 idle"
    );
}

#[test]
fn test_mailbox_stop_then_late_suspend_waits_for_fresh_attempt() {
    let (mailbox, inbox) = make_mailbox();
    let current = mailbox
        .attach_external_attempt_sync(CancellationToken::new(), false)
        .unwrap();
    let waiting = make_input(&mailbox, "停止前待办");
    mailbox.enqueue_sync(&waiting).unwrap();
    mailbox.stop_sync();
    // 旧 loop 可能已越过取消检查，迟到报告挂起。
    mailbox.enter_idle_sync();
    assert!(
        inbox.queue_for_test().is_empty_sync(),
        "迟到 idle 不能绕过 Stop"
    );
    assert_eq!(
        mailbox.snapshot_sync().items[0].state,
        UserInputState::Queued
    );
    let resumed = make_input(&mailbox, "用户明确重新提交");
    mailbox.enqueue_sync(&resumed).unwrap();
    assert!(
        inbox.queue_for_test().is_empty_sync(),
        "新输入不能唤醒已被 Stop 的旧执行"
    );

    mailbox.finish_attempt_sync(&current, UserInputAttemptOutcome::Interrupted);
    let next = mailbox.reserve_run().expect("明确恢复必须保留新执行的准入");
    assert!(mailbox.attach_attempt_sync(&next, CancellationToken::new()));
    let delivered = inbox.queue_for_test().drain_all_sync();
    assert_eq!(delivered.len(), 1, "显式恢复只交接第一条原待办");
    assert_eq!(
        delivered[0].message().unwrap().id().as_uuid().to_string(),
        waiting.input_id
    );
    let delivered_id = delivered[0].message().unwrap().id();
    mailbox.mark_claimed_sync(&[delivered_id]);
    mailbox.mark_delivered_sync(&[delivered_id]);
    assert_eq!(
        mailbox.snapshot_sync().items[0].input_id,
        resumed.input_id,
        "新输入保留在原待办之后"
    );
    assert_eq!(
        mailbox.snapshot_sync().items[0].state,
        UserInputState::Queued
    );
}

/// [回归测试] Stop 完成后，新的后台 continuation 仍继承 paused；新输入必须
/// 解除暂停并交接给这个仍有效、未取消的当前执行。
#[test]
fn test_mailbox_new_input_wakes_background_attempt_after_finished_stop() {
    let (mailbox, inbox) = make_mailbox();
    let old = mailbox
        .attach_external_attempt_sync(CancellationToken::new(), false)
        .unwrap();
    mailbox.stop_sync();
    mailbox.finish_attempt_sync(&old, UserInputAttemptOutcome::Interrupted);

    let background = mailbox
        .attach_external_attempt_sync(CancellationToken::new(), false)
        .unwrap();
    mailbox.enter_idle_sync();

    let input = make_input(&mailbox, "停止后重新发送");
    mailbox.enqueue_sync(&input).unwrap();

    assert_eq!(mailbox.active_run_ticket(), Some(background));
    assert!(
        mailbox.reserve_run().is_none(),
        "已有执行时不能启动第二个执行"
    );
    let received = inbox.queue_for_test().drain_all_sync();
    assert_eq!(received.len(), 1, "应交接给当前未取消的后台执行");
    assert_eq!(
        received[0].message().unwrap().id().as_uuid().to_string(),
        input.input_id
    );
}

/// [回归测试] 输入先于新后台执行的 idle 边界完成 promotion；enter_idle 必须
/// 交接已存在的 ready，而不是要求本次调用重新 promote。
#[test]
fn test_mailbox_ready_input_is_handed_off_when_external_attempt_enters_idle() {
    let (mailbox, inbox) = make_mailbox();
    let input = make_input(&mailbox, "已经进入 ready");
    mailbox.enqueue_sync(&input).unwrap();

    let background = mailbox
        .attach_external_attempt_sync(CancellationToken::new(), false)
        .unwrap();

    assert!(mailbox.reserve_run().is_none());
    mailbox.enter_idle_sync();

    assert_eq!(mailbox.active_run_ticket(), Some(background));
    let received = inbox.queue_for_test().drain_all_sync();
    assert_eq!(received.len(), 1, "已有 ready 不能因未再次 promote 被跳过");
    assert_eq!(
        received[0].message().unwrap().id().as_uuid().to_string(),
        input.input_id
    );
}

/// [回归测试] enqueue 先完成 promotion、随后后台执行才 attach 的竞争：
/// reserve 被拒后，现有执行进入 idle 仍要消费已 ready 的输入。
#[test]
fn test_mailbox_ready_input_survives_attach_before_idle() {
    let (mailbox, inbox) = make_mailbox();
    let old = mailbox
        .attach_external_attempt_sync(CancellationToken::new(), false)
        .unwrap();
    mailbox.stop_sync();
    mailbox.finish_attempt_sync(&old, UserInputAttemptOutcome::Interrupted);

    let input = make_input(&mailbox, "attach 前已 ready");
    mailbox.enqueue_sync(&input).unwrap();
    let background = mailbox
        .attach_external_attempt_sync(CancellationToken::new(), false)
        .unwrap();

    assert!(mailbox.reserve_run().is_none(), "当前执行仍持有准入");
    mailbox.enter_idle_sync();

    assert_eq!(mailbox.active_run_ticket(), Some(background));
    let received = inbox.queue_for_test().drain_all_sync();
    assert_eq!(received.len(), 1);
    assert_eq!(
        received[0].message().unwrap().id().as_uuid().to_string(),
        input.input_id
    );
}
