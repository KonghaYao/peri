//! 恢复启动门释放与显式 Stop 的启动屏障回归。
//!
//! 与 `recovery_test.rs` 同夹具；这里覆盖需要真实 scheduler 或多阶段屏障的出口。

use super::*;
use crate::host::prompt_dispatch::admission_gate;
use crate::host::{dispatch_prompt_turn, PromptOrigin};

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

/// 2h. 恢复启动门释放后必须唤醒监听器重查独立工作：scheduler 判定本次恢复续跑
/// 被新 prompt 取代后只清门；已入队的 `WorkflowComplete` 不能等待无关事件。
#[tokio::test]
async fn gate_release_rewakes_queued_workflow_after_skipped_recovery() {
    use peri_acp_types::messages::BaseMessage;
    use peri_acp_types::session::{MessageSource, QueuedMessage};

    let mut harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    harness.set_working(&thread_id).await;
    let recovery = harness.recovery();
    let report = recovery.enter(&thread_id).await.unwrap();
    assert_eq!(report.recovered, vec![thread_id.clone()]);

    // 监听器首轮固定在「恢复续跑已排入、门仍关闭」的窗口。
    let listener_gate = admission_gate::install(&format!("{thread_id}:listener-first-poll"));
    // 恢复续跑在拿到 prompt lock 后、校验 epoch 前暂停。
    let dispatch_gate = admission_gate::install(&thread_id);
    recovery.launch(&report).await;

    // 普通工作先入队：监听器醒来时门未释放，不应为它下单。
    harness
        .queue(&thread_id)
        .enqueue(QueuedMessage::defer(
            MessageSource::WorkflowComplete,
            BaseMessage::human("workflow completed"),
        ))
        .await
        .expect("WorkflowComplete 必须入队");
    harness.spawn_continuation_scheduler();

    tokio::time::timeout(WAIT_TIMEOUT, dispatch_gate.entered.notified())
        .await
        .expect("恢复续跑必须到达 epoch 校验前的屏障");
    // 新 prompt 先推进代次：恢复续跑随后被判为过期，scheduler 只清门后 continue。
    {
        let mut sessions = harness.sessions.lock().await;
        sessions
            .get_mut(&thread_id)
            .expect("会话存在")
            .continuation_epoch += 1;
    }
    // 放行监听器：它消费本轮 watch 版本，因门关闭而返回并等待。
    listener_gate.release.notify_one();
    tokio::time::sleep(Duration::from_millis(100)).await;

    dispatch_gate.release.notify_one();
    wait_for("清门后监听器重新检查到 WorkflowComplete", || async {
        harness
            .order
            .lock()
            .unwrap()
            .iter()
            .filter(|entry| **entry == "continuation")
            .count()
            >= 2
    })
    .await;
    wait_for("WorkflowComplete 被普通续跑消费", || async {
        harness._mock.matched()
    })
    .await;
}

/// 2i. 门释放只重查独立来源：启动失败的恢复提醒仍只由下一次显式进入重试。
#[tokio::test]
async fn gate_release_does_not_retry_a_failed_recovery_reminder() {
    let mut harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    harness.set_working(&thread_id).await;
    let recovery = harness.recovery();
    let report = recovery.enter(&thread_id).await.unwrap();
    assert_eq!(report.recovered, vec![thread_id.clone()]);

    let listener_gate = admission_gate::install(&format!("{thread_id}:listener-first-poll"));
    let dispatch_gate = admission_gate::install(&thread_id);
    recovery.launch(&report).await;
    harness.spawn_continuation_scheduler();

    tokio::time::timeout(WAIT_TIMEOUT, dispatch_gate.entered.notified())
        .await
        .expect("恢复续跑必须到达 epoch 校验前的屏障");
    {
        let mut sessions = harness.sessions.lock().await;
        sessions
            .get_mut(&thread_id)
            .expect("会话存在")
            .continuation_epoch += 1;
    }
    listener_gate.release.notify_one();
    tokio::time::sleep(Duration::from_millis(100)).await;
    dispatch_gate.release.notify_one();

    // 只清门、不自动重试：队列里只有失败的恢复提醒。
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        harness
            .order
            .lock()
            .unwrap()
            .iter()
            .filter(|entry| **entry == "continuation")
            .count(),
        1,
        "失败的恢复提醒不得在清门后被普通激活自动重试"
    );
    assert!(!harness._mock.matched(), "自动重试不得调用模型");
    assert!(
        harness
            .cfg
            .session_manager
            .get_session(&thread_id)
            .expect("session runtime is registered")
            .recovery_launch_failed(),
        "失败事实保留给下一次显式进入"
    );

    let retry = recovery.enter(&thread_id).await.unwrap();
    assert_eq!(
        retry.recovered,
        vec![thread_id.clone()],
        "显式进入必须采纳同一条提醒重试"
    );
    assert_eq!(
        harness.recovery_rows(&thread_id).await.len(),
        1,
        "显式重试不得重复入队"
    );
}
