//! 任务账本（投影）中的 child thread 身份回归。
//!
//! 契约：`TaskRecord.child_thread_id` 是后台 Agent 任务注册时从 live inbox 取的真实
//! 线程身份，与 `task_id`（`bg-{uuid}`）是两个领域身份，不得由前缀推导；live 执行
//! 结束后投影仍保留该字段，机器消费者不必依赖到达顺序或历史回执文本去猜。

use super::*;
use crate::agent::async_tasks::{BackgroundTask, BackgroundTaskStatus, BgCancelHandle};
use peri_acp_types::session::MessageQueue;
use std::sync::Arc;

/// 历史样本的真实形态：task id 与 child thread id 独立生成，前段相同、尾段不同。
const TASK_ID: &str = "bg-01a1407d-c445-7ac3-a7ab-db7f4535cb10";
const CHILD_THREAD_ID: &str = "01a1407d-c445-7ac3-a7ab-db6d6e66266c";

fn base_task(kind: BgTaskKind, agent_inbox: Option<Arc<BackgroundAgentInbox>>) -> BackgroundTask {
    BackgroundTask {
        id: TASK_ID.into(),
        agent_name: "fixture".into(),
        prompt_summary: "identity".into(),
        status: BackgroundTaskStatus::Running,
        started_at: std::time::Instant::now(),
        chrono_started_at: chrono::Utc::now(),
        kind,
        cancel_handle: BgCancelHandle::Kill(Some(Box::new(|| {}))),
        cancel_token: None,
        pid: None,
        output_preview: None,
        agent_inbox,
        initiator_session_id: None,
        owner_session_id: None,
        owner_identity: None,
    }
}

fn terminal_result() -> BackgroundTaskResult {
    BackgroundTaskResult {
        task_id: TASK_ID.into(),
        agent_name: "fixture".into(),
        prompt_summary: "identity".into(),
        success: true,
        output: "done".into(),
        tool_calls_count: 0,
        duration_ms: 1,
        child_thread_id: Some(CHILD_THREAD_ID.into()),
        timed_out: false,
        subagent_failure: None,
        shell_output: None,
    }
}

fn record(registry: &BackgroundTaskRegistry) -> peri_acp_types::tasks::TaskRecord {
    registry
        .snapshot()
        .tasks
        .into_iter()
        .find(|task| task.task_id == TASK_ID)
        .expect("任务应在投影中")
}

/// 注册的 Agent 任务在投影里记录真实线程身份，且 live 执行结束（终态回收 inbox）
/// 后仍保留——这是「任务账本记录正确字段」的最低要求。
#[tokio::test]
async fn agent_task_projection_keeps_child_thread_identity_past_terminal() {
    let registry = BackgroundTaskRegistry::new();
    let inbox = BackgroundAgentInbox::new(
        CHILD_THREAD_ID.into(),
        MessageQueue::new(),
        CancellationToken::new(),
    );
    let ticket = registry
        .register_with_kind(base_task(BgTaskKind::Agent, Some(inbox)))
        .unwrap();

    let live = record(&registry);
    assert_eq!(
        live.child_thread_id.as_deref(),
        Some(CHILD_THREAD_ID),
        "live 期投影应记录真实线程身份"
    );
    assert_ne!(
        live.task_id,
        live.child_thread_id.clone().unwrap(),
        "task 身份与线程身份是两个字段，不得同值改写"
    );

    assert!(registry.complete(&ticket, terminal_result()));
    let settled = record(&registry);
    assert_eq!(
        settled.child_thread_id.as_deref(),
        Some(CHILD_THREAD_ID),
        "终态回收 live inbox 后投影仍保留线程身份"
    );
}

/// 非 Agent 任务（Shell / Workflow / Mcp）没有线程身份：字段保持缺失，
/// 不伪造、不由 task id 推导。
#[tokio::test]
async fn non_agent_task_projection_has_no_child_thread_identity() {
    for kind in [BgTaskKind::Shell, BgTaskKind::Workflow, BgTaskKind::Mcp] {
        let registry = BackgroundTaskRegistry::new();
        registry.register_with_kind(base_task(kind, None)).unwrap();
        assert!(
            record(&registry).child_thread_id.is_none(),
            "{kind:?} 任务不得携带线程身份"
        );
    }
}
