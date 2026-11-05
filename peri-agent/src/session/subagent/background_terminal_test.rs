//! 后台 child 的终态兜底：没有投递路由时也必须一次写定终态与工作意图。

use super::*;
use crate::agent::events::BackgroundTaskResult;
use crate::session::test_resources::mock::MockSessionResources;
use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::thread::WorkState;

fn result() -> BackgroundTaskResult {
    BackgroundTaskResult {
        task_id: "task-1".into(),
        agent_name: "worker".into(),
        prompt_summary: "task".into(),
        success: true,
        output: "done".into(),
        tool_calls_count: 0,
        duration_ms: 0,
        timed_out: false,
        child_thread_id: Some("child".into()),
        subagent_failure: None,
        shell_output: None,
    }
}

fn resources() -> Arc<MockSessionResources> {
    let store = MockSessionResources::new();
    store.register_bound_session("child", "/tmp");
    store
}

async fn persisted(store: &MockSessionResources, status: AgentStatus) -> BackgroundTaskResult {
    store
        .set_work_state(&ThreadId::from("child"), WorkState::Working)
        .await
        .unwrap();
    let mut outcome = result();
    let store_ref: &dyn SessionResources = store;
    assert!(
        persist_background_terminal(Some(store_ref), "child", status, &mut outcome, None).await,
        "有存储时终态兜底必须成功"
    );
    outcome
}

/// 无 route 的完成兜底一次写定 Done + Idle，不残留 Done + working。
#[tokio::test]
async fn background_without_route_settles_done_idle() {
    let store = resources();
    persisted(&store, AgentStatus::Done).await;

    let meta = store
        .load_session_meta(&ThreadId::from("child"))
        .await
        .unwrap();
    assert_eq!(meta.agent_status, AgentStatus::Done);
    assert_eq!(meta.work_state, WorkState::Idle);
    assert_eq!(store.status_writes(), 1, "终态只能由一个写入点提交");
}

/// 无 route 的失败兜底一次写定 Error + Idle，不残留 Error + working。
#[tokio::test]
async fn background_without_route_settles_error_idle() {
    let store = resources();
    persisted(&store, AgentStatus::Error).await;

    let meta = store
        .load_session_meta(&ThreadId::from("child"))
        .await
        .unwrap();
    assert_eq!(meta.agent_status, AgentStatus::Error);
    assert_eq!(meta.work_state, WorkState::Idle);
    assert_eq!(store.status_writes(), 1, "终态只能由一个写入点提交");
}

/// 没有 SessionResources 时不写半状态：返回失败并让结果携带诊断。
#[tokio::test]
async fn background_without_resources_fails_closed() {
    let mut outcome = result();
    assert!(
        !persist_background_terminal(None, "child", AgentStatus::Done, &mut outcome, None).await
    );
    assert!(!outcome.success);
    assert!(
        outcome.output.contains("session resources unavailable"),
        "失败诊断必须可见: {}",
        outcome.output
    );
}
