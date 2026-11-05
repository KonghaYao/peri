//! 恢复 child 的直接父级运行登记（W3a）：任务投影、cascade 取消、结算与 adoption。
//!
//! 与 `recovery_child_test.rs` 同夹具；本文件只覆盖父级登记与收件人/策略。

use super::recovery_child::{parent_notices, settled_meta};
use super::*;
use crate::host::recovery::{
    deliver_child_turn, finish_recovered_child_without_result, register_recovered_child_for_turn,
    ChildTurnOutcome,
};
use peri_acp_types::tasks::BgRegistryEvent;
use peri_acp_types::thread::{CancelPolicy, ThreadId, WorkState};

/// 父发布广播接收者；`child_task_events` 只读取 child 自己的 Started/Completed。
/// 父会话的 `session/bg-tasks` 投影。
fn parent_bg_tasks(harness: &RecoveryHarness, parent: &str) -> Vec<serde_json::Value> {
    let value = crate::host::requests::session_lifecycle::handle_bg_tasks(
        &serde_json::json!({ "sessionId": parent }),
        &harness.cfg,
    )
    .expect("parent bg-tasks must be queryable");
    value["tasks"].as_array().cloned().unwrap_or_default()
}

/// 排空 task manager 事件流，返回 child 的 Started/Completed 事实。
pub(super) fn child_task_events(
    receiver: &mut tokio::sync::broadcast::Receiver<peri_acp_types::tasks::TaskChange>,
    child: &str,
) -> (bool, Option<bool>) {
    let mut started = false;
    let mut completed = None;
    while let Ok(change) = receiver.try_recv() {
        match change.event {
            BgRegistryEvent::Started { task_id, .. } if task_id == child => started = true,
            BgRegistryEvent::Completed {
                task_id, success, ..
            } if task_id == child => completed = Some(success),
            _ => {}
        }
    }
    (started, completed)
}

/// 恢复 child 出现在父 `session/bg-tasks`，结算后父任务收尾且不残留 pending。
#[tokio::test]
async fn recovered_child_registers_in_live_parent_and_finishes_after_settlement() {
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
    recovery.launch(&report).await;
    wait_for("父 MQ 收到恢复结果", || async {
        !parent_notices(&harness, &root).await.is_empty()
    })
    .await;
    harness._mock.assert_async().await;

    // 任务身份在 turn 真正开始前登记：Started 事件证明它进入过父任务投影。
    let (started, completed) = child_task_events(&mut events, "child-b");
    assert!(started, "恢复 child 必须以父任务身份开始过");
    assert_eq!(completed, Some(true), "结算后父任务必须收尾为完成");
    assert_eq!(
        parent_manager.active_count(),
        0,
        "结算后不得留下 pending/active 父任务"
    );
    let tasks = parent_bg_tasks(&harness, &root);
    let task = tasks
        .iter()
        .find(|task| task["task_id"] == "child-b")
        .expect("父 session/bg-tasks 必须包含恢复 child");
    assert_ne!(task["status"], "running", "投影不得停留在 running");
}

/// 同一恢复投递重复登记是 no-op：父任务与 Started 事件都只有一个。
#[tokio::test]
async fn repeated_recovery_registration_is_deduplicated() {
    let mut harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.set_working("child-b").await;
    harness.load_session("child-b").await;
    {
        let mut states = harness.sessions.lock().await;
        states
            .get_mut("child-b")
            .expect("child session is published")
            .cancel_token = Some(tokio_util::sync::CancellationToken::new());
    }
    harness
        .cfg
        .session_manager
        .get_session("child-b")
        .expect("child session is published")
        .record_recovery_parent(ThreadId::from(root.clone()), None);
    let parent_manager = harness
        .cfg
        .session_manager
        .get_session(&root)
        .expect("root session is published")
        .task_manager
        .clone();
    let mut events = parent_manager.subscribe_events();

    assert!(
        register_recovered_child_for_turn(&harness.cfg, &harness.sessions, "child-b")
            .await
            .unwrap(),
        "首次准入必须登记"
    );
    assert!(
        !register_recovered_child_for_turn(&harness.cfg, &harness.sessions, "child-b")
            .await
            .unwrap(),
        "同一投递不得重复登记"
    );
    let (started, completed) = child_task_events(&mut events, "child-b");
    assert!(started);
    assert_eq!(completed, None);
    assert_eq!(parent_manager.active_count(), 1);
    assert_eq!(
        parent_manager
            .snapshot()
            .tasks
            .iter()
            .filter(|task| task.task_id == "child-b")
            .count(),
        1,
        "重复进入不得产生第二个父任务投影"
    );
    {
        let parent = harness
            .cfg
            .session_manager
            .get_session(&root)
            .expect("root session is published");
        let runtime = parent
            .active_agents
            .get(&ThreadId::from("child-b"))
            .expect("恢复 child 必须登记为父级 cascade 运行时");
        assert_eq!(runtime.cancel_policy, CancelPolicy::Cascade);
    }

    let delivery = harness
        .cfg
        .session_manager
        .get_session("child-b")
        .expect("child session is published")
        .take_recovery_parent()
        .expect("recovery delivery must be retained");
    finish_recovered_child_without_result(&harness.cfg, &delivery);
    assert_eq!(parent_manager.active_count(), 0);
    assert!(
        !harness
            .cfg
            .session_manager
            .get_session(&root)
            .expect("root session is published")
            .active_agents
            .contains_key(&ThreadId::from("child-b")),
        "结算必须移除父级运行登记"
    );
}

/// 直接父未 live：恢复照常运行，但不会在非直接父级产生幽灵任务。
#[tokio::test]
async fn parent_not_live_skips_registration_without_ghost_task() {
    let mut harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.add_child("child-b", "child-c", &root).await;
    harness.set_working("child-c").await;
    harness.spawn_continuation_scheduler();
    let recovery = harness.recovery();

    let report = recovery.enter(&root).await.unwrap();
    assert_eq!(report.recovered, vec!["child-c".to_owned()]);
    recovery.launch(&report).await;
    wait_for("直接父 B 收到结果", || async {
        !parent_notices(&harness, "child-b").await.is_empty()
    })
    .await;
    harness._mock.assert_async().await;

    let root_manager = harness
        .cfg
        .session_manager
        .get_session(&root)
        .expect("root session is published")
        .task_manager
        .clone();
    assert_eq!(root_manager.active_count(), 0);
    assert!(
        root_manager
            .snapshot()
            .tasks
            .iter()
            .all(|task| task.task_id != "child-c"),
        "父 B 未 live 时不得把任务登记到祖先 A"
    );
}

/// 父级之后进入：已在运行的恢复 child 被 adoption 到直接父级，不启动新执行。
#[tokio::test]
async fn parent_entry_adopts_an_already_running_recovered_child() {
    let mut harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.add_child("child-b", "child-c", &root).await;
    harness.set_working("child-c").await;
    harness.load_session("child-b").await;
    harness.load_session("child-c").await;
    {
        let mut states = harness.sessions.lock().await;
        states
            .get_mut("child-c")
            .expect("child session is published")
            .cancel_token = Some(tokio_util::sync::CancellationToken::new());
    }
    assert!(
        harness
            .cfg
            .session_manager
            .get_session("child-c")
            .expect("child session is published")
            .record_recovery_parent(ThreadId::from("child-b"), None),
        "模拟 C 已在运行且等待结算"
    );
    let recovery = harness.recovery();

    let report = recovery.enter(&root).await.unwrap();
    assert_eq!(report.reused, vec!["child-c".to_owned()]);
    let parent_manager = harness
        .cfg
        .session_manager
        .get_session("child-b")
        .expect("parent B is live")
        .task_manager
        .clone();
    let task = parent_manager
        .snapshot()
        .tasks
        .iter()
        .find(|task| task.task_id == "child-c")
        .cloned()
        .expect("adoption 必须补登记直接父任务");
    assert_eq!(task.status, "running");
    assert!(
        harness
            .cfg
            .session_manager
            .get_session("child-b")
            .expect("parent B is live")
            .active_agents
            .contains_key(&ThreadId::from("child-c")),
        "adoption 必须补齐 cascade 取消关系"
    );

    let delivery = harness
        .cfg
        .session_manager
        .get_session("child-c")
        .expect("child session is published")
        .take_recovery_parent()
        .expect("adopted delivery must be retained");
    deliver_child_turn(
        &harness.cfg,
        &harness.sessions,
        "child-c",
        &delivery,
        ChildTurnOutcome::Finished {
            stop_reason: "end_turn",
        },
    )
    .await;
    assert_eq!(parent_manager.active_count(), 0);
    assert_eq!(
        parent_manager
            .snapshot()
            .tasks
            .iter()
            .find(|task| task.task_id == "child-c")
            .map(|task| task.status.as_str()),
        Some("completed")
    );
    assert!(!harness
        .cfg
        .session_manager
        .get_session("child-b")
        .expect("parent B is live")
        .active_agents
        .contains_key(&ThreadId::from("child-c")));
}

/// 父 Stop：cascade 恢复 child 被取消并落 `stopped`；父任务仍可收尾。
#[tokio::test]
async fn parent_stop_cancels_cascade_recovered_child_and_persists_stopped() {
    let mut harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.set_working("child-b").await;
    harness.load_session("child-b").await;
    let token = tokio_util::sync::CancellationToken::new();
    {
        let mut states = harness.sessions.lock().await;
        states
            .get_mut("child-b")
            .expect("child session is published")
            .cancel_token = Some(token.clone());
    }
    harness
        .cfg
        .session_manager
        .get_session("child-b")
        .expect("child session is published")
        .record_recovery_parent(ThreadId::from(root.clone()), None);
    assert!(
        register_recovered_child_for_turn(&harness.cfg, &harness.sessions, "child-b")
            .await
            .unwrap()
    );

    let outcome = {
        let mut states = harness.sessions.lock().await;
        crate::host::notify::handle_notification(
            "session/cancel",
            &serde_json::json!({ "sessionId": root }),
            &mut states,
            &harness.cfg,
        )
        .await
    };
    assert!(outcome.stop_targets.contains(&ThreadId::from("child-b")));
    assert!(token.is_cancelled(), "父 Stop 必须取消 cascade 恢复 child");
    crate::host::notify::persist_notification_effects(
        "session/cancel",
        &harness.cfg,
        outcome.stop_targets,
        outcome.context_usage_invalidations,
    )
    .await;
    assert_eq!(
        settled_meta(&harness, "child-b").await.work_state,
        WorkState::Stopped,
        "Stop 必须把 cascade 恢复 child 落为 stopped"
    );

    let delivery = harness
        .cfg
        .session_manager
        .get_session("child-b")
        .expect("child session is published")
        .take_recovery_parent()
        .expect("recovery delivery must be retained");
    finish_recovered_child_without_result(&harness.cfg, &delivery);
}

/// 父 close：既有 cancel_all 取消恢复 child；结算后 close 能完成。
#[tokio::test]
async fn parent_close_cancels_recovered_child_registration() {
    let mut harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.set_working("child-b").await;
    harness.load_session("child-b").await;
    let token = tokio_util::sync::CancellationToken::new();
    {
        let mut states = harness.sessions.lock().await;
        states
            .get_mut("child-b")
            .expect("child session is published")
            .cancel_token = Some(token.clone());
    }
    harness
        .cfg
        .session_manager
        .get_session("child-b")
        .expect("child session is published")
        .record_recovery_parent(ThreadId::from(root.clone()), None);
    assert!(
        register_recovered_child_for_turn(&harness.cfg, &harness.sessions, "child-b")
            .await
            .unwrap()
    );
    let parent_manager = harness
        .cfg
        .session_manager
        .get_session(&root)
        .expect("root session is published")
        .task_manager
        .clone();

    harness.cfg.session_manager.pre_close_session(&root);
    assert!(token.is_cancelled(), "父 close 必须取消恢复 child");
    assert_eq!(
        parent_manager.active_count(),
        0,
        "cancel_all 必须结束父任务投影"
    );

    let delivery = harness
        .cfg
        .session_manager
        .get_session("child-b")
        .expect("child session is published")
        .take_recovery_parent()
        .expect("recovery delivery must be retained");
    finish_recovered_child_without_result(&harness.cfg, &delivery);
    assert!(
        harness
            .cfg
            .session_manager
            .close_session(&root)
            .await
            .is_ok(),
        "登记收尾后 close 必须能完成"
    );
}

/// live 且已激活的 idle 父级在结果接纳后被唤醒并产出续跑请求。
#[tokio::test]
async fn live_idle_parent_is_woken_by_the_recovered_result() {
    let mut harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    harness.add_child(&root, "child-b", &root).await;
    harness.set_working("child-b").await;
    let receiver = harness.receiver.take().expect("receiver is unused");
    {
        let runtime = harness
            .cfg
            .session_manager
            .get_session(&root)
            .expect("root session is published");
        runtime.activation.allow();
    }
    crate::host::activation::ensure_listener(
        &root,
        &harness.sessions,
        &harness.cfg,
        &harness.sender,
    )
    .expect("parent listener must start");

    let recovery = harness.recovery();
    recovery.enter(&root).await.unwrap();
    let delivery = harness
        .cfg
        .session_manager
        .get_session("child-b")
        .expect("child session is published")
        .take_recovery_parent()
        .expect("recovery delivery must be retained");
    deliver_child_turn(
        &harness.cfg,
        &harness.sessions,
        "child-b",
        &delivery,
        ChildTurnOutcome::Finished {
            stop_reason: "end_turn",
        },
    )
    .await;

    let request = tokio::time::timeout(Duration::from_secs(5), async {
        let mut receiver = receiver;
        receiver.recv().await
    })
    .await
    .expect("live idle parent must be woken")
    .expect("continuation channel must stay open");
    assert_eq!(request.session_id, root);
    assert!(
        harness.queue(&root).has_ensure_processing().await.unwrap(),
        "唤醒来自父 MQ 的 ensure_processing 结果行"
    );
    assert_eq!(
        settled_meta(&harness, &root).await.work_state,
        WorkState::Working
    );
}

/// 结果固定投给持久父链上的直接父；显式 resume 的调用者不会改写收件人。
#[tokio::test]
async fn recovery_result_targets_persistent_direct_parent_not_the_resumer() {
    let harness = RecoveryHarness::new().await;
    let root = harness.thread_id.clone();
    // B 只是同树的兄弟；C 的持久父是 A。B 显式 resume C 不改写 parent_thread_id，
    // 因此崩溃后的恢复结果仍回到 A（直接父权威，不伪造 caller 目标）。
    harness.add_child(&root, "child-b", &root).await;
    harness.add_child(&root, "child-c", &root).await;
    harness.set_working("child-c").await;
    let recovery = harness.recovery();

    recovery.enter(&root).await.unwrap();
    let delivery = harness
        .cfg
        .session_manager
        .get_session("child-c")
        .expect("child session is published")
        .take_recovery_parent()
        .expect("recovery delivery must be retained");
    assert_eq!(delivery.parent, ThreadId::from(root.clone()));
    deliver_child_turn(
        &harness.cfg,
        &harness.sessions,
        "child-c",
        &delivery,
        ChildTurnOutcome::Finished {
            stop_reason: "end_turn",
        },
    )
    .await;
    assert_eq!(parent_notices(&harness, &root).await.len(), 1);
    assert!(
        parent_notices(&harness, "child-b").await.is_empty(),
        "不得把结果伪造给 resumer"
    );
}
