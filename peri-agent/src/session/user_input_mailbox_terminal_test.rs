use super::*;

fn withdraw(mailbox: &UserInputMailbox, input: &EnqueueUserInputRequest) {
    mailbox
        .take_back(&TakeBackUserInputRequest {
            session_id: input.session_id.clone(),
            generation: input.generation.clone(),
            command_id: uuid::Uuid::now_v7().to_string(),
            input_id: input.input_id.clone(),
        })
        .unwrap();
}

fn fill_withdrawn(mailbox: &UserInputMailbox) {
    for _ in 0..TERMINAL_CAPACITY {
        let input = make_input(mailbox, "撤回填充");
        mailbox.enqueue(&input).unwrap();
        withdraw(mailbox, &input);
    }
}

#[test]
fn overflow_preserves_delivered_and_withdrawn_identity() {
    for terminal in [UserInputState::Delivered, UserInputState::Withdrawn] {
        let (mailbox, inbox) = make_mailbox();
        mailbox
            .attach_external_attempt(CancellationToken::new(), false)
            .unwrap();
        let mut input = make_input(&mailbox, "只执行一次");
        mailbox.enqueue(&input).unwrap();
        if terminal == UserInputState::Delivered {
            mailbox.enter_idle();
            let messages = inbox.queue().drain_all();
            let id = messages[0].message().unwrap().id();
            mailbox.mark_claimed(&[id]);
            mailbox.mark_delivered(&[id]);
        } else {
            withdraw(&mailbox, &input);
        }
        fill_withdrawn(&mailbox);
        assert!(!mailbox
            .snapshot()
            .terminal_items
            .iter()
            .any(|item| item.input_id == input.input_id));
        input.command_id = uuid::Uuid::now_v7().to_string();
        let receipt = mailbox.enqueue(&input).unwrap();
        assert_eq!(receipt.results[0].state, terminal);
        assert!(receipt.snapshot.items.is_empty());
        mailbox.enter_idle();
        assert!(inbox.queue().is_empty());
        input.command_id = uuid::Uuid::now_v7().to_string();
        input.content = MessageContent::text("更换载荷");
        assert_eq!(
            mailbox.enqueue(&input).unwrap_err(),
            UserInputQueueError::IdentityConflict
        );
    }
}

#[test]
fn old_enqueue_new_delivery_survives_overflow_and_lost_event() {
    let snapshots = Arc::new(Mutex::new(Vec::new()));
    let captured = snapshots.clone();
    let inbox = Arc::new(SessionInbox::new(Arc::new(MessageQueue::new())));
    let mailbox = UserInputMailbox::new(
        "session".into(),
        inbox.clone(),
        Arc::new(move |event| {
            if let StateEvent::UserInputQueueChanged { snapshot, .. } = event {
                captured.lock().push(snapshot);
            }
        }),
    );
    mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let input = make_input(&mailbox, "最早入队，最后交付");
    mailbox.enqueue(&input).unwrap();
    fill_withdrawn(&mailbox);
    snapshots.lock().clear();
    mailbox.enter_idle();
    let messages = inbox.queue().drain_all();
    let id = messages[0].message().unwrap().id();
    mailbox.mark_claimed(&[id]);
    mailbox.mark_delivered(&[id]);
    assert!(snapshots.lock().iter().any(|snapshot| snapshot
        .terminal_items
        .iter()
        .any(|item| item.input_id == input.input_id)));
    // 丢弃所有发布事件后，轮询仍保留最后交付项，而不是按 enqueue 顺序挤掉它。
    snapshots.lock().clear();
    let snapshot = mailbox.snapshot();
    assert_eq!(snapshot.terminal_items.len(), TERMINAL_CAPACITY);
    assert_eq!(
        snapshot.terminal_items.last().unwrap().input_id,
        input.input_id
    );
}

#[test]
fn same_batch_delivery_uses_receive_order_not_enqueue_order() {
    let (mailbox, _) = make_mailbox();
    mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let a = make_input(&mailbox, "A");
    let b = make_input(&mailbox, "B");
    mailbox.enqueue(&a).unwrap();
    mailbox.enqueue(&b).unwrap();
    mailbox.dispatch(&make_dispatch(&mailbox, &[&b])).unwrap();
    mailbox.dispatch(&make_dispatch(&mailbox, &[&a])).unwrap();
    let ids: Vec<_> = [&b, &a]
        .iter()
        .map(|input| MessageId::from(uuid::Uuid::parse_str(&input.input_id).unwrap()))
        .collect();
    mailbox.mark_claimed(&ids);
    assert_eq!(
        mailbox.mark_delivered(&ids),
        vec![b.input_id.clone(), a.input_id.clone()]
    );
    assert_eq!(
        mailbox
            .pending_delivery_notifications()
            .iter()
            .map(|item| item.input_id.clone())
            .collect::<Vec<_>>(),
        vec![b.input_id.clone(), a.input_id.clone()],
        "交付日志必须按 Receive 实际消费顺序，而不是入队顺序"
    );
    mailbox.ack_delivery_notification(&b.input_id);
    assert_eq!(
        mailbox
            .pending_delivery_notifications()
            .iter()
            .map(|item| item.input_id.clone())
            .collect::<Vec<_>>(),
        vec![a.input_id.clone()],
        "ack 只移除已推送项，不改变剩余顺序"
    );
    assert!(
        mailbox.mark_delivered(&ids).is_empty(),
        "重复确认不得重复进入交付日志"
    );
    assert_eq!(
        mailbox
            .snapshot()
            .terminal_items
            .iter()
            .map(|item| item.input_id.clone())
            .collect::<Vec<_>>(),
        vec![b.input_id, a.input_id]
    );
}

#[test]
fn delivery_journal_admission_rejects_before_claim_and_recovers_after_ack() {
    let (mailbox, inbox) = make_mailbox();
    mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    for index in 0..DELIVERY_JOURNAL_CAPACITY {
        let input = make_input(&mailbox, &format!("交付 {index}"));
        mailbox.enqueue(&input).unwrap();
        mailbox.enter_idle();
        let messages = inbox.queue().drain_all();
        let id = messages[0].message().unwrap().id();
        mailbox.mark_claimed(&[id]);
        assert_eq!(mailbox.mark_delivered(&[id]).len(), 1);
    }
    // 日志已满：新输入在 enqueue 准入处被拒绝，尚未进入 MQ/Claimed。
    let rejected = make_input(&mailbox, "准入拒绝输入");
    assert_eq!(
        mailbox.enqueue(&rejected).unwrap_err(),
        UserInputQueueError::Capacity,
        "日志容量必须在消费前拒绝，不能留下 Claimed"
    );
    assert!(inbox.queue().is_empty(), "拒绝的输入不能交接给 MQ");
    assert!(
        !mailbox
            .snapshot()
            .items
            .iter()
            .any(|item| item.input_id == rejected.input_id),
        "拒绝的输入不能进入队列投影"
    );

    // 客户端恢复：泵确认旧日志后，槽位释放，同一输入可以正常准入、消费并交付。
    let pending = mailbox.pending_delivery_notifications();
    assert_eq!(pending.len(), DELIVERY_JOURNAL_CAPACITY);
    for item in pending {
        mailbox.ack_delivery_notification(&item.input_id);
    }
    mailbox.enqueue(&rejected).unwrap();
    mailbox.enter_idle();
    let messages = inbox.queue().drain_all();
    let rejected_id = messages[0].message().unwrap().id();
    mailbox.mark_claimed(&[rejected_id]);
    assert_eq!(
        mailbox.mark_delivered(&[rejected_id]),
        vec![rejected.input_id]
    );
    assert_eq!(mailbox.pending_delivery_notifications().len(), 1);
}

#[test]
fn unpublished_terminal_cannot_be_evicted() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let blocked = Arc::new(AtomicBool::new(false));
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let release = Arc::new(std::sync::Barrier::new(2));
    let callback_release = release.clone();
    let callback_blocked = blocked.clone();
    let snapshots = Arc::new(Mutex::new(Vec::new()));
    let captured = snapshots.clone();
    let inbox = Arc::new(SessionInbox::new(Arc::new(MessageQueue::new())));
    let mailbox = UserInputMailbox::new(
        "session".into(),
        inbox.clone(),
        Arc::new(move |event| {
            if let StateEvent::UserInputQueueChanged { snapshot, .. } = event {
                if snapshot
                    .terminal_items
                    .iter()
                    .any(|item| item.state == UserInputState::Delivered)
                    && !callback_blocked.swap(true, Ordering::SeqCst)
                {
                    entered_tx.send(()).unwrap();
                    callback_release.wait();
                }
                captured.lock().push(snapshot);
            }
        }),
    );
    mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let input = make_input(&mailbox, "等待首次发布");
    mailbox.enqueue(&input).unwrap();
    mailbox.enter_idle();
    let messages = inbox.queue().drain_all();
    let id = messages[0].message().unwrap().id();
    mailbox.mark_claimed(&[id]);
    let delivering = mailbox.clone();
    let thread = std::thread::spawn(move || delivering.mark_delivered(&[id]));
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    // 第一次发布仍阻塞。后续快照必须先携带该终态，才允许它被淘汰。
    fill_withdrawn(&mailbox);
    let observed = snapshots.lock().iter().any(|snapshot| {
        snapshot
            .terminal_items
            .iter()
            .any(|item| item.input_id == input.input_id)
    });
    release.wait();
    thread.join().unwrap();
    assert!(observed, "并发终结不能在首次快照发布之前挤掉旧终态");
    assert_eq!(mailbox.snapshot().terminal_items.len(), TERMINAL_CAPACITY);
}
