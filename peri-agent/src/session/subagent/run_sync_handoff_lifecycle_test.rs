// Responsibility-specific cases share the original test namespace and fixtures.

/// pending 任务列表为空但仍有未消费消息：wrapper 仍以 receipt 交接，不因空列表 abort。
#[tokio::test(start_paused = true)]
async fn handoff_with_empty_task_list_keeps_retained_messages() {
    let manager = Arc::new(TaskManager::new());
    let parent_manager = Arc::new(TaskManager::new());
    let (session, _owner, store) = bound_child_with_store(manager.clone());
    let ticket = shell_task(&manager, "shell-task");
    session.queue().push_sync(QueuedMessage::info(
        MessageSource::HookStopIntent,
        crate::messages::BaseMessage::human("stop"),
    ));
    session.queue().push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run shell"),
    ));
    let built = context(&session, "child answer");
    let child_id = session
        .store()
        .thread_id
        .clone()
        .expect("the clone fixture result is required for this assertion");
    store.register_bound_session(&child_id, "/tmp");
    let terminal_route = TerminalMqRoute {
        store: Arc::clone(&store) as Arc<dyn SessionResources>,
        target: child_id.clone(),
        mailbox: None,
    };
    let task_id = crate::session::subagent::run_sync::new_sync_task_id();
    let run_id = child_id.clone();
    let run_session = session.clone();
    let (delivered_tx, delivered_rx) = tokio::sync::oneshot::channel();
    let delivered_tx = parking_lot::Mutex::new(Some(delivered_tx));
    let call = tokio::spawn(
        crate::session::subagent::run_sync::run_sync_with_registered_handoff(
            session.clone(),
            child_id.clone(),
            "fixture".into(),
            Some(parent_manager.clone()),
            Some(task_id.clone()),
            Some(Arc::new(
                move |result: &peri_acp_types::event::BackgroundTaskResult, _| {
                    if let Some(sender) = delivered_tx.lock().take() {
                        let _ = sender.send(result.clone());
                    }
                    Ok(())
                },
            )),
            Some(terminal_route),
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
    tokio::time::advance(Duration::from_secs(1)).await;
    let delivery = crate::session::bg_complete::task_bg_complete_callback(
        crate::agent::async_tasks::delivery::SessionTerminalDelivery::for_queue(
            session.queue().clone(),
        ),
    );
    let mut result = shell_result("shell-task");
    result.agent_name = "shell".into();
    manager.confirm_external_execution_stopped(&ticket);
    assert!(manager
        .settle_completed(&ticket, result, delivery)
        .expect("the settle completed fixture result is required for this assertion"));
    let completion = call.await.expect("the call fixture result is required for this assertion").expect("handoff with empty task list keeps retained messages: the completed fixture operation must provide its result payload");
    let SyncCompletion::Pending(pending) = completion else {
        panic!("retained message must hand off even with empty task list");
    };
    assert!(pending.receipt.tasks.is_empty());
    assert!(!pending.receipt.message_ids.is_empty());
    let delivered = tokio::time::timeout(Duration::from_secs(5), delivered_rx)
        .await
        .expect("parent result must be delivered")
        .expect("the expect fixture result is required for this assertion");
    assert!(!delivered.success);
    assert!(!delivered.timed_out);
    assert_eq!(delivered.task_id, task_id);
}

/// worker 必须释放完整计算对象，而不只是 TaskTracker 计数归零。
#[tokio::test(start_paused = true)]
async fn sync_worker_releases_session_llm_and_context_after_takeover() {
    let manager = Arc::new(TaskManager::new());
    let parent_manager = Arc::new(TaskManager::new());
    let (session, _owner, store) = bound_child_with_store(manager.clone());
    shell_task(&manager, "shell-task");
    session.queue().push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run shell"),
    ));
    let marker = Arc::new(());
    let weak = Arc::downgrade(&marker);
    let built = probe_context(&session, marker);
    let child_id = session
        .store()
        .thread_id
        .clone()
        .expect("the clone fixture result is required for this assertion");
    store.register_bound_session(&child_id, "/tmp");
    let terminal_route = TerminalMqRoute {
        store: Arc::clone(&store) as Arc<dyn SessionResources>,
        target: child_id.clone(),
        mailbox: None,
    };
    let sync_state = session
        .subagent_host()
        .expect("the subagent host fixture result is required for this assertion")
        .close_state
        .clone();
    let task_id = crate::session::subagent::run_sync::new_sync_task_id();
    let (delivered_tx, delivered_rx) = tokio::sync::oneshot::channel();
    let delivered_tx = parking_lot::Mutex::new(Some(delivered_tx));
    let run_id = child_id.clone();
    let call = tokio::spawn(
        crate::session::subagent::run_sync::run_sync_with_registered_handoff(
            session.clone(),
            child_id.clone(),
            "fixture".into(),
            Some(parent_manager),
            Some(task_id.clone()),
            Some(Arc::new(
                move |result: &peri_acp_types::event::BackgroundTaskResult, _| {
                    if let Some(sender) = delivered_tx.lock().take() {
                        let _ = sender.send(result.clone());
                    }
                    Ok(())
                },
            )),
            Some(terminal_route),
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
                    session,
                    None,
                    Some(handoff),
                )
                .await
            },
        ),
    );
    assert!(matches!(
        call.await.expect("the call fixture result is required for this assertion").expect("sync worker releases session llm and context after takeover: the completed fixture operation must provide its result payload"),
        SyncCompletion::Pending(_)
    ));
    let delivered = tokio::time::timeout(Duration::from_secs(5), delivered_rx)
        .await
        .expect("parent result must be delivered")
        .expect("the expect fixture result is required for this assertion");
    assert!(!delivered.success);
    tokio::time::timeout(Duration::from_secs(2), async {
        while !sync_state.sync_workers.is_empty() {
            peri_time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("worker must end");
    tokio::time::timeout(Duration::from_secs(2), async {
        while weak.upgrade().is_some() {
            peri_time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("LLM/StageContext 必须随 worker 释放");
}

/// 真实 run_sync_subagent：执行错误 + handoff flush 失败时父级保留完整证据。
#[tokio::test(start_paused = true)]
async fn run_sync_handoff_failure_preserves_error_and_receipt() {
    use crate::session::subagent::types::SubagentFailure;

    let manager = Arc::new(TaskManager::new());
    let (session, owner, store) = bound_child_with_store(manager.clone());
    shell_task(&manager, "shell-task");
    session.queue().push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run shell"),
    ));
    let child_id = session
        .store()
        .thread_id
        .clone()
        .expect("the clone fixture result is required for this assertion");
    let built = crate::session::subagent::build_v2_subagent_context(
        Some(session.clone()),
        Box::new(FailingLlm),
        Arc::new(MiddlewareChain::new()),
        Vec::new(),
        Arc::new(|_| true),
        None,
        "/tmp",
        session.config().cancel_token.as_ref().clone(),
        None,
        None,
        None,
        None,
        Some(crate::session::subagent::agent_id_from_child_thread(
            &child_id,
        )),
    );
    store.restrict_to_history_read_only();
    // MQ 原子移交不再排队异步持久化，handoff 前的 flush 只能因 writer 关闭而失败。
    let writer = session
        .transcript()
        .read()
        .persist_tx_handle()
        .expect("the persist tx handle fixture result is required for this assertion");
    session.transcript().read().shutdown_persistence();
    tokio::time::timeout(std::time::Duration::from_secs(10), writer.closed())
        .await
        .expect("测试 writer 必须在 handoff 前关闭");
    let error = crate::session::subagent::run_sync::run_sync_subagent(
        &child_id, "fixture", "/tmp", 10, None, None, None, None, None, None, None, None, None,
        built, session, None, None,
    )
    .await
    .expect_err("model + handoff write failure must return a typed error");
    let failure = error
        .downcast_ref::<SubagentFailure>()
        .expect("typed SubagentFailure");
    assert!(
        failure
            .error()
            .user_facing_message()
            .contains("model failed"),
        "原执行错误必须保留: {}",
        failure.error().user_facing_message()
    );
    let handoff_failure = failure.handoff_failure().expect("结构化交接失败");
    assert!(
        handoff_failure.contains("handoff_persistence_failed"),
        "{handoff_failure}"
    );
    let flush_failure = failure
        .handoff_flush_failure()
        .expect("flush 失败必须附加到结构化失败");
    assert!(
        flush_failure.contains("append_history") || flush_failure.contains("persistence"),
        "{flush_failure}"
    );
    assert!(failure
        .public_message()
        .contains("handoff history flush failed"));
    let receipt = failure.handoff().expect("冻结 receipt");
    assert_eq!(receipt.child_thread_id, child_id);
    assert!(receipt
        .tasks
        .iter()
        .any(|task| task.task_id == "shell-task"));
    assert!(receipt.unprocessed_tool_calls.is_empty());
    assert!(owner.pending_receipt().is_some());
    assert!(owner.check_resume().is_err(), "未确认交接必须拒绝 resume");
}
