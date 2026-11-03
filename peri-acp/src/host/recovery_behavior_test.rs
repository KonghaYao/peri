//! 恢复协调行为：注入、去重、并发合并、Stop、实例复用与启动门。
//!
//! 与 `recovery_test.rs` 同夹具；本文件只放不需要 `ServerLoop` 的协调器用例。

use super::*;

/// 1. 根 `working`：进入后只注入一条提醒，启动后恰好跑一个 turn 并被 Receive 移交。
#[tokio::test]
async fn working_root_injects_one_reminder_and_launch_runs_one_turn() {
    let mut harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    harness.set_working(&thread_id).await;
    harness.spawn_continuation_scheduler();
    let recovery = harness.recovery();

    let report = recovery.enter(&thread_id).await.expect("恢复准备必须成功");
    assert_eq!(report.root_thread_id, thread_id);
    assert_eq!(report.entered_thread_id, thread_id);
    assert_eq!(report.recovered, vec![thread_id.clone()]);
    assert!(report.reused.is_empty());
    assert!(report.failures.is_empty());
    assert!(
        matches!(
            report.peripheral_tasks,
            peri_acp_types::ports::PeripheralTasks::Unknown { .. }
        ),
        "没有可查询的 workspace 池时必须是未知，不是空列表: {:?}",
        report.peripheral_tasks
    );

    let rows = harness.recovery_rows(&thread_id).await;
    assert_eq!(rows.len(), 1, "只允许一条恢复提醒");
    assert_eq!(rows[0].kind, peri_acp_types::session::MessageKind::Defer);
    assert_eq!(reminder_source(&rows[0]), "session_recovery");
    assert!(
        reminder_body(&rows[0]).contains("会话从中断后的已提交历史继续"),
        "正文必须说明从已提交历史继续：{}",
        reminder_body(&rows[0])
    );
    assert!(
        reminder_body(&rows[0]).contains("外围任务归属与存活状态：未知"),
        "阶段 3 不得把外围任务写成已知状态"
    );
    assert!(
        !harness.listener_started(&thread_id),
        "启动前不建立 listener"
    );
    assert!(!harness._mock.matched(), "启动前不得调用模型");

    recovery.launch(&report).await;
    wait_for("恢复 turn 调用模型", || async {
        harness._mock.matched()
    })
    .await;
    harness._mock.assert_async().await;
    assert!(harness.activation_allowed(&thread_id));
    assert!(harness.listener_started(&thread_id));
    // Receive 移交后提醒离开 MQ（进入历史），不再重复消费。
    wait_for("提醒被 Receive 移交", || async {
        harness.recovery_rows(&thread_id).await.is_empty()
    })
    .await;
}

/// 2. 重复进入同一根：不出现第二条提醒。
#[tokio::test]
async fn repeat_enter_keeps_a_single_reminder() {
    let harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    harness.set_working(&thread_id).await;
    let recovery = harness.recovery();

    let first = recovery.enter(&thread_id).await.unwrap();
    assert_eq!(first.recovered, vec![thread_id.clone()]);
    let second = recovery.enter(&thread_id).await.unwrap();
    assert!(
        second.recovered.is_empty(),
        "第二次进入不得再注入：{:?}",
        second.recovered
    );
    assert_eq!(
        second.skipped,
        vec![peri_acp_types::recovery::RecoverySkip {
            thread_id: thread_id.clone(),
            reason: RecoverySkipReason::AlreadyRunning,
        }]
    );
    assert_eq!(harness.recovery_rows(&thread_id).await.len(), 1);
}

/// 3. 并发进入同一根：合并为一次注入。
#[tokio::test]
async fn concurrent_enter_merges_into_one_reminder() {
    let harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    harness.set_working(&thread_id).await;
    let recovery = harness.recovery();

    let (first, second) = tokio::join!(recovery.enter(&thread_id), recovery.enter(&thread_id));
    let first = first.expect("并发进入不得失败");
    let second = second.expect("并发进入不得失败");
    let recovered = first.recovered.len() + second.recovered.len();
    assert_eq!(recovered, 1, "并发进入只能有一次真正注入");
    assert_eq!(harness.recovery_rows(&thread_id).await.len(), 1);
}

/// 4. 根 `idle` + child `working`：根不启动；child 独立装配并按自己的队列接纳提醒。
#[tokio::test]
async fn idle_root_does_not_start_and_working_child_admits_its_own_reminder() {
    let harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.set_working("child-b").await;
    let recovery = harness.recovery();

    let report = recovery.enter(&root).await.unwrap();
    assert_eq!(
        report.recovered,
        vec!["child-b".to_owned()],
        "只恢复 working child"
    );
    assert_eq!(
        report.skipped,
        vec![peri_acp_types::recovery::RecoverySkip {
            thread_id: root.clone(),
            reason: RecoverySkipReason::Idle,
        }]
    );
    assert!(harness.recovery_rows(&root).await.is_empty());
    assert_eq!(harness.recovery_rows("child-b").await.len(), 1);
    assert!(!harness._mock.matched(), "准备阶段不得调用模型");
    // 不改工作意图：根仍 idle、child 仍是磁盘上的 working（收尾由它自己的 turn 决定）。
    assert_eq!(
        harness
            .cfg
            .session_resources
            .load_session_meta(&root)
            .await
            .unwrap()
            .work_state,
        WorkState::Idle
    );
    assert_eq!(
        harness
            .cfg
            .session_resources
            .load_session_meta(&"child-b".to_owned())
            .await
            .unwrap()
            .work_state,
        WorkState::Working
    );
}

/// 5. 嵌套 A→B→C：三个 thread 各自在自己的 MQ 接纳提醒，任务列表只写直接子候选。
#[tokio::test]
async fn nested_working_chain_keeps_each_reminder_in_its_own_queue() {
    let harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.add_child("child-b", "child-c", &root).await;
    harness.set_working(&root).await;
    harness.set_working("child-b").await;
    harness.set_working("child-c").await;
    let recovery = harness.recovery();

    let report = recovery.enter(&root).await.unwrap();
    let mut recovered = report.recovered.clone();
    recovered.sort();
    let mut expected = vec![root.clone(), "child-b".to_owned(), "child-c".to_owned()];
    expected.sort();
    assert_eq!(recovered, expected, "范围内每个 working thread 各自恢复");
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    for thread in [&root, &"child-b".to_owned(), &"child-c".to_owned()] {
        assert_eq!(
            harness.recovery_rows(thread).await.len(),
            1,
            "{thread} 只允许一条自己的提醒"
        );
    }
    // 提醒的任务列表只写本 thread 的直接 working child。
    let body = reminder_body(&harness.recovery_rows(&root).await[0]);
    assert!(body.contains("child-b"), "正文: {body}");
    assert!(!body.contains("child-c"), "只写直接子候选，正文: {body}");
    let child_b = reminder_body(&harness.recovery_rows("child-b").await[0]);
    assert!(child_b.contains("child-c"), "正文: {child_b}");
}

/// 6. 准备期间已有显式 Stop：不注入、不启动。
#[tokio::test]
async fn stop_before_entry_injects_nothing_and_never_launches() {
    let harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    harness.set_working(&thread_id).await;
    harness
        .cfg
        .session_manager
        .get_session(&thread_id)
        .expect("会话运行时必须已登记")
        .activation
        .suppress();
    let recovery = harness.recovery();

    let report = recovery.enter(&thread_id).await.unwrap();
    assert!(report.recovered.is_empty());
    assert_eq!(
        report.skipped,
        vec![peri_acp_types::recovery::RecoverySkip {
            thread_id: thread_id.clone(),
            reason: RecoverySkipReason::Stopped,
        }]
    );
    assert!(harness.recovery_rows(&thread_id).await.is_empty());
    assert!(!harness.activation_allowed(&thread_id));

    // 即使调用方仍尝试启动，也不得建立 listener 或发出 continuation。
    recovery.launch(&report).await;
    assert!(!harness.listener_started(&thread_id));
    assert!(!harness._mock.matched());
}

/// 7. 已有活实例（turn 正在跑）：复用，不重复注入。
#[tokio::test]
async fn existing_turn_is_reused_without_a_second_reminder() {
    let harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    harness.set_working(&thread_id).await;
    {
        let mut states = harness.sessions.lock().await;
        states.get_mut(&thread_id).unwrap().cancel_token =
            Some(tokio_util::sync::CancellationToken::new());
    }
    let recovery = harness.recovery();

    let report = recovery.enter(&thread_id).await.unwrap();
    assert_eq!(report.reused, vec![thread_id.clone()]);
    assert!(report.recovered.is_empty());
    assert!(harness.recovery_rows(&thread_id).await.is_empty());
}
