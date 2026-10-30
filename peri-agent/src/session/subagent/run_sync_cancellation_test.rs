use super::*;

/// 阻塞在模型调用中的同步子任务：父调用 future 被 drop（生产侧 cancel 分支）
/// 后，子执行仍须走完既有收尾并恰好发射一次 SubagentStopped。
#[tokio::test]
async fn cancelled_sync_child_emits_stopped_after_caller_future_dropped() {
    let manager = Arc::new(TaskManager::new());
    let (session, _owner, store) = bound_child_with_store(manager.clone());
    let child_id = session.store().thread_id.clone().unwrap();
    session.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("block in model call"),
    ));
    let entered = Arc::new(tokio::sync::Notify::new());
    let gate = Arc::new(tokio::sync::Notify::new());
    let built = blocking_context(&session, entered.clone(), gate.clone());

    let events: Arc<parking_lot::Mutex<Vec<ExecutorEvent>>> = Arc::default();
    let capture = events.clone();
    let handler: Arc<dyn AgentEventHandler> =
        Arc::new(FnEventHandler(move |event| capture.lock().push(event)));

    let resources: Arc<dyn SessionResources> = store.clone();
    let cancel = session.config().cancel_token.clone();
    let caller_cancel = cancel.clone();
    let run_session = session.clone();
    let run_id = child_id.clone();
    let run_handler = handler.clone();
    // 与生产同形的父工具调用 future：cancel 分支命中时整块 wrapper 被 drop。
    let caller = tokio::spawn(async move {
        let wrapper = run_sync_with_registered_handoff(
            run_session.clone(),
            run_id.clone(),
            "fixture".into(),
            None,
            Some(new_sync_task_id()),
            Some(Arc::new(|_, _| Ok(()))),
            move |handoff, _| async move {
                run_sync_subagent(
                    &run_id,
                    "fixture",
                    "/tmp",
                    10,
                    Some(run_handler),
                    None,
                    None,
                    Some(resources),
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
        );
        tokio::pin!(wrapper);
        tokio::select! {
            biased;
            _ = caller_cancel.cancelled() => {}
            outcome = &mut wrapper => panic!(
                "wrapper finished before cancellation: {}",
                outcome
                    .err()
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "completed".into())
            ),
        }
    });

    tokio::time::timeout(Duration::from_secs(5), async {
        while !events
            .lock()
            .iter()
            .any(|event| matches!(event.payload(), ExecutorEvent::SubagentStarted { .. }))
        {
            peri_time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("SubagentStarted must reach the parent handler");
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("child model call must be in flight before cancellation");
    cancel.cancel();
    caller.await.unwrap();

    let stopped = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let observed = events
                .lock()
                .iter()
                .find(|event| matches!(event.payload(), ExecutorEvent::SubagentStopped { .. }))
                .cloned();
            if let Some(event) = observed {
                break event;
            }
            peri_time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("dropping the caller future must not swallow SubagentStopped");
    let ExecutorEvent::SubagentStopped {
        result,
        is_error,
        instance_id,
        ..
    } = stopped.payload().clone()
    else {
        unreachable!("filtered to SubagentStopped above");
    };
    assert_eq!(instance_id, child_id);
    assert!(is_error, "cancel 收尾必须标记 error: {result}");
    assert_eq!(result, "interrupted");
    // 收尾顺序：close 落盘终态后才是 v1 Stopped，故此处终态已持久化。
    assert_eq!(
        store.statuses().last().map(|(_, status)| status.as_str()),
        Some("error"),
        "子线程终态必须持久化"
    );
    assert_eq!(manager.active_count(), 0);

    peri_time::sleep(Duration::from_millis(200)).await;
    let events = events.lock();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.payload(), ExecutorEvent::SubagentStarted { .. }))
            .count(),
        1,
        "Started 恰好一次"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.payload(), ExecutorEvent::SubagentStopped { .. }))
            .count(),
        1,
        "取消收尾只发一次 Stopped"
    );
}

/// 关闭失败必须保留诊断，不因同步 worker 结束而宣称成功。
#[tokio::test]
async fn explicit_close_reports_failed_sync_delivery() {
    let manager = Arc::new(TaskManager::new());
    let session = child(manager.clone());
    session
        .subagent_host()
        .unwrap()
        .close_state
        .record_sync_delivery_failure("sync-broken", "callback rejected");
    let error = super::super::super::close::close_subagent_session_scope(session)
        .await
        .unwrap_err();
    assert!(error.contains("callback rejected"), "{error}");
}
