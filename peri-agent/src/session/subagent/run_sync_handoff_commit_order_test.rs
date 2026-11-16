// Responsibility-specific cases share the original test namespace and fixtures.

/// [回归] wrapper 在交接回执发出后、接管终态前被 drop：执行体接回唯一写入，
/// 不留下无人收敛的 HandedOff。worker 未准入，父 MQ 不应出现迟到结果。
#[tokio::test(start_paused = true)]
async fn wrapper_drop_after_handoff_keeps_execution_as_single_terminal_writer() {
    let (session, _owner, store) =
        crate::session::subagent::run_sync::tests::bound_child_with_store(Arc::new(
            TaskManager::new(),
        ));
    let child_id = session
        .store()
        .thread_id
        .clone()
        .expect("the clone fixture result is required for this assertion");
    store.register_bound_session(&child_id, "/tmp");
    let parent_manager = Arc::new(TaskManager::new());
    let parent_target = uuid::Uuid::now_v7().to_string();
    store.register_bound_session(&parent_target, "/tmp");
    let route = TerminalMqRoute {
        store: Arc::clone(&store) as Arc<dyn SessionResources>,
        target: parent_target.clone(),
        mailbox: None,
    };
    let receipt = receipt(&session, &child_id);
    let built = crate::session::subagent::run_sync::tests::context(&session, "child answer");
    let run_session = Arc::clone(&session);
    let run_child = child_id.clone();
    let run_store = Arc::clone(&store) as Arc<dyn SessionResources>;
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();

    let call = crate::session::subagent::run_sync::run_sync_with_registered_handoff(
        Arc::clone(&session),
        child_id.clone(),
        "fixture".into(),
        Some(Arc::clone(&parent_manager)),
        Some(crate::session::subagent::run_sync::new_sync_task_id()),
        Some(Arc::new(|_, _| Ok(()))),
        Some(route),
        move |handoff, _| async move {
            let SyncHandoffSignal {
                sender,
                terminal,
                wrapper_settles,
            } = handoff;
            sender.send(receipt).expect("receipt receiver alive");
            entered_tx
                .send(())
                .expect("entered tx send test receiver must remain available for this delivery");
            release_rx
                .await
                .expect("the release rx fixture result is required for this assertion");
            // 回执已发出但 wrapper 不会再轮询；执行体仍须按同一归属收尾。
            let (unused_sender, _unused_receiver) = tokio::sync::oneshot::channel();
            crate::session::subagent::run_sync::run_sync_subagent(
                &run_child,
                "fixture",
                "/tmp",
                10,
                None,
                None,
                None,
                Some(run_store),
                None,
                None,
                None,
                None,
                None,
                built,
                run_session,
                None,
                Some(SyncHandoffSignal {
                    sender: unused_sender,
                    terminal,
                    wrapper_settles,
                }),
            )
            .await
        },
    );
    let mut call = Box::pin(call);
    let first = std::future::poll_fn(|cx| std::task::Poll::Ready(call.as_mut().poll(cx))).await;
    assert!(
        matches!(first, std::task::Poll::Pending),
        "首轮只启动执行，等待交接回执"
    );
    // 执行已发出回执；wrapper 未再被轮询就 drop，模拟父工具调用放弃接管。
    entered_rx
        .await
        .expect("the entered rx fixture result is required for this assertion");
    drop(call);
    release_tx
        .send(())
        .expect("release tx send test receiver must remain available for this delivery");

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let meta = store
                .load_session_meta(&ThreadId::from(child_id.clone()))
                .await
                .expect("the load session meta fixture result is required for this assertion");
            if meta.agent_status.is_terminal() {
                break;
            }
            peri_time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("execution must take over the terminal after wrapper drop");

    let meta = store
        .load_session_meta(&ThreadId::from(child_id.clone()))
        .await
        .expect("the load session meta fixture result is required for this assertion");
    // wrapper drop 触发的取消守卫使本次执行按真实取消映射；关键是终态由执行体写入。
    assert_eq!(meta.agent_status, AgentStatus::Cancelled);
    assert_eq!(meta.work_state, WorkState::Stopped);
    assert_eq!(store.status_writes(), 1, "执行体接管后只能有一个终态写入者");
    assert!(
        parent_manager.snapshot().tasks.is_empty(),
        "worker 未准入不得留下父任务登记"
    );
    let parent_queue = MessageQueue::bind(
        Arc::clone(&store) as Arc<dyn SessionResources>,
        parent_target,
    );
    assert_eq!(
        parent_queue
            .snapshot()
            .await
            .expect("the snapshot fixture result is required for this assertion")
            .len(),
        0,
        "未准入 worker 不得产生父 MQ 结果"
    );
}

#[tokio::test]
async fn handoff_waits_for_working_commit_and_failure_never_returns_pending() {
    use crate::session::test_resources::mock::ResumeLoadGate;
    use std::sync::atomic::{AtomicBool, Ordering};
    for fail in [false, true] {
        let fixture = fixture();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *fixture
            .store
            .working_write_gate
            .lock()
            .expect("fixture store mutex must remain unpoisoned throughout the test") =
            Some(ResumeLoadGate {
                entered: entered_tx,
                release: release_rx,
                dropped: Arc::new(AtomicBool::new(false)),
            });
        fixture
            .store
            .fail_working_write
            .store(fail, Ordering::SeqCst);
        let receipt = receipt(&fixture.session, &fixture.child_id);
        let cancel = fixture.session.config().cancel_token.clone();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let call = tokio::spawn(
            crate::session::subagent::run_sync::run_sync_with_registered_handoff(
                fixture.session.clone(),
                fixture.child_id.clone(),
                "fixture".into(),
                Some(fixture.parent_manager.clone()),
                Some(fixture.task_id.clone()),
                Some(Arc::new(|_, _| Ok(()))),
                Some(fixture.route.clone()),
                move |handoff, _| async move {
                    handoff.sender.send(receipt).expect(
                        "handoff sender test receiver must remain available for this delivery",
                    );
                    tokio::select! { _ = finish_rx => {}, _ = cancel.cancelled() => {} }
                    Ok(ChildRunOutcome::Settled)
                },
            ),
        );
        entered_rx
            .await
            .expect("the entered rx fixture result is required for this assertion");
        assert!(!call.is_finished(), "持久化确认前不得返回 Pending");
        assert_eq!(
            fixture
                .store
                .load_session_meta(&fixture.child_id)
                .await
                .expect("the load session meta fixture result is required for this assertion")
                .work_state,
            WorkState::Idle
        );
        release_tx
            .send(())
            .expect("release tx send test receiver must remain available for this delivery");
        let result = call
            .await
            .expect("the call fixture result is required for this assertion");
        if fail {
            assert!(result.is_err());
            assert!(fixture.session.config().cancel_token.clone().is_cancelled());
        } else {
            assert!(matches!(
                result.expect("the result fixture result is required for this assertion"),
                SyncCompletion::Pending(_)
            ));
            assert_eq!(
                fixture
                    .store
                    .load_session_meta(&fixture.child_id)
                    .await
                    .expect("the load session meta fixture result is required for this assertion")
                    .work_state,
                WorkState::Working
            );
        }
        let _ = finish_tx.send(());
    }
}
