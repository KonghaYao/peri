use super::*;

#[tokio::test(start_paused = true)]
async fn old_bound_control_message_is_suppressed_on_new_run() {
    let manager = Arc::new(TaskManager::new());
    let child = make_child(manager.clone(), true);
    let built = nested_context(
        &child,
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    );
    let old_execution = peri_acp_types::session::ExecutionBinding {
        turn_id: Default::default(),
        attempt_id: Default::default(),
    };
    let reminder = crate::session::producer_reminders::trusted_reminder(
        peri_acp_types::system_reminder::ReminderCategory::Task,
        "subagent_task_lifecycle",
        "old_run_control",
        peri_acp_types::system_reminder::ReminderSeverity::Info,
        peri_acp_types::system_reminder::ReminderDelivery::Required,
        "old run control".into(),
        None,
        serde_json::json!({}),
    );
    child.queue().push(
        QueuedMessage::system_reminder(
            crate::session::MessageKind::Defer,
            MessageSource::SystemInjected,
            reminder,
        )
        .with_policy(peri_acp_types::session::MessagePolicy::continue_current_run(old_execution)),
    );
    let received = run_receive(ReceiveInput {
        context: built.context,
    })
    .await
    .unwrap();
    assert_eq!(received.consumed_count, 1);
    assert_eq!(received.wake_up_count, 0, "旧执行绑定必须被抑制");
}

struct FailingLlm;

#[async_trait::async_trait]
impl ReactLLM for FailingLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[crate::messages::BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning> {
        Err(anyhow::anyhow!("model failed").into())
    }
}

/// 执行错误 + 交接写入失败：原错误与结构化 handoff_persistence_failed 同时保留。
#[tokio::test(start_paused = true)]
async fn execution_error_with_handoff_write_failure_keeps_both_causes() {
    let manager = Arc::new(TaskManager::new());
    let child_id = uuid::Uuid::now_v7().to_string();
    let child = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some(child_id.clone()),
    );
    let (owner, store) = bind_owner_store(&child, &manager);
    child.set_subagent_host(SubagentHost {
        task_manager: Some(manager.clone()),
        child_owner: Some(owner),
        ..Default::default()
    });
    TaskManagerPort::register(
        manager.as_ref(),
        BgTaskRegistration {
            task_id: "shell-permanent".into(),
            kind: BgTaskKind::Shell,
            summary: "permanent".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        },
    )
    .unwrap();
    child.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run shell"),
    ));
    let built = build_v2_subagent_context(
        Some(child.clone()),
        Box::new(FailingLlm),
        Arc::new(MiddlewareChain::new()),
        Vec::new(),
        Arc::new(|_| true),
        None,
        "/tmp",
        child.config().cancel_token.as_ref().clone(),
        None,
        None,
        None,
        None,
        Some(agent_id_from_child_thread(&child_id)),
    );
    let permit = permit_for(&child, &built.context);
    store.restrict_to_history_read_only();
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        run_child_until_terminal(built.context, 10, &child),
    )
    .await
    .expect("model failure must not wait for D");
    if let Some(permit) = permit {
        permit.finish(Err("handoff persistence failed".into()));
    }
    let ChildRunOutcome::HandoffFailed { cause, failure } = outcome else {
        panic!("expected structured handoff failure: {outcome:?}");
    };
    let cause = cause.expect("原执行错误必须保留");
    assert!(
        cause.user_facing_message().contains("model failed"),
        "{cause:?}"
    );
    assert!(
        failure.error.to_string().contains("persistence")
            || failure.error.to_string().contains("append_history"),
        "{:?}",
        failure.error
    );
    let receipt = failure.receipt.expect("冻结交接身份必须保留");
    assert_eq!(receipt.child_thread_id, child_id);
    assert!(receipt
        .tasks
        .iter()
        .any(|task| task.task_id == "shell-permanent"));
}

/// Receive 的 canonical 提交跨过 D：提交完成后才判定到期，必须交接该未推理消息，
/// 不能因 active/pending/MQ 已清空而误报 Settled。
#[tokio::test(start_paused = true)]
async fn receive_commit_crossing_deadline_hands_off_unprocessed_result() {
    let manager = Arc::new(TaskManager::new());
    let child_id = uuid::Uuid::now_v7().to_string();
    let child = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some(child_id.clone()),
    );
    let (owner, store) = bind_owner_store(&child, &manager);
    child.set_subagent_host(SubagentHost {
        task_manager: Some(manager.clone()),
        child_owner: Some(owner),
        ..Default::default()
    });
    let ticket = TaskManagerPort::register(
        manager.as_ref(),
        BgTaskRegistration {
            task_id: "shell-task".into(),
            kind: BgTaskKind::Shell,
            summary: "permanent until result arrives".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        },
    )
    .unwrap();
    child.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run shell"),
    ));
    let built = fixed_context(&child, "child answer");
    let permit = permit_for(&child, &built.context);
    let run_child = child.clone();
    let mut runner = tokio::spawn(async move {
        let outcome = run_child_until_terminal(built.context, 10, &run_child).await;
        if let Some(permit) = permit {
            permit.finish(Ok(()));
        }
        outcome
    });
    while !child.transcript().read().entries().iter().any(|entry| {
        matches!(entry, crate::session::TranscriptEntry::Reminder { reminder, .. }
            if reminder.as_reminder().kind == "active_background_shell_before_submit")
    }) {
        tokio::task::yield_now().await;
    }
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    // No process exists in this fixture; the delayed obligation is the message.
    manager.confirm_external_execution_stopped(&ticket);
    assert!(manager.complete(&ticket, nested_result()));
    let (entered, release) = store.gate_next_reminder_append();
    let delivery_id = peri_acp_types::messages::MessageId::new();
    let reminder = crate::session::producer_reminders::trusted_reminder(
        peri_acp_types::system_reminder::ReminderCategory::Task,
        "subagent_task_lifecycle",
        "late_terminal_result",
        peri_acp_types::system_reminder::ReminderSeverity::Info,
        peri_acp_types::system_reminder::ReminderDelivery::Required,
        "late terminal result".into(),
        None,
        serde_json::json!({}),
    );
    child.queue().push(
        QueuedMessage::system_reminder_with_delivery_id(
            crate::session::MessageKind::Defer,
            MessageSource::SystemInjected,
            reminder,
            delivery_id,
        )
        .with_policy(peri_acp_types::session::MessagePolicy::ensure_processing()),
    );
    entered.await.expect("Receive must enter canonical append");
    // D 在 append 等待期间届满，提交完成后再判定。
    tokio::time::advance(Duration::from_secs(121)).await;
    let _ = release.send(());
    let outcome = tokio::time::timeout(Duration::from_secs(5), &mut runner)
        .await
        .expect("runner must finish after commit")
        .unwrap();
    let ChildRunOutcome::HandedOff { receipt } = outcome else {
        panic!("committed-but-unreasoned result must hand off: {outcome:?}");
    };
    assert!(receipt.tasks.is_empty(), "task 已终态");
    assert!(
        receipt.message_ids.contains(&delivery_id),
        "receipt 必须携带未推理消息身份: {:?}",
        receipt.message_ids
    );
    assert_eq!(manager.active_count(), 0);
}

/// A→B→C：C 的未完成结果归属直接父 B，A 不接收 C 的身份；各层均有界结束。
#[tokio::test(start_paused = true)]
async fn nested_a_b_c_reports_incomplete_only_to_direct_parent() {
    let c_manager = Arc::new(TaskManager::new());
    let c = make_child(c_manager.clone(), true);
    let c_id = c.store().thread_id.clone().unwrap();
    TaskManagerPort::register(
        c_manager.as_ref(),
        BgTaskRegistration {
            task_id: "c-shell".into(),
            kind: BgTaskKind::Shell,
            summary: "C permanent shell".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        },
    )
    .unwrap();
    c.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("C task"),
    ));
    let c_built = fixed_context(&c, "C waiting");
    let b_manager = Arc::new(TaskManager::new());
    let b = make_child(b_manager.clone(), false);
    let inbox = crate::session::async_router::AsyncRouter::for_queue(b.queue());
    let c_run_id = c_id.clone();
    let c_completion = crate::session::subagent::run_sync::run_sync_with_registered_handoff(
        c.clone(),
        c_id.clone(),
        "C".into(),
        Some(b_manager.clone()),
        Some(crate::session::subagent::run_sync::new_sync_task_id()),
        Some(Arc::new(move |result, kind| {
            inbox.route_bg_result(result, kind);
            Ok(())
        })),
        move |handoff, _| async move {
            crate::session::subagent::run_sync::run_sync_subagent(
                &c_run_id,
                "C",
                "/tmp",
                10,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                c_built,
                c,
                None,
                Some(handoff),
            )
            .await
        },
    )
    .await
    .unwrap();
    let crate::session::subagent::run_sync::SyncCompletion::Pending(pending) = c_completion else {
        panic!("C must hand off its permanent shell");
    };
    assert_eq!(pending.receipt.child_thread_id, c_id);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !b.queue().has_required() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("C's incomplete result must reach direct parent B");

    b.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("B waits for C"),
    ));
    let b_built = fixed_context(&b, "B summarized C");
    let b_outcome = run_child_until_terminal(b_built.context, 10, &b).await;
    assert!(
        matches!(b_outcome, ChildRunOutcome::Settled),
        "{b_outcome:?}"
    );
    let b_text = crate::session::subagent::util::extract_last_ai_text(&b);
    assert!(b_text.contains("B summarized C"));
    assert!(transcript_contains(&b, &c_id), "直接父 B 必须收到 C 的身份");

    let a = make_child(Arc::new(TaskManager::new()), false);
    a.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("B summarized C"),
    ));
    let a_built = fixed_context(&a, "A done");
    let a_outcome = run_child_until_terminal(a_built.context, 10, &a).await;
    assert!(
        matches!(a_outcome, ChildRunOutcome::Settled),
        "{a_outcome:?}"
    );
    assert!(
        !transcript_contains(&a, &c_id),
        "A 不得越过 B 接收 C 的身份"
    );
    assert_eq!(c_manager.active_count(), 1, "C 的 Shell 保持真实状态");
}
