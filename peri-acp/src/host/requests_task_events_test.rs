use super::*;
use peri_acp_types::{event::BackgroundTaskResult, tasks::BgTaskKind};

#[test]
fn completed_executor_cancellation_uses_existing_cancelled_wire_event() {
    let result = BackgroundTaskResult {
        task_id: "mcp-task".into(),
        agent_name: "bg-shell".into(),
        prompt_summary: "command".into(),
        success: false,
        output: "Shell command cancelled; read the output files as needed.".into(),
        tool_calls_count: 0,
        duration_ms: 15,
        timed_out: false,
        child_thread_id: None,
        subagent_failure: None,
        shell_output: None,
    };
    let change = |result: BackgroundTaskResult| TaskChange {
        revision: 12,
        event: BgRegistryEvent::Completed {
            task_id: "mcp-task".into(),
            kind: Some(BgTaskKind::Shell),
            success: false,
            output_preview: result.output.clone(),
            duration_ms: 15,
            result,
        },
    };
    let (event, data) = task_change_value(change(result.clone()));
    assert_eq!(event, "bg-task-cancelled");
    assert_eq!(
        data,
        json!({"task_id":"mcp-task","reason":"cancelled","revision":12})
    );
    let mut timed_out = result;
    timed_out.timed_out = true;
    let (event, data) = task_change_value(change(timed_out));
    assert_eq!(event, "bg-task-completed");
    assert_eq!(data["success"], false);
    assert_eq!(data["revision"], 12);
    let mut untrusted = change(evidence_with_cancelled_text());
    if let BgRegistryEvent::Completed { kind, .. } = &mut untrusted.event {
        *kind = Some(BgTaskKind::Mcp);
    }
    assert_eq!(task_change_value(untrusted).0, "bg-task-completed");
}

fn evidence_with_cancelled_text() -> BackgroundTaskResult {
    BackgroundTaskResult {
        task_id: "mcp-task".into(),
        agent_name: "mcp".into(),
        prompt_summary: "external".into(),
        success: false,
        output: "Shell command cancelled".into(),
        tool_calls_count: 0,
        duration_ms: 0,
        timed_out: false,
        child_thread_id: None,
        subagent_failure: None,
        shell_output: None,
    }
}
