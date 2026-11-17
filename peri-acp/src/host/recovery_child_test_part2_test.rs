/// 装配期间已有 turn 取得会话：拒绝路径不得回收执行方仍在使用的实例。
#[tokio::test]
async fn live_turn_during_assembly_is_not_rolled_back() {
    let harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-live-turn", &root).await;
    harness.add_child(&root, "child-b", &root).await;
    harness.set_working("child-live-turn").await;
    let gate = admission_gate::install(
        "child-live-turn",
        admission_gate::PausePoint::AfterColdAssembly,
    );
    let recovery = harness.recovery();
    let entered = root.clone();
    let task = tokio::spawn(async move { recovery.enter(&entered).await });

    gate.wait_arrived(1).await;
    {
        // 普通 prompt 与恢复准入共用 sessions 锁发布令牌：模拟 turn 在装配后取得会话。
        let mut states = harness.sessions.lock().await;
        states.get_mut("child-live-turn").unwrap().cancel_token =
            Some(tokio_util::sync::CancellationToken::new());
    }
    set_completion_target(&harness, "child-live-turn", Some("child-b")).await;
    gate.release(1);
    let report = task.await.unwrap().expect("拒绝只记逐项失败");
    admission_gate::uninstall(
        "child-live-turn",
        admission_gate::PausePoint::AfterColdAssembly,
    );

    assert!(
        report
            .failures
            .iter()
            .any(|failure| failure.thread_id == "child-live-turn"),
        "{report:?}"
    );
    assert!(
        harness
            .sessions
            .lock()
            .await
            .get("child-live-turn")
            .is_some_and(|state| state.cancel_token.is_some()),
        "turn 拥有的实例不得被回滚"
    );
    assert!(
        harness
            .cfg
            .session_manager
            .get_session("child-live-turn")
            .is_some(),
        "turn 拥有的 manager 登记不得移除"
    );
}

/// 装配期间 child 收尾为终态：按 idle 跳过并回收本次装配。
#[tokio::test]
async fn terminal_during_assembly_rolls_back_the_assembled_child() {
    let harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-terminal", &root).await;
    harness.set_working("child-terminal").await;
    let gate = admission_gate::install(
        "child-terminal",
        admission_gate::PausePoint::AfterColdAssembly,
    );
    let recovery = harness.recovery();
    let entered = root.clone();
    let task = tokio::spawn(async move { recovery.enter(&entered).await });

    gate.wait_arrived(1).await;
    assert!(harness
        .sessions
        .lock()
        .await
        .get("child-terminal")
        .is_some());
    set_agent_status(&harness, "child-terminal", AgentStatus::Done).await;
    gate.release(1);
    let report = task.await.unwrap().expect("终态只记跳过");
    admission_gate::uninstall(
        "child-terminal",
        admission_gate::PausePoint::AfterColdAssembly,
    );

    assert!(
        report
            .skipped
            .contains(&peri_acp_types::recovery::RecoverySkip {
                thread_id: "child-terminal".to_owned(),
                reason: RecoverySkipReason::Idle,
            }),
        "{report:?}"
    );
    assert!(
        harness
            .sessions
            .lock()
            .await
            .get("child-terminal")
            .is_none(),
        "终态跳过必须回收本次装配"
    );
    assert!(harness
        .cfg
        .session_manager
        .get_session("child-terminal")
        .is_none());
    assert!(harness.recovery_rows("child-terminal").await.is_empty());
}

/// 7. 恢复 turn 被取消：终态 `cancelled`、工作意图 `stopped`，结果仍入父 MQ。
#[tokio::test]
async fn recovered_child_cancelled_settles_cancelled_and_stopped() {
    let harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.set_working("child-b").await;
    let recovery = harness.recovery();
    recovery.enter(&root).await.unwrap();

    recovery::deliver_child_turn(
        &harness.cfg,
        &harness.sessions,
        "child-b",
        &delivery(&root, None),
        ChildTurnOutcome::Finished {
            stop_reason: "cancelled",
        },
    )
    .await;

    let meta = settled_meta(&harness, "child-b").await;
    assert_eq!(meta.agent_status, AgentStatus::Cancelled);
    assert_eq!(meta.work_state, WorkState::Stopped);
    assert_eq!(parent_notices(&harness, &root).await.len(), 1);
}

/// 8. 恢复 turn 明确失败：终态 `error`、工作意图 `idle`，结果仍入父 MQ。
#[tokio::test]
async fn recovered_child_failure_settles_error_and_idle() {
    let harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.set_working("child-b").await;
    let recovery = harness.recovery();
    recovery.enter(&root).await.unwrap();

    recovery::deliver_child_turn(
        &harness.cfg,
        &harness.sessions,
        "child-b",
        &delivery(&root, None),
        ChildTurnOutcome::Failed {
            code: -32603,
            message: "recovered child failed",
        },
    )
    .await;

    let meta = settled_meta(&harness, "child-b").await;
    assert_eq!(meta.agent_status, AgentStatus::Error);
    assert_eq!(meta.work_state, WorkState::Idle);
    assert_eq!(parent_notices(&harness, &root).await.len(), 1);
}

/// 9. run_prompt 早失败（未进入 finish_prompt_turn）：本轮没有可结算的终态，
///    不伪造完成/失败，也不投递父级结果；保留 working 与恢复提醒供下次进入重试。
#[tokio::test]
async fn recovered_child_early_failure_keeps_intent_without_fake_terminal() {
    let mut harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.set_working("child-b").await;
    harness.spawn_continuation_scheduler();
    let recovery = harness.recovery();
    let parent_manager = harness
        .cfg
        .session_manager
        .get_session(&root)
        .expect("root session is published")
        .task_manager
        .clone();
    let mut events = parent_manager.subscribe_events();

    let report = recovery.enter(&root).await.unwrap();
    assert_eq!(report.recovered, vec!["child-b".to_owned()]);
    // 装配成功后移除冻结上下文：run_prompt 会在进入收尾前失败。
    {
        let mut states = harness.sessions.lock().await;
        states.get_mut("child-b").unwrap().frozen = None;
    }

    recovery.launch(&report).await;
    // 早失败不产生终态结算，因此没有父级结果通知可等；等调度器收到续跑请求后给
    // 失败路径留出完成窗口。
    crate::host::requests::tests::recovery_tests::wait_for("续跑请求进入调度器", || async {
        harness.order.lock().unwrap().contains(&"continuation")
    })
    .await;
    crate::host::requests::tests::recovery_tests::wait_for(
        "父任务登记出现并收尾",
        || async {
            parent_manager
                .snapshot()
                .tasks
                .iter()
                .any(|task| task.task_id == "child-b")
        },
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let meta = settled_meta(&harness, "child-b").await;
    // 本次 attempt 未进入 Receive：不伪造终态，也不向父级投递结果事实；保留
    // `working` 与原提醒，供下一次显式进入重试（设计 §5.1）。
    assert_eq!(meta.agent_status, AgentStatus::Active);
    assert_eq!(meta.work_state, WorkState::Working);
    assert!(
        entries(&harness, "child-b")
            .await
            .iter()
            .any(|entry| entry.policy.ensures_processing()),
        "早失败不会消费恢复提醒"
    );
    assert!(parent_notices(&harness, &root).await.is_empty());
    assert!(!harness._mock.matched(), "早失败不得调用模型");
    // 父任务登记必须收尾：早失败没有结果，但不能把父任务留在 pending。
    let (started, completed) = parent_binding_tests::child_task_events(&mut events, "child-b");
    assert!(started, "早失败前父任务必须已经登记");
    assert_eq!(completed, None, "早失败不得伪造父任务完成");
    assert_eq!(parent_manager.active_count(), 0);
    assert_eq!(
        parent_manager
            .snapshot()
            .tasks
            .iter()
            .find(|task| task.task_id == "child-b")
            .map(|task| task.status.as_str()),
        Some("cancelled"),
        "早失败按取消收尾父任务投影"
    );
    assert!(!harness
        .cfg
        .session_manager
        .get_session(&root)
        .expect("root session is published")
        .active_agents
        .contains_key(&ThreadId::from("child-b")));
}

/// 11. 遗留 `Active + Idle` 的 child：进入根时收敛为 `Error + Idle`，
///     不注入提醒、不自动运行；随后显式 resume 通过 Active 检查。
#[tokio::test]
async fn stale_active_idle_child_settles_error_and_explicit_resume_works() {
    let harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    let recovery = harness.recovery();

    let report = recovery.enter(&root).await.expect("恢复准备必须成功");
    assert!(report.recovered.is_empty(), "{report:?}");
    let meta = settled_meta(&harness, "child-b").await;
    assert_eq!(meta.agent_status, AgentStatus::Error);
    assert_eq!(meta.work_state, WorkState::Idle, "工作意图保持原值");
    assert!(
        entries(&harness, "child-b").await.is_empty(),
        "收敛不注入任何提醒"
    );
    assert!(!harness._mock.matched(), "收敛不得启动 turn");

    harness
        .cfg
        .session_resources
        .claim_child_resume(&ThreadId::from("child-b"), &ThreadId::from(root.clone()))
        .await
        .expect("收敛后显式 resume 必须通过 Active 检查");
}

/// 12. 遗留 `Active + Stopped`：收敛为 `Cancelled + Stopped`。
#[tokio::test]
async fn stale_active_stopped_child_settles_cancelled() {
    let harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness
        .cfg
        .session_resources
        .set_work_state(&"child-b".to_owned(), WorkState::Stopped)
        .await
        .unwrap();
    let recovery = harness.recovery();

    recovery.enter(&root).await.expect("恢复准备必须成功");

    let meta = settled_meta(&harness, "child-b").await;
    assert_eq!(meta.agent_status, AgentStatus::Cancelled);
    assert_eq!(meta.work_state, WorkState::Stopped, "工作意图保持原值");
    assert!(entries(&harness, "child-b").await.is_empty());
    assert!(!harness._mock.matched());
}

/// 12b. 模拟重启：resume 成功留下的 `Active + Working` 会被恢复流程接纳；
/// 同一排队事实上的 `Stopped` 仍不自动恢复。
#[tokio::test]
async fn restart_admits_working_child_with_pending_prompt_but_not_stopped() {
    let harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-working", &root).await;
    harness.add_child(&root, "child-stopped", &root).await;
    for child in ["child-working", "child-stopped"] {
        harness
            .queue(child)
            .push_async(peri_acp_types::session::QueuedMessage::defer(
                MessageSource::SubAgentComplete,
                peri_acp_types::messages::BaseMessage::human("pending prompt"),
            ))
            .await;
    }
    harness
        .cfg
        .session_resources
        .set_work_state(&"child-working".to_owned(), WorkState::Working)
        .await
        .unwrap();
    harness
        .cfg
        .session_resources
        .set_work_state(&"child-stopped".to_owned(), WorkState::Stopped)
        .await
        .unwrap();
    let recovery = harness.recovery();

    let report = recovery.enter(&root).await.expect("恢复准备必须成功");
    assert_eq!(
        report.recovered,
        vec!["child-working".to_owned()],
        "只有持久工作意图为 working 的 child 被接纳"
    );
    assert!(
        !report
            .recovered
            .iter()
            .any(|thread| thread == "child-stopped"),
        "Stopped child 不得自动恢复: {report:?}"
    );
    // 遗留 Active 由收敛器结算为 Cancelled，但工作意图保持 Stopped：下一次进入
    // 仍不会自动恢复。
    let stopped = settled_meta(&harness, "child-stopped").await;
    assert_eq!(stopped.agent_status, AgentStatus::Cancelled);
    assert_eq!(stopped.work_state, WorkState::Stopped);
    assert_eq!(
        entries(&harness, "child-working").await.len(),
        2,
        "原 prompt 保留，另注入一条本 thread 的恢复提醒"
    );
    assert_eq!(
        entries(&harness, "child-stopped").await.len(),
        1,
        "Stopped child 只保留原 prompt，不注入恢复提醒"
    );
    assert!(harness.recovery_rows("child-stopped").await.is_empty());
    assert!(!harness._mock.matched(), "准备阶段不得调用模型");
}

/// 13. 本进程活实例（turn 在途）不被收敛。
#[tokio::test]
async fn live_child_instance_is_not_converged() {
    let mut harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.load_session("child-b").await;
    {
        let mut states = harness.sessions.lock().await;
        states
            .get_mut("child-b")
            .expect("child session is published")
            .cancel_token = Some(tokio_util::sync::CancellationToken::new());
    }
    let recovery = harness.recovery();

    recovery.enter(&root).await.expect("恢复准备必须成功");

    assert_eq!(
        settled_meta(&harness, "child-b").await.agent_status,
        AgentStatus::Active,
        "活实例不得被收敛"
    );
}

/// 14. close 未结清的 child 不被收敛。
#[tokio::test]
async fn closing_child_is_not_converged() {
    let harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness
        .cfg
        .session_resources
        .mark_session_closing(&ThreadId::from("child-b"))
        .await
        .unwrap();
    let recovery = harness.recovery();

    recovery.enter(&root).await.expect("恢复准备必须成功");

    assert_eq!(
        settled_meta(&harness, "child-b").await.agent_status,
        AgentStatus::Active,
        "close 未结清的 child 由 close 自己结算"
    );
}

/// 15. 收敛不触碰 `working` 候选，也不触碰进入的根。
#[tokio::test]
async fn convergence_skips_working_candidates_and_the_root() {
    let harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.set_working("child-b").await;
    let recovery = harness.recovery();

    recovery.enter(&root).await.expect("恢复准备必须成功");

    assert_eq!(
        settled_meta(&harness, &root).await.agent_status,
        AgentStatus::Active,
        "进入的根自身不得被收敛"
    );
    assert_eq!(
        settled_meta(&harness, "child-b").await.agent_status,
        AgentStatus::Active,
        "working 候选不是中断遗留"
    );
    assert_eq!(
        settled_meta(&harness, "child-b").await.work_state,
        WorkState::Working
    );
}

/// 10. 重复结算：同一 child 的终态只投递一次结果，重复调用是 no-op。
#[tokio::test]
async fn repeated_recovered_child_settlement_is_idempotent() {
    let harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.set_working("child-b").await;
    let recovery = harness.recovery();
    recovery.enter(&root).await.unwrap();

    for _ in 0..2 {
        recovery::deliver_child_turn(
            &harness.cfg,
            &harness.sessions,
            "child-b",
            &delivery(&root, None),
            ChildTurnOutcome::Finished {
                stop_reason: "end_turn",
            },
        )
        .await;
    }

    let meta = settled_meta(&harness, "child-b").await;
    assert_eq!(meta.agent_status, AgentStatus::Done);
    assert_eq!(meta.work_state, WorkState::Idle);
    assert_eq!(
        parent_notices(&harness, &root).await.len(),
        1,
        "重复结算不得产生第二条结果"
    );
}

/// 同一 child 再次恢复：新恢复提醒带来新种子，结果不得被上一条历史身份跳过。
#[tokio::test]
async fn second_recovery_settlement_uses_a_new_delivery_identity() {
    let harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.set_working("child-b").await;
    let parent = ThreadId::from(root.clone());

    recovery::deliver_child_turn(
        &harness.cfg,
        &harness.sessions,
        "child-b",
        &delivery(&root, Some(peri_acp_types::messages::MessageId::new())),
        ChildTurnOutcome::Finished {
            stop_reason: "end_turn",
        },
    )
    .await;
    assert_eq!(parent_notices(&harness, &root).await.len(), 1);

    // 第二次恢复必须先经过一次显式 resume：认领把 child 写回 active，随后再次
    // 进入 working。认领 handle 未声明开始/失败即 drop 不会回写，保留认领后的
    // active 事实，正好模拟「新一轮恢复已重新认领、尚未结算」。
    let claim = harness
        .cfg
        .session_resources
        .claim_child_resume(&ThreadId::from("child-b"), &parent)
        .await
        .expect("显式 resume 必须成功认领");
    drop(claim);
    harness.set_working("child-b").await;
    let resumed = settled_meta(&harness, "child-b").await;
    assert_eq!(resumed.agent_status, AgentStatus::Active);
    assert_eq!(resumed.work_state, WorkState::Working);

    recovery::deliver_child_turn(
        &harness.cfg,
        &harness.sessions,
        "child-b",
        &delivery(&root, Some(peri_acp_types::messages::MessageId::new())),
        ChildTurnOutcome::Failed {
            code: -32603,
            message: "second recovery run failed",
        },
    )
    .await;
    assert_eq!(
        parent_notices(&harness, &root).await.len(),
        2,
        "第二次恢复的结果不得被上一条历史身份吞掉"
    );
    let settled = settled_meta(&harness, "child-b").await;
    assert_eq!(settled.agent_status, AgentStatus::Error);
    assert_eq!(settled.work_state, WorkState::Idle);
}

/// close 成功后的 child 不得再被恢复。
///
/// 当前 close 只结清 close intent、不写终态（`lifecycle.rs:6-28`），
/// `Active + Working` 仍会被下一次进入续跑。是否让 close 写终态需要产品裁决，
/// 见 `spec/issues/2026-11-10-p1-terminal-child-recovery.md`。
#[ignore = "close 不写终态，需产品裁决"]
#[tokio::test]
async fn closed_child_is_not_recovered() {
    use peri_acp_types::session_resources::CloseSettlement;

    let mut harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.set_working("child-b").await;
    let resources = Arc::clone(&harness.cfg.session_resources);
    let child = ThreadId::from("child-b");
    resources.mark_session_closing(&child).await.unwrap();
    resources.finish_close(&child).await.unwrap();
    assert_eq!(
        resources.close_settlement(&child).await.unwrap(),
        CloseSettlement::Finished,
        "close 必须已结清"
    );
    harness.spawn_continuation_scheduler();
    let recovery = harness.recovery();

    let report = recovery.enter(&root).await.unwrap();
    assert!(
        report.recovered.is_empty(),
        "已关闭的 child 不得被恢复：{report:?}"
    );
    assert!(
        harness.recovery_rows("child-b").await.is_empty(),
        "已关闭的 child 不得收到恢复提醒"
    );
    recovery.launch(&report).await;
    assert!(!harness._mock.matched(), "已关闭的 child 不得调用模型");
}

/// workflow 形状 child（hidden、父链直连根）在 run 结算后既不是恢复候选，
/// 也不会在进入父会话时产生准备失败；与 executor 的结算回归共同锁定「不再重复
/// 唤醒已完成 workflow child」。
#[tokio::test]
async fn settled_workflow_child_is_not_a_candidate_or_failure() {
    let mut harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "workflow-child", &root).await;
    harness.set_working("workflow-child").await;
    harness
        .cfg
        .session_resources
        .settle_terminal_without_result(
            &ThreadId::from("workflow-child".to_owned()),
            AgentStatus::Done,
            WorkState::Idle,
        )
        .await
        .unwrap();

    let entered = ThreadId::from(root.clone());
    let scope = harness
        .cfg
        .session_resources
        .load_recovery_scope(&entered)
        .await
        .unwrap();
    assert!(
        scope.candidates.is_empty(),
        "settled workflow child must not be a candidate: {:?}",
        scope.candidates
    );
    assert!(
        harness
            .cfg
            .session_resources
            .load_interrupted_threads(&entered)
            .await
            .unwrap()
            .is_empty(),
        "settled workflow child must not need Active convergence"
    );

    harness.spawn_continuation_scheduler();
    let report = harness.recovery().enter(&entered).await.unwrap();
    assert!(
        report.recovered.is_empty(),
        "settled workflow child must not be recovered: {:?}",
        report.recovered
    );
    assert!(
        report.failures.is_empty(),
        "settled workflow child must not produce recovery failures: {:?}",
        report.failures
    );
}
