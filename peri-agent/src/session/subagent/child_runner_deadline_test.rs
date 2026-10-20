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
