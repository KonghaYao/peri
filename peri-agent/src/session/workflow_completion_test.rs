use std::sync::Arc;

use peri_acp_types::session::{MessageKind, MessageSource, SessionInbox};
use peri_acp_types::tasks::{BgTaskKind, BgTaskRegistration, TaskManager};
use peri_acp_types::workflow::{WorkflowRunStatus, WorkflowTaskResult};

use crate::agent::async_tasks::TaskManager as BgTaskManager;
use crate::session::async_router::AsyncRouter;
use crate::session::workflow_completion::apply_workflow_task_result;

fn make_inbox() -> (SessionInbox, peri_acp_types::session::InboxHandle) {
    let queue = Arc::new(peri_acp_types::session::MessageQueue::new());
    let inbox = SessionInbox::new(queue);
    let handle = inbox.handle();
    (inbox, handle)
}

fn failed_task_result(run_id: &str) -> WorkflowTaskResult {
    WorkflowTaskResult {
        run_id: run_id.to_string(),
        workflow_name: "fast-fail".to_string(),
        success: false,
        status: WorkflowRunStatus::Failed,
        execution_status: Default::default(),
        acceptance_status: Default::default(),
        post_processing_status: Default::default(),
        delivery_status: Default::default(),
        state_artifact_exists: false,
        duration_ms: 12,
        agent_count: 0,
        tool_calls_count: 0,
        error: Some("boom".to_string()),
        phase_summaries: vec![],
        attempts: vec![],
    }
}

/// #117: session consumer must enqueue Defer (and wake) before `TaskManager::complete` clears
/// `active_count`, so `idle_should_wait` can suspend the loop until the failure is routed.
#[test]
fn consumer_applies_defer_before_clearing_active_count() {
    let run_id = "01900000-0000-7000-8000-000000000117";
    let bg: Arc<dyn TaskManager> = Arc::new(BgTaskManager::new());
    bg.register(BgTaskRegistration {
        task_id: run_id.to_string(),
        kind: BgTaskKind::Workflow,
        summary: "fast-fail".to_string(),
        pid: None,
        kill: None,
    })
    .expect("register workflow bg task");
    assert_eq!(bg.active_count(), 1);

    let (inbox, handle) = make_inbox();
    let router = AsyncRouter::new(handle);
    let task_result = failed_task_result(run_id);

    apply_workflow_task_result(&task_result, Some(&router), None, bg.as_ref());

    assert!(
        inbox.queue().has_wake_up(),
        "Defer must wake inbox before active_count drops"
    );
    let msgs = inbox.queue().drain_all();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].kind, MessageKind::Defer);
    assert_eq!(msgs[0].source, MessageSource::WorkflowComplete);
    let text = msgs[0].message.content();
    assert!(
        text.contains("failed"),
        "notification text should use failed status word: {text}"
    );

    assert_eq!(
        bg.active_count(),
        0,
        "complete must run after Defer is queued"
    );
}
