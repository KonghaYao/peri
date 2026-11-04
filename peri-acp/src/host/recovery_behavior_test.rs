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

/// 2b. 响应发送失败（discard）后再次进入：旧提醒被采纳而不是永久 AlreadyRunning。
#[tokio::test]
async fn discard_keeps_reminder_and_next_enter_adopts_without_duplicate() {
    let mut harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    harness.set_working(&thread_id).await;
    harness.spawn_continuation_scheduler();
    let recovery = harness.recovery();

    let first = recovery.enter_prepared(&thread_id).await.unwrap();
    assert_eq!(first.report.recovered, vec![thread_id.clone()]);
    recovery.discard(&first);
    assert_eq!(
        harness.recovery_rows(&thread_id).await.len(),
        1,
        "discard 只丢弃启动资格，提醒仍留在 MQ"
    );

    let second = recovery.enter(&thread_id).await.unwrap();
    assert_eq!(
        second.recovered,
        vec![thread_id.clone()],
        "下一次进入必须采纳旧提醒：{second:?}"
    );
    assert_eq!(
        harness.recovery_rows(&thread_id).await.len(),
        1,
        "采纳不得重复入队"
    );

    recovery.launch(&second).await;
    wait_for("采纳后的恢复 turn 调用模型", || async {
        harness._mock.matched()
    })
    .await;
    harness._mock.assert_async().await;
}

/// 2c. 同来源的 Info（诊断提醒）不构成恢复 entry，不得抑制注入。
#[tokio::test]
async fn info_notice_does_not_suppress_recovery() {
    let harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    harness.set_working(&thread_id).await;
    let notice = recovery_shaped_reminder(&thread_id);
    harness
        .queue(&thread_id)
        .enqueue(peri_acp_types::session::QueuedMessage::system_reminder(
            peri_acp_types::session::MessageKind::Info,
            peri_acp_types::session::MessageSource::SessionRecovery,
            notice,
        ))
        .await
        .unwrap();

    let report = harness.recovery().enter(&thread_id).await.unwrap();
    assert_eq!(
        report.recovered,
        vec![thread_id.clone()],
        "Info 提醒不是待消费的恢复 entry"
    );
    let rows = harness.recovery_rows(&thread_id).await;
    assert_eq!(rows.len(), 2, "必须新增一条 Defer entry：{rows:?}");
    assert_eq!(
        rows.iter()
            .filter(|row| row.kind == peri_acp_types::session::MessageKind::Defer)
            .count(),
        1
    );
}

/// 2d. MQ 查询失败必须让进入显式失败，不能静默注入第二条提醒。
#[tokio::test]
async fn mq_query_failure_fails_entry_instead_of_reinjecting() {
    let harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    harness.set_working(&thread_id).await;
    use sqlx::Connection as _;
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(&harness.db_path),
    )
    .await
    .unwrap();
    sqlx::query("DROP TABLE thread_mq")
        .execute(&mut connection)
        .await
        .unwrap();
    drop(connection);

    let error = harness
        .recovery()
        .enter(&thread_id)
        .await
        .expect_err("MQ 查询失败不得被当作没有提醒");
    assert_eq!(
        error.kind(),
        peri_acp_types::recovery::RecoveryErrorKind::Unavailable,
        "缺少 MQ 表必须作为不可用失败上报：{error}"
    );
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

/// 8. 准备窗口内发生 Stop：恢复放行不得清除停止意图，也不得入队提醒或启动。
#[tokio::test]
async fn stop_during_the_preparation_window_blocks_admission() {
    use crate::host::recovery::admission_gate::{self, PausePoint};

    let harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    harness.set_working(&thread_id).await;
    let recovery = Arc::new(harness.recovery());
    let gate = admission_gate::install(&thread_id, PausePoint::BeforeAdmit);

    let entering = {
        let recovery = Arc::clone(&recovery);
        let thread_id = thread_id.clone();
        tokio::spawn(async move { recovery.enter_prepared(&thread_id).await })
    };
    gate.wait_arrived(1).await;
    harness
        .cfg
        .session_manager
        .get_session(&thread_id)
        .expect("会话运行时必须已登记")
        .activation
        .suppress();
    gate.release(1);

    let entered = entering
        .await
        .expect("进入任务不得 panic")
        .expect("准备本身不是请求错误");
    assert!(entered.report.recovered.is_empty());
    assert_eq!(
        entered.report.skipped,
        vec![peri_acp_types::recovery::RecoverySkip {
            thread_id: thread_id.clone(),
            reason: RecoverySkipReason::Stopped,
        }]
    );
    assert!(
        harness.recovery_rows(&thread_id).await.is_empty(),
        "Stop 之后不得入队恢复提醒"
    );
    assert!(!harness.activation_allowed(&thread_id));

    recovery.launch(&entered.report).await;
    assert!(!harness.listener_started(&thread_id));
    assert!(!harness._mock.matched());
    admission_gate::uninstall(&thread_id, PausePoint::BeforeAdmit);
}

/// 9. 三路并发进入共用同一把根锁：等待者未退出前不得删表重建第二把门。
#[tokio::test]
async fn three_way_enter_shares_one_gate_and_injects_once() {
    use crate::host::recovery::admission_gate::{self, PausePoint};
    use peri_acp_types::thread::ThreadId;

    let harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    let root = ThreadId::from(thread_id.clone());
    harness.set_working(&thread_id).await;
    let recovery = Arc::new(harness.recovery());
    let gate = admission_gate::install(&thread_id, PausePoint::InsideGate);

    let spawn_enter = |recovery: Arc<crate::host::recovery::HostSessionRecovery>,
                       thread_id: String| {
        tokio::spawn(async move { recovery.enter_prepared(&ThreadId::from(thread_id)).await })
    };

    let first = spawn_enter(Arc::clone(&recovery), thread_id.clone());
    gate.wait_arrived(1).await;
    let second = spawn_enter(Arc::clone(&recovery), thread_id.clone());
    wait_for("第二个进入持有同一把根锁", || async {
        recovery.in_flight_gate_count(&root) >= 3
    })
    .await;

    gate.release(1);
    let first = first
        .await
        .expect("第一个进入不得 panic")
        .expect("准备本身不是请求错误");
    gate.wait_arrived(2).await;
    assert!(
        recovery.in_flight_gate(&root).is_some(),
        "仍有等待者时删表会让下一次进入新建并行的锁"
    );

    let third = spawn_enter(Arc::clone(&recovery), thread_id.clone());
    wait_for("第三个进入排在同一把门后", || async {
        recovery.in_flight_gate_count(&root) >= 3
    })
    .await;
    gate.release(2);

    let second = second
        .await
        .expect("第二个进入不得 panic")
        .expect("准备本身不是请求错误");
    let third = third
        .await
        .expect("第三个进入不得 panic")
        .expect("准备本身不是请求错误");
    assert_eq!(first.report.recovered, vec![thread_id.clone()]);
    assert!(second.report.recovered.is_empty(), "{second:?}");
    assert!(third.report.recovered.is_empty(), "{third:?}");
    assert_eq!(harness.recovery_rows(&thread_id).await.len(), 1);
    admission_gate::uninstall(&thread_id, PausePoint::InsideGate);
}

/// 10. `discard` 只按本次准入快照删除：会话换代后的新 `Prepared` 必须保留。
#[tokio::test]
async fn discard_does_not_remove_a_newer_generation_admission() {
    let mut harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    harness.set_working(&thread_id).await;
    let recovery = harness.recovery();

    let first = recovery.enter_prepared(&thread_id).await.unwrap();
    assert_eq!(first.report.recovered, vec![thread_id.clone()]);
    // 会话重新 load：代次前进，下一次进入写下新的准入。
    harness
        .sessions
        .lock()
        .await
        .get_mut(&thread_id)
        .unwrap()
        .refresh_session_generation();
    let second = recovery.enter_prepared(&thread_id).await.unwrap();
    assert_eq!(
        second.report.recovered,
        vec![thread_id.clone()],
        "新代次写入自己的准入"
    );

    recovery.discard(&first);
    // 再次进入若仍看到 `Prepared` 准入，说明旧报告的 discard 没有误删新代次。
    let third = recovery.enter(&thread_id).await.unwrap();
    assert_eq!(
        third.skipped,
        vec![peri_acp_types::recovery::RecoverySkip {
            thread_id: thread_id.clone(),
            reason: RecoverySkipReason::AlreadyRunning,
        }],
        "discard 不得删除新代次的准入"
    );

    harness.spawn_continuation_scheduler();
    recovery.launch(&second.report).await;
    wait_for("新代次的准入仍可启动", || async {
        harness._mock.matched()
    })
    .await;
    harness._mock.assert_async().await;
}

/// 11. 同线程超过 8 条 Defer 提醒时，靠后的可复用 entry 仍然可见。
#[tokio::test]
async fn pending_recovery_is_found_behind_more_than_eight_defer_rows() {
    use peri_acp_types::session::{MessageKind, MessageSource, QueuedMessage};

    let harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    harness.set_working(&thread_id).await;
    let queue = harness.queue(&thread_id);
    for index in 0..9 {
        queue
            .enqueue(QueuedMessage::system_reminder(
                MessageKind::Defer,
                MessageSource::SessionRecovery,
                recovery_shaped_reminder_with_kind(&thread_id, &format!("stale_{index}")),
            ))
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(2)).await;
    queue
        .enqueue(QueuedMessage::system_reminder(
            MessageKind::Defer,
            MessageSource::SessionRecovery,
            recovery_shaped_reminder_with_kind(&thread_id, "entry"),
        ))
        .await
        .unwrap();

    let report = harness.recovery().enter(&thread_id).await.unwrap();
    assert_eq!(
        report.recovered,
        vec![thread_id.clone()],
        "必须采纳第 10 条可复用 entry"
    );
    assert_eq!(
        harness.recovery_rows(&thread_id).await.len(),
        10,
        "采纳不得重复注入"
    );
}

/// 构造与真实恢复提醒同形状的可信提醒（来源/kind 由调用方经 QueuedMessage 指定）。
fn recovery_shaped_reminder(
    thread_id: &str,
) -> peri_acp_types::system_reminder::TrustedSystemReminder {
    recovery_shaped_reminder_with_kind(thread_id, "entry")
}

fn recovery_shaped_reminder_with_kind(
    thread_id: &str,
    kind: &str,
) -> peri_acp_types::system_reminder::TrustedSystemReminder {
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };
    let reminder = SystemReminder {
        version: SYSTEM_REMINDER_VERSION,
        category: ReminderCategory::Lifecycle,
        source: ReminderSource("session_recovery".to_owned()),
        kind: kind.to_owned(),
        severity: ReminderSeverity::Warning,
        delivery: ReminderDelivery::Required,
        audiences: ReminderAudiences(vec![ReminderAudience::Tui]),
        body: "diagnostic notice".to_owned(),
        summary: None,
        metadata: serde_json::json!({"thread_id": thread_id}),
    };
    TrustedSystemReminderFactory::for_recovery()
        .construct(reminder)
        .expect("trusted recovery reminder")
}
