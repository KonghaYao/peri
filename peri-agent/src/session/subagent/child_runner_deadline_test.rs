use super::*;

fn install_started_window(
    context: &mut crate::agent::stages::StageContext,
) -> tokio::time::Instant {
    let window = Arc::new(ChildSettlementWindow::default());
    let deadline = window.start();
    context.async_ctx.child_settlement = Some(window);
    deadline
}

/// 完成与 D 同刻，且完成通知先被调度：activity 胜出，正常 Settled。
#[tokio::test(start_paused = true)]
async fn completion_just_before_deadline_prefers_settled() {
    let (child, manager, mut built, _) = child_with_shell_task(false);
    let deadline = install_started_window(&mut built.context);
    tokio::time::advance(Duration::from_secs(119)).await;
    let settle_manager = manager.clone();
    let settle = tokio::spawn(async move {
        tokio::time::sleep_until(deadline - Duration::from_millis(1)).await;
        assert!(settle_manager.complete("shell-task", nested_result()));
    });
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    let permit = permit_for(&child, &built.context);
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        run_child_until_terminal(built.context, 10, &child),
    )
    .await
    .expect("same-instant completion must settle before D handoff");
    if let Some(permit) = permit {
        permit.finish(Ok(()));
    }
    settle.await.unwrap();
    assert!(matches!(outcome, ChildRunOutcome::Settled), "{outcome:?}");
}

/// 完成发生在 D 之后：deadline 先胜出，交接且不误报成功。
#[tokio::test(start_paused = true)]
async fn completion_just_after_deadline_hands_off() {
    let (child, manager, mut built, _) = child_with_shell_task(false);
    let deadline = install_started_window(&mut built.context);
    tokio::time::advance(Duration::from_secs(119)).await;
    let settle_manager = manager.clone();
    let settle = tokio::spawn(async move {
        tokio::time::sleep_until(deadline + Duration::from_millis(1)).await;
        assert!(settle_manager.complete("shell-task", nested_result()));
    });
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    let permit = permit_for(&child, &built.context);
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        run_child_until_terminal(built.context, 10, &child),
    )
    .await
    .expect("deadline must win before the later completion");
    if let Some(permit) = permit {
        permit.finish(Ok(()));
    }
    settle.await.unwrap();
    assert!(
        matches!(outcome, ChildRunOutcome::HandedOff { .. }),
        "{outcome:?}"
    );
}

/// cancel 与 D 同刻且 cancel 先被调度：cancel 优先，不得误判为普通交接。
#[tokio::test(start_paused = true)]
async fn cancel_just_before_deadline_prefers_interrupted() {
    let (child, _manager, mut built, _) = child_with_shell_task(false);
    let deadline = install_started_window(&mut built.context);
    tokio::time::advance(Duration::from_secs(119)).await;
    let cancel = child.config().cancel_token.clone();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep_until(deadline - Duration::from_millis(1)).await;
        cancel.cancel();
    });
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    let permit = permit_for(&child, &built.context);
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        run_child_until_terminal(built.context, 10, &child),
    )
    .await
    .expect("cancel must interrupt before the deadline handoff");
    if let Some(permit) = permit {
        permit.finish(Ok(()));
    }
    canceller.await.unwrap();
    assert!(
        matches!(outcome, ChildRunOutcome::Interrupted),
        "{outcome:?}"
    );
}

/// cancel 发生在 D 之后：deadline 先结束计算，Shell 不被自动取消。
#[tokio::test(start_paused = true)]
async fn cancel_just_after_deadline_hands_off_without_cancelling_shell() {
    let (child, manager, mut built, _) = child_with_shell_task(false);
    let deadline = install_started_window(&mut built.context);
    tokio::time::advance(Duration::from_secs(119)).await;
    let cancel = child.config().cancel_token.clone();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep_until(deadline + Duration::from_millis(1)).await;
        cancel.cancel();
    });
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    let permit = permit_for(&child, &built.context);
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        run_child_until_terminal(built.context, 10, &child),
    )
    .await
    .expect("deadline must win before the later cancel");
    if let Some(permit) = permit {
        permit.finish(Ok(()));
    }
    canceller.await.unwrap();
    assert!(
        matches!(outcome, ChildRunOutcome::HandedOff { .. }),
        "{outcome:?}"
    );
    assert_eq!(manager.active_count(), 1, "普通 deadline 不取消 Shell");
}

/// 已准入 Act 的 tool result 只存在于 transcript；D 到期时 runner 必须交接而非 Settled。
#[tokio::test(start_paused = true)]
async fn admitted_act_tool_result_hands_off_instead_of_settled() {
    let manager = Arc::new(TaskManager::new());
    let child = make_child(manager.clone(), true);
    child.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("use a tool"),
    ));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut built = build_v2_subagent_context(
        Some(child.clone()),
        Box::new(ToolThenExpireLlm {
            calls: calls.clone(),
        }),
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
        Some(agent_id_from_child_thread(
            &child.store().thread_id.clone().unwrap(),
        )),
    );
    let window = Arc::new(ChildSettlementWindow::default());
    window.start();
    built.context.async_ctx.child_settlement = Some(window);
    let permit = permit_for(&child, &built.context);
    let outcome = tokio::time::timeout(
        Duration::from_secs(200),
        run_child_until_terminal(built.context, 10, &child),
    )
    .await
    .expect("runner must finish after admitted Act");
    if let Some(permit) = permit {
        permit.finish(Ok(()));
    }
    let ChildRunOutcome::HandedOff { receipt } = outcome else {
        panic!("tool result must hand off, not settle: {outcome:?}");
    };
    assert!(receipt.tasks.is_empty());
    assert_eq!(receipt.unprocessed_tool_calls, vec!["call-1"]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// 父级映射：已准入 Act tool result 的交接必须 success=false。
#[tokio::test(start_paused = true)]
async fn admitted_act_tool_result_parent_result_is_failure() {
    let manager = Arc::new(TaskManager::new());
    let parent_manager = Arc::new(TaskManager::new());
    let child = make_child(manager, true);
    child.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("use a tool"),
    ));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut built = build_v2_subagent_context(
        Some(child.clone()),
        Box::new(ToolThenExpireLlm {
            calls: calls.clone(),
        }),
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
        Some(agent_id_from_child_thread(
            &child.store().thread_id.clone().unwrap(),
        )),
    );
    let window = Arc::new(ChildSettlementWindow::default());
    window.start();
    built.context.async_ctx.child_settlement = Some(window);
    let child_id = child.store().thread_id.clone().unwrap();
    let task_id = crate::session::subagent::run_sync::new_sync_task_id();
    let (delivered_tx, delivered_rx) = tokio::sync::oneshot::channel();
    let delivered_tx = parking_lot::Mutex::new(Some(delivered_tx));
    let run_id = child_id.clone();
    let run_session = child.clone();
    let call = tokio::spawn(
        crate::session::subagent::run_sync::run_sync_with_registered_handoff(
            child.clone(),
            child_id.clone(),
            "fixture".into(),
            Some(parent_manager),
            Some(task_id),
            Some(Arc::new(
                move |result: &crate::agent::events::BackgroundTaskResult, _| {
                    if let Some(sender) = delivered_tx.lock().take() {
                        let _ = sender.send(result.clone());
                    }
                    Ok(())
                },
            )),
            move |handoff, _| async move {
                crate::session::subagent::run_sync::run_sync_subagent(
                    &run_id,
                    "fixture",
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
                    built,
                    run_session,
                    None,
                    Some(handoff),
                )
                .await
            },
        ),
    );
    let completion = call.await.unwrap().unwrap();
    let crate::session::subagent::run_sync::SyncCompletion::Pending(pending) = completion else {
        panic!("admitted Act tool result must detach as Pending");
    };
    assert_eq!(pending.receipt.unprocessed_tool_calls, vec!["call-1"]);
    let delivered = tokio::time::timeout(Duration::from_secs(5), delivered_rx)
        .await
        .expect("parent result must be delivered")
        .unwrap();
    assert!(!delivered.success);
    assert!(!delivered.timed_out);
    assert!(delivered.output.contains("status: incomplete"));
    assert!(delivered.output.contains("call-1"));
}
