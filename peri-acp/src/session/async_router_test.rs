//! Tests for async_router

use super::*;
use peri_agent::agent::session::inbox::SessionInbox;
use std::sync::Arc;

fn make_inbox() -> (SessionInbox, InboxHandle) {
    let queue = Arc::new(peri_agent::session::MessageQueue::new());
    let inbox = SessionInbox::new(queue);
    let handle = inbox.handle();
    (inbox, handle)
}

fn make_bg_result(task_id: &str, agent_name: &str, output: &str) -> BackgroundTaskResult {
    BackgroundTaskResult {
        task_id: task_id.to_string(),
        agent_name: agent_name.to_string(),
        prompt_summary: "test prompt".to_string(),
        success: true,
        output: output.to_string(),
        tool_calls_count: 3,
        duration_ms: 1500,
        child_thread_id: None,
        timed_out: false,
    }
}

#[test]
fn test_route_bg_result_pushes_defer() {
    let (inbox, handle) = make_inbox();
    let router = AsyncRouter::new(handle);
    let result = make_bg_result("abc123", "test-agent", "done");

    router.route_bg_result(&result);

    assert_eq!(inbox.queue().len(), 1);
    assert!(inbox.queue().has_wake_up(), "Defer should wake the inbox");
}

#[test]
fn test_route_bg_result_uses_subagent_complete_source() {
    let (inbox, handle) = make_inbox();
    let router = AsyncRouter::new(handle);
    let result = make_bg_result("abc123", "test-agent", "done");

    router.route_bg_result(&result);

    let msgs = inbox.queue().drain_all();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].source, MessageSource::SubAgentComplete);
}

#[test]
fn test_route_bg_result_notification_text_contains_task_info() {
    let (inbox, handle) = make_inbox();
    let router = AsyncRouter::new(handle);
    let result = make_bg_result("task-12345", "my-agent", "output text");

    router.route_bg_result(&result);

    let msgs = inbox.queue().drain_all();
    let text = msgs[0].message.content();
    assert!(text.contains("task-12"), "should contain short task_id");
    assert!(text.contains("my-agent"), "should contain agent_name");
    assert!(text.contains("output text"), "should contain output");
}

#[test]
fn test_route_workflow_event_pushes_defer() {
    let (inbox, handle) = make_inbox();
    let router = AsyncRouter::new(handle);

    router.route_workflow_event("wf-run-999", "deploy-pipeline", 5000, 4, 12, &[]);

    assert_eq!(inbox.queue().len(), 1);
    assert!(inbox.queue().has_wake_up(), "Defer should wake the inbox");
}

#[test]
fn test_route_workflow_event_uses_workflow_complete_source() {
    let (inbox, handle) = make_inbox();
    let router = AsyncRouter::new(handle);

    router.route_workflow_event("wf-run-999", "deploy-pipeline", 5000, 4, 12, &[]);

    let msgs = inbox.queue().drain_all();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].source, MessageSource::WorkflowComplete);
}

#[test]
fn test_route_workflow_event_notification_format() {
    let (inbox, handle) = make_inbox();
    let router = AsyncRouter::new(handle);

    router.route_workflow_event("wf-run-999", "deploy-pipeline", 5000, 4, 12, &[]);

    let msgs = inbox.queue().drain_all();
    let text = msgs[0].message.content();
    assert!(text.contains("wf-run-"), "should contain short run_id");
    assert!(
        text.contains("deploy-pipeline"),
        "should contain workflow_name"
    );
    assert!(text.contains("5000ms"), "should contain duration");
    assert!(text.contains("4 agents"), "should contain agent count");
    assert!(
        text.contains("12 tool calls"),
        "should contain tool_calls_count"
    );
}

#[test]
fn test_multiple_routes_accumulate_in_queue() {
    let (inbox, handle) = make_inbox();
    let router = AsyncRouter::new(handle);

    let result1 = make_bg_result("task-1", "agent-a", "output-a");
    let result2 = make_bg_result("task-2", "agent-b", "output-b");
    router.route_bg_result(&result1);
    router.route_workflow_event("wf-3", "test-wf", 100, 1, 2, &[]);
    router.route_bg_result(&result2);

    assert_eq!(inbox.queue().len(), 3);

    let msgs = inbox.queue().drain_all();
    assert_eq!(msgs[0].source, MessageSource::SubAgentComplete);
    assert_eq!(msgs[1].source, MessageSource::WorkflowComplete);
    assert_eq!(msgs[2].source, MessageSource::SubAgentComplete);
}
