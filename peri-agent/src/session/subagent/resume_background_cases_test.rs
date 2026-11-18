// Responsibility-specific cases share the original test namespace and fixtures.

/// bg resume（slice 5）：Background 模式 → 新 task_id（bg- 前缀）、
/// TaskManager 注册 Running、放行后完成收尾 done + registry 移除
#[tokio::test]
async fn test_resume_subagent_background_mode_done() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    crate::session::subagent::tests::preset_resumable_thread(&store, &thread_id, None).await;
    store
        .append_messages(&thread_id, &[BaseMessage::human("task")])
        .await
        .expect("the append messages fixture result is required for this assertion");

    let task_manager = Arc::new(TaskManager::new());
    let (gate, release_tx) = GateLLM::new();
    let config = crate::session::subagent::tests::resume_config_with(
        store.clone(),
        thread_id.clone(),
        crate::session::test_resources::mock::model::fixture_source(
            std::sync::Arc::new(gate.clone()),
            "fixture-scripted",
        ),
        Some(Arc::clone(&task_manager)),
        None,
    );
    let spawned = SessionFactory::resume_subagent(None, config)
        .await
        .expect("bg resume ok");

    // 新 task_id：与 thread_id 分离、bg- 前缀
    let task_id = spawned.task_id;
    assert!(task_id.starts_with("bg-"), "task_id 格式 bg-{{uuid}}");
    assert_ne!(task_id, thread_id, "task_id 与 thread_id 分离");

    // TaskManager 注册（gate 挂起 LLM，任务仍 Running）
    let tasks = task_manager.list_tasks();
    assert!(
        tasks.iter().any(
            |(id, status, _)| id == &task_id && matches!(status, BackgroundTaskStatus::Running)
        ),
        "bg resume 必须注册 TaskManager，tasks: {:?}",
        tasks
    );

    // 放行 → 完成：status done + registry 移除（complete 后仅保留 Running）。
    // 先确认 LLM 已被调用（挂起生效）再放行，保证任务确实进入执行。
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while gate.calls() == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("bg 任务应进入 LLM 调用");
    let _ = release_tx.send(());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if store.statuses().last().map(|(_, s)| s.as_str()) == Some("done") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("bg 任务应在超时前完成");
    assert_eq!(
        task_manager.active_count(),
        0,
        "bg 完成后 registry 移除任务"
    );
}

/// bg resume cancelled 分支：Reason 内取消 → bg 中断收尾写 "cancelled"
#[tokio::test]
async fn test_resume_subagent_background_mode_cancelled() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    crate::session::subagent::tests::preset_resumable_thread(&store, &thread_id, None).await;

    let task_manager = Arc::new(TaskManager::new());
    let token = CancellationToken::new();
    let (gate, entered_rx) = CancelGateLLM::new();
    let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
    let completed_tx = Arc::new(std::sync::Mutex::new(Some(completed_tx)));
    let mut config = crate::session::subagent::tests::resume_config_with(
        store.clone(),
        thread_id.clone(),
        crate::session::test_resources::mock::model::fixture_source(
            std::sync::Arc::new(gate),
            "fixture-scripted",
        ),
        Some(Arc::clone(&task_manager)),
        Some(token.clone()),
    );
    config.on_bg_complete = Some(Arc::new(move |result, _kind| {
        if let Some(completed) = completed_tx
            .lock()
            .expect("completed tx lock mutex must remain unpoisoned throughout the test")
            .take()
        {
            let _ = completed.send(result.clone());
        }
        Ok(())
    }));
    let spawned = SessionFactory::resume_subagent(None, config)
        .await
        .expect("bg resume ok");
    assert!(spawned.task_id.starts_with("bg-"), "恒为后台任务 ID");
    entered_rx
        .await
        .expect("background subagent 必须进入 Reason");
    token.cancel();

    let completed = completed_rx.await.expect("on_bg_complete 必须收到终态");
    assert!(!completed.success, "取消后 background completion 不得成功");
    assert!(
        completed.output.contains("interrupted"),
        "background completion 必须呈现 interrupted: {}",
        completed.output
    );
    assert_eq!(
        store.statuses().last().map(|(_, s)| s.as_str()),
        Some("cancelled"),
        "bg 中断收尾必须写 cancelled"
    );
}

/// bg resume 注册失败回滚（review MEDIUM-1，路径 1：task_manager 缺失）：
/// spawn_background_subagent 注册前置失败 → Err 携带 thread_id + status 回滚至
/// 原值（不被 active 卡死）+ 提供 task_manager 后可再次恢复
#[tokio::test]
async fn test_resume_subagent_bg_registration_failure_rolls_back() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    crate::session::subagent::tests::preset_resumable_thread(&store, &thread_id, None).await;

    // 不传 task_manager → 注册失败（任务未执行）
    let config = crate::session::subagent::tests::resume_config_with(
        store.clone(),
        thread_id.clone(),
        crate::session::test_resources::mock::model::fixture_source(
            std::sync::Arc::new(EchoLLM),
            "fixture-scripted",
        ),
        None,
        None,
    );
    let err = crate::session::subagent::tests::resume_err(None, config).await;
    assert!(
        err.contains(&thread_id),
        "注册失败错误必须携带 thread_id，got: {}",
        err
    );
    assert!(
        err.contains("no task manager configured"),
        "错误须带注册失败原因，got: {}",
        err
    );

    // status 回滚至原值（done），未被 active 卡死
    let meta = store
        .load_meta(&thread_id)
        .await
        .expect("the load meta fixture result is required for this assertion");
    assert_eq!(
        meta.agent_status,
        AgentStatus::Done,
        "注册失败必须回滚 status 至原值"
    );
    {
        let statuses = store.statuses();
        let seq: Vec<&str> = statuses.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(seq, vec!["done", "active", "done"], "active 后回滚原值");
    }

    // 回滚后可再次恢复（提供 task_manager）→ bg 正常完成收尾 done
    let task_manager = Arc::new(TaskManager::new());
    let config = crate::session::subagent::tests::resume_config_with(
        store.clone(),
        thread_id.clone(),
        crate::session::test_resources::mock::model::fixture_source(
            std::sync::Arc::new(EchoLLM),
            "fixture-scripted",
        ),
        Some(Arc::clone(&task_manager)),
        None,
    );
    let spawned = SessionFactory::resume_subagent(None, config)
        .await
        .expect("回滚后可再次恢复");
    assert_eq!(spawned.child_thread_id, thread_id);
    assert!(spawned.task_id.starts_with("bg-"));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if store.statuses().last().map(|(_, s)| s.as_str()) == Some("done") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("bg 任务应在超时前完成");
}

/// bg resume 不受 Agent 类并发上限阻挡（原 AGENT_LIMIT=3 已移除）：已有 5 个
/// Agent 任务在跑时，第 6 个后台恢复仍必须成功、正常收尾 done，且既有条目不被
/// 丢弃/改写。
#[tokio::test]
async fn test_resume_subagent_bg_beyond_previous_agent_cap() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    crate::session::subagent::tests::preset_resumable_thread(&store, &thread_id, None).await;
    store
        .append_messages(&thread_id, &[BaseMessage::human("task")])
        .await
        .expect("the append messages fixture result is required for this assertion");

    let task_manager = Arc::new(TaskManager::new());
    // 5 个在跑 Agent 任务（>3，覆盖已取消的上限）
    for i in 0..5 {
        task_manager
            .register_with_kind(BackgroundTask {
                id: format!("placeholder-{}", i),
                agent_name: "placeholder".to_string(),
                prompt_summary: "placeholder".to_string(),
                status: BackgroundTaskStatus::Running,
                started_at: std::time::Instant::now(),
                chrono_started_at: chrono::Utc::now(),
                kind: BgTaskKind::Agent,
                cancel_handle: BgCancelHandle::Kill(None),
                cancel_token: None,
                pid: None,
                output_preview: None,
                agent_inbox: None,
                initiator_session_id: None,
                owner_session_id: None,
                owner_identity: None,
            })
            .expect("占位任务注册应成功（Agent 类不限额）");
    }
    assert_eq!(task_manager.active_count(), 5);

    let config = crate::session::subagent::tests::resume_config_with(
        store.clone(),
        thread_id.clone(),
        crate::session::test_resources::mock::model::fixture_source(
            std::sync::Arc::new(EchoLLM),
            "fixture-scripted",
        ),
        Some(Arc::clone(&task_manager)),
        None,
    );
    let spawned = SessionFactory::resume_subagent(None, config)
        .await
        .expect("超过 3 个在跑任务时 bg 恢复不得被拒绝");
    assert_eq!(spawned.child_thread_id, thread_id);
    let task_id = spawned.task_id;
    assert!(task_id.starts_with("bg-"), "task_id 格式 bg-{{uuid}}");

    // 任务完成：status done；registry 收敛回 5 个占位任务（无泄漏/无丢弃）
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if store.statuses().last().map(|(_, s)| s.as_str()) == Some("done") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("bg 任务应在超时前完成");
    assert_eq!(
        task_manager.active_count(),
        5,
        "完成的任务移除后仅剩占位任务"
    );
    assert!(
        task_manager
            .list_tasks()
            .iter()
            .all(|(id, _, _)| id.starts_with("placeholder-")),
        "既有占位条目不得被改写"
    );
}

/// bg resume 执行权前置失败：session execution scope 已关闭时，恢复在 claim
/// （写 active）之前被拒——不产生执行，也不留下 active 脏状态。
///
/// 历史：本用例原以 `register_with_kind` 撞 per-kind 上限（AGENT_LIMIT=3）制造
/// 注册失败；Agent 类后台任务取消并发上限后（放开并行委派），该类注册失败不复
/// 存在，改用 scope 关闭作为可确定复现的执行前失败。执行前失败回滚（active →
/// 原值）的完整契约由 `test_resume_subagent_bg_registration_failure_rolls_back`
/// 覆盖（task_manager 缺失路径）。
#[tokio::test]
async fn test_resume_subagent_bg_scope_closed_rejected_before_claim() {
    use peri_acp_types::tasks::TaskManager as _;
    use peri_acp_types::tasks::TaskShutdownReport;

    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    crate::session::subagent::tests::preset_resumable_thread(&store, &thread_id, None).await;

    let task_manager = Arc::new(TaskManager::new());
    // 关闭 session execution scope：恢复侧外部执行权申请被拒
    assert_eq!(
        task_manager.shutdown().await,
        TaskShutdownReport::Complete,
        "无在跑任务时空闲关闭应为 Complete"
    );

    let config = crate::session::subagent::tests::resume_config_with(
        store.clone(),
        thread_id.clone(),
        crate::session::test_resources::mock::model::fixture_source(
            std::sync::Arc::new(EchoLLM),
            "fixture-scripted",
        ),
        Some(Arc::clone(&task_manager)),
        None,
    );
    let err = crate::session::subagent::tests::resume_err(None, config).await;
    assert!(
        err.contains("closing"),
        "错误须带执行前失败原因，got: {}",
        err
    );

    // claim 之前失败：thread 从未被写成 active，状态保持原值
    let meta = store
        .load_meta(&thread_id)
        .await
        .expect("the load meta fixture result is required for this assertion");
    assert_eq!(
        meta.agent_status,
        AgentStatus::Done,
        "执行前被拒不得改写 thread 状态"
    );
    {
        let statuses = store.statuses();
        let seq: Vec<&str> = statuses.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(seq, vec!["done"], "claim 之前失败不写 active");
    }
    assert_eq!(task_manager.active_count(), 0, "不得留下后台任务条目");
}
