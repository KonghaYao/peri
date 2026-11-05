//! 后台 child 终态结算：终态、工作意图与结果接纳在同一次存储提交里。

use super::*;
use crate::agent::async_tasks::delivery::terminal_delivery_id;
use crate::agent::events::BackgroundTaskResult;
use crate::session::test_resources::TestSession;
use peri_acp_types::thread::{AgentStatus, WorkState};

fn result(task_id: &str) -> BackgroundTaskResult {
    BackgroundTaskResult {
        task_id: task_id.into(),
        agent_name: "fixture".into(),
        prompt_summary: "task".into(),
        success: true,
        output: "done".into(),
        tool_calls_count: 0,
        duration_ms: 0,
        child_thread_id: Some("child".into()),
        timed_out: false,
        subagent_failure: None,
        shell_output: None,
    }
}

fn route(session: &TestSession, target: peri_acp_types::thread::ThreadId) -> TerminalMqRoute {
    TerminalMqRoute {
        store: session.resources(),
        target,
        mailbox: None,
    }
}

/// 终态写入只有一个归属：handoff 接管后执行体不得再写，反之亦然。
#[test]
fn terminal_ownership_has_exactly_one_writer() {
    let handed_off = TerminalOwnership::new();
    assert!(handed_off.claim_handoff());
    assert!(
        !handed_off.claim_settlement(),
        "handoff worker 已接管时执行体必须让出终态"
    );

    let execution = TerminalOwnership::new();
    assert!(execution.claim_settlement());
    assert!(!execution.claim_handoff(), "执行体已收尾时不得再 detach");
}

/// 交接后 wrapper 退出/准入被拒：执行体与 wrapper 通过同一 CAS 竞争唯一写入；
/// worker 已准入时任何一方都不得抢写。
#[test]
fn abandoned_handoff_has_exactly_one_terminal_writer() {
    let wrapper_exit = TerminalOwnership::new();
    assert!(wrapper_exit.claim_handoff());
    wrapper_exit.mark_worker_admitted();
    wrapper_exit.wrapper_finished();
    assert!(
        !wrapper_exit.reclaim_abandoned_handoff(),
        "已准入的 worker 持有终态"
    );
    assert!(!wrapper_exit.claim_settlement());

    let rejected = TerminalOwnership::new();
    assert!(rejected.claim_handoff());
    rejected.abandon_handoff();
    assert!(
        rejected.reclaim_abandoned_handoff(),
        "准入被拒后由执行体接回终态"
    );
    assert!(!rejected.reclaim_abandoned_handoff(), "只能有一个写入者");

    let dropped = TerminalOwnership::new();
    assert!(dropped.claim_handoff());
    dropped.wrapper_finished();
    assert!(
        dropped.reclaim_abandoned_handoff(),
        "wrapper 在准入前退出时由执行体写终态"
    );
}

/// 一次提交同时成立：终态、续跑意图与目标 MQ 行。
#[tokio::test]
async fn settle_commits_terminal_work_state_and_result_together() {
    let session = TestSession::open().await;
    let child = session.thread_id();
    session
        .resources()
        .set_work_state(&child, WorkState::Working)
        .await
        .unwrap();
    let route = route(&session, child.clone());

    route
        .settle(&child, AgentStatus::Done, &result("task-1"))
        .await
        .unwrap();

    let meta = session.resources().load_session_meta(&child).await.unwrap();
    assert_eq!(meta.agent_status, AgentStatus::Done);
    assert_eq!(meta.work_state, WorkState::Idle);
    let entries = session.queue().snapshot().await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].delivery_id,
        Some(terminal_delivery_id("task-1", "terminal"))
    );

    // 重放同一结果身份：存储已有同一条，不新增第二行、不改终态。
    route
        .settle(&child, AgentStatus::Done, &result("task-1"))
        .await
        .unwrap();
    assert_eq!(session.queue().snapshot().await.unwrap().len(), 1);
}

/// [回归] 正常终态已写库后执行才 panic：panic 兜底以同一 task_id 再结算一次
/// （Error + panic 正文）必须是成功 no-op，不得把冲突一路传播成永久重试。
#[tokio::test]
async fn settle_is_final_after_the_first_terminal() {
    let session = TestSession::open().await;
    let child = session.thread_id();
    let route = route(&session, child.clone());

    route
        .settle(&child, AgentStatus::Done, &result("task-panic"))
        .await
        .unwrap();

    let mut panicked = result("task-panic");
    panicked.success = false;
    panicked.output = "Background sub-agent execution panicked: boom".into();
    route
        .settle(&child, AgentStatus::Error, &panicked)
        .await
        .expect("panic 兜底的重复结算不得传播冲突");

    let meta = session.resources().load_session_meta(&child).await.unwrap();
    assert_eq!(
        meta.agent_status,
        AgentStatus::Done,
        "已写下的终态不得被 panic 改写"
    );
    assert_eq!(
        session.queue().snapshot().await.unwrap().len(),
        1,
        "不得新增结果行"
    );
}

/// [回归] 结果无处投递时整次结算失败：终态与续跑意图都不得先写。
///
/// 旧实现先写状态、再由下游投递，父 thread 不存在时会留下「child 已结束、结果没投递」。
#[tokio::test]
async fn settle_leaves_terminal_untouched_when_the_target_is_missing() {
    let session = TestSession::open().await;
    let child = session.thread_id();
    session
        .resources()
        .set_work_state(&child, WorkState::Working)
        .await
        .unwrap();
    let missing: peri_acp_types::thread::ThreadId =
        "00000000-0000-7000-8000-000000000000".to_owned();
    let route = route(&session, missing);

    let error = route
        .settle(&child, AgentStatus::Done, &result("task-2"))
        .await
        .expect_err("目标 thread 不存在必须如实失败");
    assert!(!error.is_empty());

    let meta = session.resources().load_session_meta(&child).await.unwrap();
    assert_eq!(
        meta.agent_status,
        AgentStatus::Active,
        "终态不得越过失败的提交"
    );
    assert_eq!(
        meta.work_state,
        WorkState::Working,
        "续跑意图保持最后提交的值"
    );
    assert!(
        session.queue().snapshot().await.unwrap().is_empty(),
        "MQ 不得留下半条结果"
    );
}
