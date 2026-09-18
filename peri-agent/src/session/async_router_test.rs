//! Tests for async_router

use super::*;
use peri_acp_types::session::{QueuedPayload, SessionInbox};
use peri_acp_types::tasks::BgTaskKind;
use std::sync::Arc;

fn make_inbox() -> (SessionInbox, InboxHandle) {
    let queue = Arc::new(peri_acp_types::session::MessageQueue::new());
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
        subagent_failure: None,
        shell_output: None,
    }
}

#[test]
fn test_route_bg_result_pushes_defer() {
    let (inbox, handle) = make_inbox();
    let router = AsyncRouter::new(handle);
    let result = make_bg_result("abc123", "test-agent", "done");

    router.route_bg_result(&result, BgTaskKind::Agent);

    assert_eq!(inbox.queue().len(), 1);
    assert!(inbox.queue().has_wake_up(), "Defer should wake the inbox");
}

#[test]
fn test_route_bg_result_uses_subagent_complete_source() {
    let (inbox, handle) = make_inbox();
    let router = AsyncRouter::new(handle);
    let result = make_bg_result("abc123", "test-agent", "done");

    router.route_bg_result(&result, BgTaskKind::Agent);

    let msgs = inbox.queue().drain_all();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].source, MessageSource::SubAgentComplete);
}

#[test]
fn test_route_bg_result_shell_uses_shell_complete_source() {
    let (inbox, handle) = make_inbox();
    let router = AsyncRouter::new(handle);
    let result = make_bg_result("shell-123", "Bash", "done");

    router.route_bg_result(&result, BgTaskKind::Shell);

    assert!(
        !inbox
            .queue()
            .has_pending_defer(&MessageSource::SubAgentComplete),
        "shell completion must not qualify as a background-agent callback"
    );
    let msgs = inbox.queue().drain_all();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].source, MessageSource::ShellComplete);
}

#[test]
fn test_route_bg_result_workflow_uses_workflow_complete_source() {
    let (inbox, handle) = make_inbox();
    let router = AsyncRouter::new(handle);
    let result = make_bg_result("workflow-123", "workflow", "done");

    router.route_bg_result(&result, BgTaskKind::Workflow);

    let msgs = inbox.queue().drain_all();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].source, MessageSource::WorkflowComplete);
}

#[test]
fn test_route_bg_result_notification_text_contains_task_info() {
    let (inbox, handle) = make_inbox();
    let router = AsyncRouter::new(handle);
    let result = make_bg_result("task-12345", "my-agent", "output text");

    router.route_bg_result(&result, BgTaskKind::Agent);

    let msgs = inbox.queue().drain_all();
    let text = match &msgs[0].payload {
        QueuedPayload::SystemReminder(r) => r.as_reminder().body.as_str(),
        _ => panic!("expected reminder"),
    };
    assert!(text.contains("task-12"), "should contain short task_id");
    assert!(text.contains("my-agent"), "should contain agent_name");
    assert!(text.contains("output text"), "should contain output");
}

/// [回归测试] 后台 shell 大输出曾经触发 64 KiB reminder 校验 panic。
#[test]
fn test_route_shell_output_only_injects_file_references() {
    let (inbox, handle) = make_inbox();
    let router = AsyncRouter::new(handle);
    let mut result = make_bg_result("shell-output", "bg-shell", &"私有输出正文".repeat(100_000));
    result.success = false;
    result.shell_output = Some(Box::new(peri_acp_types::event::ShellOutput {
        stdout_path: Some("/tmp/fixture.stdout.log".into()),
        stderr_path: Some("/tmp/fixture.stderr.log".into()),
        complete: true,
        error: None,
        exit_code: Some(1),
    }));
    router.route_bg_result(&result, BgTaskKind::Shell);
    assert!(inbox.queue().has_wake_up());
    let messages = inbox.queue().drain_all();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].source, MessageSource::ShellComplete);
    let QueuedPayload::SystemReminder(reminder) = &messages[0].payload else {
        panic!("完成结果必须使用 canonical reminder");
    };
    let reminder = reminder.as_reminder();
    assert!(reminder.validate().is_ok());
    let encoded = serde_json::to_string(reminder).unwrap();
    assert!(encoded.len() < 2_048, "通知大小应与原输出大小无关");
    assert!(
        !encoded.contains("私有输出正文"),
        "正文不能绕过 body 进入 metadata 或 summary"
    );
    assert!(reminder.body.contains("/tmp/fixture.stdout.log"));
    assert!(reminder.body.contains("/tmp/fixture.stderr.log"));
    assert!(reminder.body.contains("Read"));
    assert!(reminder.body.contains("退出码 1"));
    assert_eq!(reminder.severity, ReminderSeverity::Error);
}

#[test]
fn test_invalid_background_notification_reports_delivery_failure_without_panicking() {
    let (inbox, handle) = make_inbox();
    let router = AsyncRouter::new(handle);
    let mut result = make_bg_result("shell-invalid", "bg-shell", "private output");
    result.shell_output = Some(Box::new(peri_acp_types::event::ShellOutput {
        stdout_path: Some("unrepresentable-path".repeat(10_000)),
        stderr_path: None,
        complete: false,
        error: None,
        exit_code: Some(0),
    }));
    router.route_bg_result(&result, BgTaskKind::Shell);
    assert!(inbox.queue().has_wake_up());
    let messages = inbox.queue().drain_all();
    assert_eq!(messages.len(), 1);
    let QueuedPayload::SystemReminder(reminder) = &messages[0].payload else {
        panic!("expected diagnostic reminder");
    };
    let reminder = reminder.as_reminder();
    assert_eq!(reminder.kind, "notification_failed");
    assert_eq!(reminder.metadata["success"], true);
    assert!(reminder.validate().is_ok());
    let encoded = serde_json::to_string(reminder).unwrap();
    assert!(encoded.len() < 2_048);
    assert!(!encoded.contains("unrepresentable-path"));
    assert!(!encoded.contains("private output"));
}

#[test]
fn test_route_workflow_event_pushes_defer() {
    let (inbox, handle) = make_inbox();
    let router = AsyncRouter::new(handle);

    router.route_workflow_event(
        "wf-run-999",
        "deploy-pipeline",
        "completed",
        5000,
        4,
        12,
        &[],
    );

    assert_eq!(inbox.queue().len(), 1);
    assert!(inbox.queue().has_wake_up(), "Defer should wake the inbox");
}

#[test]
fn test_route_workflow_event_uses_workflow_complete_source() {
    let (inbox, handle) = make_inbox();
    let router = AsyncRouter::new(handle);

    router.route_workflow_event(
        "wf-run-999",
        "deploy-pipeline",
        "completed",
        5000,
        4,
        12,
        &[],
    );

    let msgs = inbox.queue().drain_all();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].source, MessageSource::WorkflowComplete);
}

#[test]
fn test_route_workflow_event_notification_format() {
    let (inbox, handle) = make_inbox();
    let router = AsyncRouter::new(handle);

    router.route_workflow_event(
        "wf-run-999",
        "deploy-pipeline",
        "completed",
        5000,
        4,
        12,
        &[],
    );

    let msgs = inbox.queue().drain_all();
    let text = match &msgs[0].payload {
        QueuedPayload::SystemReminder(r) => r.as_reminder().body.as_str(),
        _ => panic!("expected reminder"),
    };
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

/// [回归测试] route_workflow_event 的 status 文本必须区分 completed/killed/failed
/// （issue 2026-08-05：kill/failed 被误报为 "completed" 的幽灵完成事件）。
#[test]
fn test_route_workflow_event_status_text_distinguishes_killed_failed() {
    let (inbox, handle) = make_inbox();
    let router = AsyncRouter::new(handle);

    router.route_workflow_event("wf-killed", "deploy", "killed", 100, 1, 2, &[]);
    router.route_workflow_event("wf-failed", "deploy", "failed", 100, 1, 2, &[]);
    router.route_workflow_event("wf-ok", "deploy", "completed", 100, 1, 2, &[]);

    let msgs = inbox.queue().drain_all();
    assert_eq!(msgs.len(), 3);
    let texts: Vec<String> = msgs
        .iter()
        .map(|m| match &m.payload {
            QueuedPayload::SystemReminder(r) => r.as_reminder().body.clone(),
            _ => panic!("expected reminder"),
        })
        .collect();
    assert!(
        texts[0].contains("'deploy' killed."),
        "killed 文本应显示 killed，实际: {}",
        texts[0]
    );
    assert!(
        texts[1].contains("'deploy' failed."),
        "failed 文本应显示 failed，实际: {}",
        texts[1]
    );
    assert!(
        texts[2].contains("'deploy' completed."),
        "completed 文本应显示 completed，实际: {}",
        texts[2]
    );
    assert!(
        !texts[0].contains("completed.") && !texts[1].contains("completed."),
        "killed/failed 不得显示为 completed"
    );
}

#[test]
fn test_multiple_routes_accumulate_in_queue() {
    let (inbox, handle) = make_inbox();
    let router = AsyncRouter::new(handle);

    let result1 = make_bg_result("task-1", "agent-a", "output-a");
    let result2 = make_bg_result("task-2", "agent-b", "output-b");
    router.route_bg_result(&result1, BgTaskKind::Agent);
    router.route_workflow_event("wf-3", "test-wf", "completed", 100, 1, 2, &[]);
    router.route_bg_result(&result2, BgTaskKind::Agent);

    assert_eq!(inbox.queue().len(), 3);

    let msgs = inbox.queue().drain_all();
    assert_eq!(msgs[0].source, MessageSource::SubAgentComplete);
    assert_eq!(msgs[1].source, MessageSource::WorkflowComplete);
    assert_eq!(msgs[2].source, MessageSource::SubAgentComplete);
}
