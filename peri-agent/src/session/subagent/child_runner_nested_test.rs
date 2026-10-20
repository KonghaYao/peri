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
