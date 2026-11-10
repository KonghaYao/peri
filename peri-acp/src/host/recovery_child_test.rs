//! 冷 child 恢复（设计 §3/§4、§9「主会话 idle、async child working」）：
//! 独立装配、逐 thread 提醒、父级结果通知与幂等/Stop/装配失败分类。
//!
//! 与 `recovery_behavior_test.rs` 同夹具；本文件只覆盖 child 路径。

use super::parent_binding_tests::child_task_events;
use super::*;
use crate::host::recovery::{deliver_child_turn, ChildTurnOutcome};
use peri_acp_types::session::{MessageKind, MessageSource};
use peri_acp_types::thread::{AgentStatus, ThreadId, WorkState};

/// 该 thread 的 MQ 行（不按来源过滤）。
async fn entries(harness: &RecoveryHarness, thread: &str) -> Vec<peri_acp_types::session::MqEntry> {
    harness
        .queue(thread)
        .query(peri_acp_types::session::MqFilter::default())
        .await
        .unwrap()
}

/// 父 thread 收到的结果通知（`SubAgentComplete` 来源）。
pub(super) async fn parent_notices(
    harness: &RecoveryHarness,
    parent: &str,
) -> Vec<peri_acp_types::session::MqEntry> {
    entries(harness, parent)
        .await
        .into_iter()
        .filter(|entry| entry.source == MessageSource::SubAgentComplete)
        .collect()
}

/// 只信 wire 事实：本次 turn 的模型输出以 `child` 的 session id 流出。
fn child_streamed(harness: &RecoveryHarness, child: &str) -> bool {
    harness
        .recorded
        .notifications()
        .iter()
        .any(|(method, params)| {
            method == "session/update"
                && params["sessionId"] == child
                && params["update"]["sessionUpdate"] == "agent_message_chunk"
                && params.to_string().contains("recovered")
        })
}

/// child 的持久事实：终态与续跑意图。
pub(super) async fn settled_meta(
    harness: &RecoveryHarness,
    thread: &str,
) -> peri_acp_types::thread::ThreadMeta {
    harness
        .cfg
        .session_resources
        .load_session_meta(&thread.to_owned())
        .await
        .unwrap()
}

/// 直接写 `agent_status`：模拟「终态已提交、工作意图仍是 working」的残留。
async fn set_agent_status(harness: &RecoveryHarness, thread: &str, status: AgentStatus) {
    harness
        .cfg
        .session_resources
        .update_session_meta(
            &thread.to_owned(),
            &peri_acp_types::session_resources::SessionMetaPatch {
                status: Some(status),
                ..Default::default()
            },
        )
        .await
        .unwrap();
}

async fn set_cwd(db_path: &std::path::Path, thread: &str, cwd: &str) {
    use sqlx::Connection as _;
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(db_path),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE threads SET cwd = ?1 WHERE id = ?2")
        .bind(cwd)
        .bind(thread)
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
}

/// 只含收件地址与种子的结算投递（父级未 live，不含运行登记）。
pub(super) fn delivery(
    parent: &str,
    seed: Option<peri_acp_types::messages::MessageId>,
) -> crate::session::RecoveryDelivery {
    crate::session::RecoveryDelivery {
        parent: ThreadId::from(parent.to_owned()),
        seed,
        task: None,
    }
}

/// 1c. 终态 child 残留 `working`：不冷恢复、不注提醒、不调用模型。
///
/// 终态由领域结果提交；残留的 `working` 只表示旧执行没写收尾，不是待继续。
#[tokio::test]
async fn terminal_child_with_residual_working_is_not_recovered() {
    for status in [
        AgentStatus::Done,
        AgentStatus::Cancelled,
        AgentStatus::Error,
    ] {
        let mut harness = RecoveryHarness::new().await;
        let root = harness.thread_id.clone();
        harness.add_child(&root, "child-b", &root).await;
        harness.set_working("child-b").await;
        set_agent_status(&harness, "child-b", status).await;
        harness.spawn_continuation_scheduler();
        let recovery = harness.recovery();

        let report = recovery.enter(&root).await.expect("恢复准备必须成功");
        assert!(
            report.recovered.is_empty(),
            "{status:?}: 终态 child 不得被恢复：{report:?}"
        );
        assert!(
            report
                .skipped
                .contains(&peri_acp_types::recovery::RecoverySkip {
                    thread_id: "child-b".to_owned(),
                    reason: RecoverySkipReason::Idle,
                }),
            "{status:?}: 终态按空闲跳过：{report:?}"
        );
        assert!(
            harness.recovery_rows("child-b").await.is_empty(),
            "{status:?}: 终态 child 不得收到恢复提醒"
        );

        recovery.launch(&report).await;
        assert!(
            !harness._mock.matched(),
            "{status:?}: 终态 child 不得调用模型"
        );
    }
}

/// 1d. 终态根残留 `working`：同样不续跑、不注入提醒。
#[tokio::test]
async fn terminal_root_with_residual_working_is_not_recovered() {
    let mut harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.set_working(&root).await;
    set_agent_status(&harness, &root, AgentStatus::Error).await;
    harness.spawn_continuation_scheduler();
    let recovery = harness.recovery();

    let report = recovery.enter(&root).await.expect("恢复准备必须成功");
    assert!(report.recovered.is_empty(), "终态根不得被恢复：{report:?}");
    assert!(
        harness.recovery_rows(&root).await.is_empty(),
        "终态根不得收到恢复提醒"
    );
    recovery.launch(&report).await;
    assert!(!harness._mock.matched(), "终态根不得调用模型");
}

/// 1. 根 `idle` + child `working`：只跑 child 一个 turn，父 MQ 收到带
///    `ensure_processing` 的结果通知，父级工作意图被提升。
#[tokio::test]
async fn idle_root_recovers_working_child_and_only_the_child_runs_a_turn() {
    let mut harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.set_working("child-b").await;
    harness.spawn_continuation_scheduler();
    let recovery = harness.recovery();

    let report = recovery.enter(&root).await.expect("恢复准备必须成功");
    assert_eq!(report.recovered, vec!["child-b".to_owned()]);
    assert_eq!(
        report.skipped,
        vec![peri_acp_types::recovery::RecoverySkip {
            thread_id: root.clone(),
            reason: RecoverySkipReason::Idle,
        }],
        "根 idle 只报告跳过，不注入"
    );
    assert!(
        harness.recovery_rows(&root).await.is_empty(),
        "child 的提醒不得写进父队列"
    );
    assert_eq!(harness.recovery_rows("child-b").await.len(), 1);
    assert!(!harness._mock.matched(), "准备阶段不得调用模型");

    recovery.launch(&report).await;
    wait_for("父 MQ 收到子任务结果通知", || async {
        !parent_notices(&harness, &root).await.is_empty()
    })
    .await;
    // 全流程只允许一次模型请求：根 idle，只有 child 跑了一个 turn。
    harness._mock.assert_async().await;
    assert!(
        child_streamed(&harness, "child-b"),
        "结果必须从 child 的会话流出"
    );
    wait_for("child 的恢复提醒被 Receive 移交", || async {
        harness.recovery_rows("child-b").await.is_empty()
    })
    .await;

    let notices = parent_notices(&harness, &root).await;
    assert_eq!(notices.len(), 1, "只允许一条结果通知");
    assert_eq!(notices[0].kind, MessageKind::Defer);
    assert!(
        notices[0].policy.ensures_processing(),
        "恢复结果与普通后台结果一致，允许推动父级处理"
    );
    assert!(
        harness.queue(&root).has_ensure_processing().await.unwrap(),
        "父级 MQ 保留可处理的结果行"
    );
    assert_eq!(
        settled_meta(&harness, &root).await.work_state,
        WorkState::Working,
        "结果接纳把父级 idle 提升为 working"
    );
    // 父级从未在本进程跑过 turn：没有 activation listener 可唤醒，下一次进入
    // 沿 durable working + ensure_processing 行继续处理（明确边界）。
    assert!(
        !harness.listener_started(&root),
        "父级不因 child 建立执行监听"
    );
    let body = reminder_body(&notices[0]);
    assert!(body.contains("child-b"), "通知必须指明来源 thread：{body}");
    // child 的收尾是 durable 的：正常结束后不再有自动续跑意图。
    let meta = settled_meta(&harness, "child-b").await;
    assert_eq!(
        meta.work_state,
        WorkState::Idle,
        "child turn 结束后不得残留 working"
    );
    assert_eq!(
        meta.agent_status,
        AgentStatus::Done,
        "恢复 child 的 end_turn 必须提交 Done 终态"
    );
}

/// 1b. 根与 child 同为 `working`：各自启动自己的 turn，不相互顶替。
#[tokio::test]
async fn working_root_and_child_each_launch_their_own_turn() {
    let mut harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.set_working(&root).await;
    harness.set_working("child-b").await;
    harness.spawn_continuation_scheduler();
    let recovery = harness.recovery();

    let report = recovery.enter(&root).await.unwrap();
    let mut recovered = report.recovered.clone();
    recovered.sort();
    let mut expected = vec![root.clone(), "child-b".to_owned()];
    expected.sort();
    assert_eq!(recovered, expected);

    recovery.launch(&report).await;
    wait_for("根与 child 各跑一个 turn", || async {
        child_streamed(&harness, &root) && child_streamed(&harness, "child-b")
    })
    .await;
    wait_for("两个 thread 的提醒都被移交", || async {
        harness.recovery_rows(&root).await.is_empty()
            && harness.recovery_rows("child-b").await.is_empty()
    })
    .await;
}

/// 2. 嵌套 `A(idle) → B(idle) → C(working)`：只恢复 C，A/B 无模型请求，
///    结果只投给 C 的直接父 B。
#[tokio::test]
async fn nested_idle_chain_recovers_only_the_working_leaf() {
    let mut harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.add_child("child-b", "child-c", &root).await;
    harness.set_working("child-c").await;
    harness.spawn_continuation_scheduler();
    let recovery = harness.recovery();

    let report = recovery.enter(&root).await.unwrap();
    assert_eq!(report.recovered, vec!["child-c".to_owned()]);
    assert_eq!(
        report
            .skipped
            .iter()
            .map(|skip| (skip.thread_id.as_str(), skip.reason))
            .collect::<Vec<_>>(),
        [(root.as_str(), RecoverySkipReason::Idle)],
        "B 不是候选（idle）只体现在范围里，不产生逐项报告"
    );
    assert!(harness.recovery_rows(&root).await.is_empty());
    assert!(harness.recovery_rows("child-b").await.is_empty());
    assert_eq!(harness.recovery_rows("child-c").await.len(), 1);

    recovery.launch(&report).await;
    wait_for("C 的直接父收到结果通知", || async {
        !parent_notices(&harness, "child-b").await.is_empty()
    })
    .await;
    harness._mock.assert_async().await;
    assert!(child_streamed(&harness, "child-c"));
    assert!(
        parent_notices(&harness, &root).await.is_empty(),
        "结果不统一转投 root"
    );
}

/// 3. 重复进入：提醒与 turn 都不成倍。
#[tokio::test]
async fn repeat_enter_keeps_one_reminder_and_one_turn() {
    let mut harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.set_working("child-b").await;
    harness.spawn_continuation_scheduler();
    let recovery = harness.recovery();

    let first = recovery.enter(&root).await.unwrap();
    assert_eq!(first.recovered, vec!["child-b".to_owned()]);
    let second = recovery.enter(&root).await.unwrap();
    assert!(second.recovered.is_empty(), "第二次进入不得再注入");
    assert_eq!(
        second.skipped,
        vec![
            peri_acp_types::recovery::RecoverySkip {
                thread_id: root.clone(),
                reason: RecoverySkipReason::Idle,
            },
            peri_acp_types::recovery::RecoverySkip {
                thread_id: "child-b".to_owned(),
                reason: RecoverySkipReason::AlreadyRunning,
            },
        ]
    );
    assert_eq!(harness.recovery_rows("child-b").await.len(), 1);

    recovery.launch(&first).await;
    wait_for("父 MQ 收到结果通知", || async {
        !parent_notices(&harness, &root).await.is_empty()
    })
    .await;
    harness._mock.assert_async().await;
    assert_eq!(parent_notices(&harness, &root).await.len(), 1);
}

/// 4. child 已有活实例（turn 正在跑）：复用，不重复注入也不启动第二个 turn。
#[tokio::test]
async fn live_child_instance_is_reused() {
    let mut harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.set_working("child-b").await;
    // `session/load` 先装配 child，并把它标记为正在执行。
    harness.load_session("child-b").await;
    {
        let mut states = harness.sessions.lock().await;
        states.get_mut("child-b").unwrap().cancel_token =
            Some(tokio_util::sync::CancellationToken::new());
    }
    let recovery = harness.recovery();

    let report = recovery.enter(&root).await.unwrap();
    assert_eq!(report.reused, vec!["child-b".to_owned()]);
    assert!(report.recovered.is_empty());
    assert!(
        harness.recovery_rows("child-b").await.is_empty(),
        "已有实例不注入"
    );
}

/// 5. child 在准备期间被 Stop：不启动，提醒留在它自己的 MQ。
#[tokio::test]
async fn stop_during_preparation_never_launches_the_child() {
    let mut harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.set_working("child-b").await;
    harness.spawn_continuation_scheduler();
    let recovery = harness.recovery();

    let report = recovery.enter(&root).await.unwrap();
    assert_eq!(report.recovered, vec!["child-b".to_owned()]);
    // 准备完成、启动前：用户 Stop 了该 child（Stop 同时提交停止意图）。
    harness
        .cfg
        .session_manager
        .get_session("child-b")
        .expect("child 运行时必须已装配")
        .activation
        .suppress();
    harness
        .cfg
        .session_resources
        .set_work_state(&"child-b".to_owned(), WorkState::Stopped)
        .await
        .unwrap();

    recovery.launch(&report).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!harness._mock.matched(), "Stop 之后不得启动 turn");
    assert_eq!(harness.recovery_rows("child-b").await.len(), 1, "提醒保留");
    assert!(
        parent_notices(&harness, &root).await.is_empty(),
        "没有 turn 就没有结果通知"
    );
}

/// 6. child 装配失败：请求仍成功，报告里是 Skip，数据与工作意图原样保留。
#[tokio::test]
async fn child_assembly_failure_is_reported_as_skip_without_failing_the_entry() {
    let harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.set_working("child-b").await;
    // 执行目录不可用：`session/load` 同款准入会拒绝该 child（WorkspaceUnavailable）。
    set_cwd(
        &harness.db_path,
        "child-b",
        "/nonexistent/peri/recovery-child",
    )
    .await;
    let recovery = harness.recovery();

    let report = recovery
        .enter(&root)
        .await
        .expect("child 装配失败不得失败请求");
    assert!(report.recovered.is_empty());
    assert!(
        report.failures.is_empty(),
        "装配失败按 Skip 报告：{report:?}"
    );
    assert_eq!(
        report.skipped,
        vec![
            peri_acp_types::recovery::RecoverySkip {
                thread_id: root.clone(),
                reason: RecoverySkipReason::Idle,
            },
            peri_acp_types::recovery::RecoverySkip {
                thread_id: "child-b".to_owned(),
                reason: RecoverySkipReason::AssemblyFailed,
            },
        ]
    );
    assert!(!harness._mock.matched());
    assert!(
        harness.recovery_rows("child-b").await.is_empty(),
        "装配失败不得注入提醒"
    );
    assert_eq!(
        harness
            .cfg
            .session_resources
            .load_session_meta(&"child-b".to_owned())
            .await
            .unwrap()
            .work_state,
        WorkState::Working,
        "失败不得改写工作意图"
    );
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

    deliver_child_turn(
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

    deliver_child_turn(
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
    wait_for("续跑请求进入调度器", || async {
        harness.order.lock().unwrap().contains(&"continuation")
    })
    .await;
    wait_for("父任务登记出现并收尾", || async {
        parent_manager
            .snapshot()
            .tasks
            .iter()
            .any(|task| task.task_id == "child-b")
    })
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
    let (started, completed) = child_task_events(&mut events, "child-b");
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
        deliver_child_turn(
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

    deliver_child_turn(
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

    deliver_child_turn(
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
