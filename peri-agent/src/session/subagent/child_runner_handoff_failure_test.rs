// Responsibility-specific cases share the original test namespace and fixtures.

/// 截止后的晚结果不自动复活旧 run；显式 resume 读取同一队列并推理。
#[tokio::test(start_paused = true)]
async fn late_result_after_deadline_requires_explicit_resume() {
    let (child, manager, built, initial_calls, result_calls, ticket) = child_with_nested_task();
    let child_id = child
        .store()
        .thread_id
        .clone()
        .expect("the clone fixture result is required for this assertion");
    let first = crate::session::subagent::run_sync::run_sync_subagent(
        &child_id,
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
        child.clone(),
        None,
        None,
    )
    .await
    .expect("the run sync subagent fixture result is required for this assertion");
    assert!(
        matches!(first, ChildRunOutcome::HandedOff { .. }),
        "{first:?}"
    );
    settle_nested_task(&child, &manager, &ticket);
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(
        result_calls.load(Ordering::SeqCst),
        0,
        "旧 run 不得自动消费截止后的结果"
    );
    child.queue().push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("continue explicitly"),
    ));
    let resumed = nested_context(&child, initial_calls.clone(), result_calls.clone());
    let second = crate::session::subagent::run_sync::run_sync_subagent(
        &child_id,
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
        resumed,
        child.clone(),
        None,
        None,
    )
    .await
    .expect("the run sync subagent fixture result is required for this assertion");
    assert!(matches!(second, ChildRunOutcome::Settled), "{second:?}");
    assert_eq!(result_calls.load(Ordering::SeqCst), 1);
    assert!(crate::session::subagent::util::extract_last_ai_text(&child)
        .contains("processed nested result"));
}

/// 无 owner 的嵌入路径在 D 后显式失败，不无限等待。
#[tokio::test(start_paused = true)]
async fn missing_owner_reports_capability_failure() {
    let manager = Arc::new(TaskManager::new());
    let child = make_child(manager.clone(), false);
    TaskManagerPort::register(
        manager.as_ref(),
        BgTaskRegistration {
            task_id: "shell-task".into(),
            kind: BgTaskKind::Shell,
            summary: "permanent".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        },
    )
    .expect("the register fixture result is required for this assertion");
    child.queue().push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run shell"),
    ));
    let built = crate::session::subagent::build_v2_subagent_context(
        Some(child.clone()),
        Box::new(NestedResultLlm {
            initial_calls: Arc::new(AtomicUsize::new(0)),
            result_calls: Arc::new(AtomicUsize::new(0)),
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
        Some(crate::session::subagent::agent_id_from_child_thread(
            &child
                .store()
                .thread_id
                .clone()
                .expect("the clone fixture result is required for this assertion"),
        )),
    );
    let outcome = tokio::time::timeout(
        Duration::from_secs(125),
        crate::session::subagent::child_runner::run_child_until_terminal(built.context, 10, &child),
    )
    .await
    .expect("missing owner must not wait beyond D");
    let ChildRunOutcome::HandoffFailed { failure, .. } = outcome else {
        panic!("expected capability failure: {outcome:?}");
    };
    assert!(failure
        .error
        .to_string()
        .contains("handoff_capability_unavailable"));
}

/// 交接 canonical 写入失败：结构化 HandoffFailed，不返回 HandedOff 或成功。
#[tokio::test(start_paused = true)]
async fn handoff_persistence_failure_is_not_reported_as_handed_off() {
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
        child_owner: Some(owner.clone()),
        ..Default::default()
    });
    store.restrict_to_history_read_only();
    TaskManagerPort::register(
        manager.as_ref(),
        BgTaskRegistration {
            task_id: "shell-task".into(),
            kind: BgTaskKind::Shell,
            summary: "permanent".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        },
    )
    .expect("the register fixture result is required for this assertion");
    child.queue().push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run shell"),
    ));
    let built = crate::session::subagent::build_v2_subagent_context(
        Some(child.clone()),
        Box::new(NestedResultLlm {
            initial_calls: Arc::new(AtomicUsize::new(0)),
            result_calls: Arc::new(AtomicUsize::new(0)),
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
        Some(crate::session::subagent::agent_id_from_child_thread(
            &child_id,
        )),
    );
    let permit = permit_for(&child, &built.context);
    let outcome = tokio::time::timeout(
        Duration::from_secs(125),
        crate::session::subagent::child_runner::run_child_until_terminal(built.context, 10, &child),
    )
    .await
    .expect("persistence failure must not wait beyond D");
    if let Some(permit) = permit {
        permit.finish(Err("handoff persistence failed".into()));
    }
    let ChildRunOutcome::HandoffFailed { failure, .. } = outcome else {
        panic!("持久化失败不得伪装成交接: {outcome:?}");
    };
    assert!(
        failure.error.to_string().contains("persistence")
            || failure
                .error
                .to_string()
                .contains("handoff_capability_unavailable")
    );
    assert!(
        owner.receipt().is_none(),
        "未提交的交接不得留 committed receipt"
    );
    assert!(owner.pending_receipt().is_some(), "冻结身份必须保留供重试");
    assert_eq!(manager.active_count(), 1);
}
