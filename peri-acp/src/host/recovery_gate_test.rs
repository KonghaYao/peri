//! 恢复启动门释放与显式 Stop 的启动屏障回归。
//!
//! 与 `recovery_test.rs` 同夹具；这里覆盖需要真实 scheduler 或多阶段屏障的出口。

use super::parent_binding_tests::child_task_events;
use super::recovery_child::{parent_notices, settled_meta};
use super::*;
use crate::host::prompt_dispatch::admission_gate;
use crate::host::{dispatch_prompt_turn, PromptOrigin};
use peri_acp_types::thread::AgentStatus;

/// 2g. 恢复来源的根 turn 在 `working` 已提交、代次尚未推进时收到显式 Stop：
/// 迟到持久化会被代次守卫跳过；本轮若随后启动失败，必须由 dispatch 落 durable
/// `stopped`，而不是按恢复早失败保留 `working`（#348 P2 复审）。
#[tokio::test]
async fn stop_between_working_commit_and_epoch_advance_stops_recovery_root() {
    let harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    harness.set_working(&thread_id).await;
    let report = harness.recovery().enter(&thread_id).await.unwrap();
    assert_eq!(report.recovered, vec![thread_id.clone()]);

    let gate = admission_gate::install(&format!("{thread_id}:working-committed"));
    let dispatch = {
        let sessions = Arc::clone(&harness.sessions);
        let locks = Arc::clone(&harness.locks);
        let transport = Arc::clone(&harness.transport);
        let cfg = Arc::clone(&harness.cfg);
        let sender = Arc::clone(&harness.sender);
        let thread_id = thread_id.clone();
        tokio::spawn(async move {
            // 非法 prompt body：确定性启动失败（校验发生在 attach 之前），
            // 用来验证失败分类而不是执行期取消。
            dispatch_prompt_turn(
                json!({"sessionId": thread_id, "prompt": "invalid"}),
                PromptOrigin::Continuation {
                    mq_steering: true,
                    pending_result: false,
                    recovery: true,
                },
                Some(0),
                &sessions,
                &locks,
                &transport,
                &cfg,
                &sender,
            )
            .await
        })
    };
    tokio::time::timeout(WAIT_TIMEOUT, gate.entered.notified())
        .await
        .expect("恢复续跑必须停在 working 提交之后、代次推进之前");

    let (targets, persist) = harness.capture_and_spawn_stop(&thread_id).await;
    assert_eq!(targets.len(), 1);
    assert_eq!(
        targets[0].epoch, 0,
        "Stop 必须读到代次推进前的值（复现迟到持久化的条件）"
    );

    gate.release.notify_one();
    let result = tokio::time::timeout(WAIT_TIMEOUT, dispatch)
        .await
        .expect("恢复续跑不得挂起")
        .expect("恢复续跑任务不得 panic");
    let error = result.expect_err("非法 prompt body 必须在模型调用前失败");
    assert_eq!(error.code, -32602, "{error:?}");
    tokio::time::timeout(WAIT_TIMEOUT, persist)
        .await
        .expect("Stop 持久化不得挂起")
        .expect("Stop 持久化任务不得 panic");

    assert!(!harness._mock.matched(), "被 Stop 的恢复轮次不得调用模型");
    assert_eq!(
        harness
            .cfg
            .session_resources
            .load_session_meta(&thread_id.to_owned())
            .await
            .unwrap()
            .work_state,
        WorkState::Stopped,
        "恢复来源的显式 Stop 必须优先于恢复早失败的 working 保留"
    );
    assert_eq!(
        harness.recovery_rows(&thread_id).await.len(),
        1,
        "恢复提醒必须保留给下一次显式进入"
    );
}

/// 2h. 恢复续跑的 eligibility 查询真实失败：scheduler 先记录启动失败再释放门；
/// 门释放必须唤醒监听器重查独立工作，`WorkflowComplete` 恰好被消费一次。
#[tokio::test]
async fn eligibility_query_failure_rewakes_queued_workflow_exactly_once() {
    use peri_acp_types::messages::BaseMessage;
    use peri_acp_types::session::{MessageSource, QueuedMessage};

    let mut harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    harness.set_working(&thread_id).await;
    let recovery = harness.recovery();
    let report = recovery.enter(&thread_id).await.unwrap();
    assert_eq!(report.recovered, vec![thread_id.clone()]);

    let listener = admission_gate::install_listener_progress(&thread_id);
    let skip_gate = admission_gate::install(&format!("{thread_id}:recovery-eligibility-skip"));

    // 独立工作先入队：恢复续跑失败期间它留在持久队列。
    harness
        .queue(&thread_id)
        .enqueue(QueuedMessage::defer(
            MessageSource::WorkflowComplete,
            BaseMessage::human("workflow completed"),
        ))
        .await
        .expect("WorkflowComplete 必须入队");

    // 真实查询失败：持久队列表暂时不可读，scheduler 的 eligibility 查询返回 false。
    harness.set_mq_table_hidden(true).await;
    harness.spawn_continuation_scheduler();
    recovery.launch(&report).await;

    // 监听器在启动门关闭期间完成首轮重查并等待；scheduler 停在失败出口。
    tokio::time::timeout(WAIT_TIMEOUT, listener.wait_until(1))
        .await
        .expect("监听器必须完成首轮重查并等待");
    tokio::time::timeout(WAIT_TIMEOUT, skip_gate.entered.notified())
        .await
        .expect("scheduler 必须停在 eligibility 失败出口");
    assert!(
        harness
            .cfg
            .session_manager
            .get_session(&thread_id)
            .expect("session runtime is registered")
            .recovery_launch_failed(),
        "真实查询失败必须记录恢复启动失败"
    );

    // 恢复真实队列后释放启动门：监听器重查只能调度独立工作。
    harness.set_mq_table_hidden(false).await;
    skip_gate.release.notify_one();
    tokio::time::timeout(WAIT_TIMEOUT, listener.wait_until(2))
        .await
        .expect("门释放必须唤醒监听器重查");

    wait_for("WorkflowComplete 被普通续跑消费", || async {
        harness._mock.matched()
    })
    .await;
    wait_for("WorkflowComplete 从 MQ 恰好消费一次", || async {
        harness
            .queue(&thread_id)
            .query(MqFilter {
                sources: vec![MessageSource::WorkflowComplete],
                ..Default::default()
            })
            .await
            .unwrap()
            .is_empty()
    })
    .await;
    wait_for("WorkflowComplete 只写入一次历史", || async {
        workflow_history_count(&harness, &thread_id).await == 1
    })
    .await;

    assert_eq!(
        continuation_requests(&harness),
        2,
        "只应有恢复启动请求与一次独立续跑"
    );
    harness._mock.assert_async().await;
    assert_eq!(
        workflow_history_count(&harness, &thread_id).await,
        1,
        "workflow payload 恰好消费一次"
    );
}

/// 2i. 绑定校验真实失败：只剩恢复提醒时不得在清门后自动重试；下一次显式进入
/// 消费失败事实并重新采纳同一条提醒。
#[tokio::test]
async fn binding_failure_keeps_failed_recovery_reminder_for_explicit_retry() {
    let mut harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    harness.set_working(&thread_id).await;
    let recovery = harness.recovery();
    let report = recovery.enter(&thread_id).await.unwrap();
    assert_eq!(report.recovered, vec![thread_id.clone()]);

    let original = harness.binding_workspace_id(&thread_id).await;
    harness
        .set_binding_workspace(&thread_id, "missing-workspace")
        .await;

    // 监听器固定在首轮 poll 之前：先让 dispatch 真实撞上绑定校验失败。
    let first_poll = admission_gate::install(&format!("{thread_id}:listener-first-poll"));
    let listener = admission_gate::install_listener_progress(&thread_id);
    harness.spawn_continuation_scheduler();
    recovery.launch(&report).await;

    wait_for("绑定校验失败并释放启动门", || async {
        harness
            .cfg
            .session_manager
            .get_session(&thread_id)
            .is_some_and(|session| {
                session.recovery_launch_failed() && !session.activation.recovery_launch_pending()
            })
    })
    .await;
    first_poll.release.notify_one();
    tokio::time::timeout(WAIT_TIMEOUT, listener.wait_until(1))
        .await
        .expect("监听器必须在失败后完成一次完整重查");

    assert_eq!(
        continuation_requests(&harness),
        1,
        "失败的恢复提醒不得在清门后被自动重试"
    );
    assert!(!harness._mock.matched(), "自动重试不得调用模型");
    assert_eq!(
        harness.recovery_rows(&thread_id).await.len(),
        1,
        "提醒保留给显式进入"
    );

    // 真实绑定恢复后显式重新进入：消费失败标记并重试同一条提醒。
    harness.set_binding_workspace(&thread_id, &original).await;
    let retry = recovery.enter(&thread_id).await.unwrap();
    assert_eq!(
        retry.recovered,
        vec![thread_id.clone()],
        "显式进入必须重新采纳同一提醒"
    );
    recovery.launch(&retry).await;
    wait_for("显式重试执行恢复 turn", || async {
        harness._mock.matched()
    })
    .await;
    harness._mock.assert_async().await;
    assert!(
        harness.recovery_rows(&thread_id).await.is_empty(),
        "重试后提醒必须被消费"
    );
}

/// 2j. 已开始执行的恢复 child 被 Stop：执行器已启动并返回 cancelled，终态、
/// 工作意图与父级结果必须一次原子结算，不能按「未移交」跳过父级结果。
#[tokio::test]
async fn stop_after_recovered_child_started_settles_cancelled_and_parent_result() {
    let mut harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.set_working("child-b").await;
    let parent_manager = harness
        .cfg
        .session_manager
        .get_session(&root)
        .expect("root session is published")
        .task_manager
        .clone();
    let mut events = parent_manager.subscribe_events();
    let model_gate = harness.install_gated_model().await;
    harness.spawn_continuation_scheduler();
    let recovery = harness.recovery();
    let report = recovery.enter(&root).await.unwrap();
    assert_eq!(report.recovered, vec!["child-b".to_owned()]);
    recovery.launch(&report).await;

    // child 已进入执行器的模型调用：此后的 Stop 属于「执行后取消」。
    tokio::time::timeout(WAIT_TIMEOUT, model_gate.entered())
        .await
        .expect("恢复 child 必须开始执行");
    let (targets, persist) = harness.capture_and_spawn_stop("child-b").await;
    assert_eq!(targets.len(), 1, "Stop 必须命中正在执行的 child");
    model_gate.release();
    tokio::time::timeout(WAIT_TIMEOUT, persist)
        .await
        .expect("Stop 持久化不得挂起")
        .expect("Stop 持久化任务不得 panic");

    wait_for("父级收到唯一的结果通知", || async {
        parent_notices(&harness, &root).await.len() == 1
    })
    .await;
    wait_for("父任务登记收尾", || async {
        parent_manager
            .snapshot()
            .tasks
            .iter()
            .any(|task| task.task_id == "child-b" && task.status != "running")
    })
    .await;
    // 执行器已启动后的 Stop 属于执行失败：本轮退出在飞状态时不得留下启动早失败
    // 标记，否则下一次显式进入会把已结算的 Launched 占位当成启动失败回收。
    wait_for("恢复续跑退出在飞状态", || async {
        harness
            .sessions
            .lock()
            .await
            .get("child-b")
            .is_some_and(|state| !state.continuation_in_flight)
    })
    .await;
    assert!(
        !harness
            .cfg
            .session_manager
            .get_session("child-b")
            .expect("child runtime is registered")
            .recovery_launch_failed(),
        "执行后 Stop 不是启动早失败，不得写恢复启动标记"
    );

    let meta = settled_meta(&harness, "child-b").await;
    assert_eq!(
        meta.agent_status,
        AgentStatus::Cancelled,
        "执行后 Stop 必须结算为 Cancelled"
    );
    assert_eq!(meta.work_state, WorkState::Stopped);
    assert_eq!(
        parent_notices(&harness, &root).await.len(),
        1,
        "父级 durable SubAgentComplete 恰好一条"
    );
    let (started, _) = child_task_events(&mut events, "child-b");
    assert!(started, "恢复 child 必须以父任务身份开始过");
    let tasks = parent_manager.snapshot().tasks;
    let task = tasks
        .iter()
        .find(|task| task.task_id == "child-b")
        .expect("父任务投影必须包含恢复 child");
    assert_eq!(task.status, "cancelled", "Stop 后父任务以取消收尾");
    assert_eq!(
        parent_manager.active_count(),
        0,
        "结算后不得留下 pending/active 父任务"
    );
}

/// 2k. 恢复 turn 已移交执行器并成功：执行事实不是启动早失败，不得写
/// `recovery_launch_failed`；否则下一次显式进入会把已执行的 Launched 占位当成
/// 启动失败回收，重新采纳已消费的提醒。
#[tokio::test]
async fn executed_recovery_turn_success_does_not_mark_launch_failed() {
    let harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    harness.set_working(&thread_id).await;
    let report = harness.recovery().enter(&thread_id).await.unwrap();
    assert_eq!(report.recovered, vec![thread_id.clone()]);

    let response = dispatch_prompt_turn(
        json!({
            "sessionId": thread_id,
            "message": {"role": "user", "content": []},
        }),
        PromptOrigin::Continuation {
            mq_steering: true,
            pending_result: false,
            recovery: true,
        },
        Some(0),
        &harness.sessions,
        &harness.locks,
        &harness.transport,
        &harness.cfg,
        &harness.sender,
    )
    .await
    .expect("恢复 turn 必须执行完成");
    assert_eq!(response["stopReason"], "end_turn");
    harness._mock.assert_async().await;
    assert!(
        !harness
            .cfg
            .session_manager
            .get_session(&thread_id)
            .expect("session runtime is registered")
            .recovery_launch_failed(),
        "执行器已启动的恢复轮次不得写成启动早失败"
    );
}

/// 监听器收到的续跑请求数（每次 scheduler 下单都计一次）。
fn continuation_requests(harness: &RecoveryHarness) -> usize {
    harness
        .order
        .lock()
        .unwrap()
        .iter()
        .filter(|entry| **entry == "continuation")
        .count()
}

/// 该 thread 历史中 `WorkflowComplete` 载荷的出现次数。
async fn workflow_history_count(harness: &RecoveryHarness, thread_id: &str) -> usize {
    harness
        .cfg
        .session_resources
        .load_session_history(&peri_acp_types::thread::ThreadId::from(
            thread_id.to_owned(),
        ))
        .await
        .unwrap()
        .iter()
        .filter(|payload| match payload {
            peri_acp_types::store::PersistedPayload::Message(message) => {
                message.content().contains("workflow completed")
            }
            peri_acp_types::store::PersistedPayload::SystemReminder { .. } => false,
        })
        .count()
}
