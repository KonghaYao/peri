use super::*;
use peri_agent::agent::events::{CompactStrategy, CompactTrigger};

#[test]
fn test_llm_call_end_all_discarded() {
    // usage: Some → discarded (token-usage event deprecated, §C)
    let ev_with_usage = ExecutorEvent::LlmCallEnd {
        step: 1,
        model: "test".into(),
        output: "answer".into(),
        usage: Some(peri_agent::llm::types::TokenUsage {
            input_tokens: 500,
            output_tokens: 200,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
            request_id: None,
        }),
        stop_reason: None,
    };
    assert!(route(&ev_with_usage).is_none());

    // usage: None → discarded (was already in discarded list)
    let ev_no_usage = ExecutorEvent::LlmCallEnd {
        step: 1,
        model: "test".into(),
        output: "answer".into(),
        usage: None,
        stop_reason: None,
    };
    assert!(route(&ev_no_usage).is_none());
}

#[test]
fn test_context_warning_routes_to_budget_warning() {
    let ev = ExecutorEvent::ContextWarning {
        used_tokens: 85000,
        total_tokens: 100000,
        percentage: 0.85,
    };
    let out = route(&ev).unwrap();
    assert_eq!(out.event_name, "budget-warning");
    assert_eq!(out.data["used"], 85000);
    assert_eq!(out.data["limit"], 100000);
    assert_eq!(out.data["threshold"], "0.85");
}

#[test]
fn test_context_warning_070_threshold() {
    let ev = ExecutorEvent::ContextWarning {
        used_tokens: 70000,
        total_tokens: 100000,
        percentage: 0.70,
    };
    let out = route(&ev).unwrap();
    assert_eq!(out.data["threshold"], "0.70");
}

#[test]
fn test_rewind_completed_routes() {
    let msgs = vec![
        peri_agent::messages::BaseMessage::human(peri_agent::messages::MessageContent::text(
            "hello",
        )),
        peri_agent::messages::BaseMessage::ai(peri_agent::messages::MessageContent::text("world")),
    ];
    let ev = ExecutorEvent::RewindCompleted {
        summary: "rolled back 2 messages".into(),
        messages: msgs,
    };
    let out = route(&ev).unwrap();
    assert_eq!(out.event_name, "rewind-preview");
    assert_eq!(out.data["messages"].as_array().unwrap().len(), 2);
}

// ── Discarded events ────────────────────────────────────────────────────

#[test]
fn test_llm_retrying_discarded() {
    let ev = ExecutorEvent::LlmRetrying {
        attempt: 1,
        max_attempts: 3,
        delay_ms: 1000,
        error: "rate limited".into(),
    };
    assert!(route(&ev).is_none());
}

#[test]
fn test_lsp_diagnostics_discarded() {
    let ev = ExecutorEvent::LspDiagnostics {
        errors: 1,
        warnings: 2,
        files_with_errors: 1,
    };
    assert!(route(&ev).is_none());
}

#[test]
fn test_compact_started_discarded() {
    assert!(route(&ExecutorEvent::CompactStarted {
        turn_id: String::new(),
        agent_id: String::new(),
        step: 0,
        strategy: CompactStrategy::Smart,
        trigger: CompactTrigger::Auto,
    })
    .is_none());
}

#[test]
fn test_compact_completed_discarded() {
    let ev = ExecutorEvent::CompactCompleted {
        summary: "done".into(),
        files: vec![],
        skills: vec![],
        micro_cleared: 0,
        messages: vec![],
        token_before: 0,
        token_after: 0,
        strategy: CompactStrategy::Smart,
    };
    assert!(route(&ev).is_none());
}

#[test]
fn test_compact_error_discarded() {
    let ev = ExecutorEvent::CompactError {
        message: "failed".into(),
    };
    assert!(route(&ev).is_none());
}

#[test]
fn test_message_added_discarded() {
    let ev = ExecutorEvent::MessageAdded(peri_agent::messages::BaseMessage::human(
        peri_agent::messages::MessageContent::text("test"),
    ));
    assert!(route(&ev).is_none());
}

#[test]
fn test_state_snapshot_discarded() {
    let ev = ExecutorEvent::StateSnapshot(vec![]);
    assert!(route(&ev).is_none());
}

#[test]
fn test_llm_call_start_discarded() {
    let ev = ExecutorEvent::LlmCallStart {
        step: 1,
        messages: std::sync::Arc::new(vec![]),
        tools: vec![],
    };
    assert!(route(&ev).is_none());
}

#[test]
fn test_llm_request_payload_discarded() {
    let ev = ExecutorEvent::LlmRequestPayload {
        step: 1,
        body: std::sync::Arc::new(serde_json::Value::Null),
    };
    assert!(route(&ev).is_none());
}

#[test]
fn test_background_task_completed_discarded() {
    let ev =
        ExecutorEvent::BackgroundTaskCompleted(peri_agent::agent::events::BackgroundTaskResult {
            task_id: "t-1".into(),
            agent_name: "worker".into(),
            prompt_summary: "test".into(),
            success: true,
            output: "done".into(),
            tool_calls_count: 2,
            duration_ms: 1000,
            child_thread_id: None,
        });
    assert!(route(&ev).is_none());
}

#[test]
fn test_bg_tool_step_discarded() {
    let ev = ExecutorEvent::BgToolStep {
        child_thread_id: "ct-1".into(),
    };
    assert!(route(&ev).is_none());
}

#[test]
fn test_workflow_progress_discarded() {
    let ev = ExecutorEvent::WorkflowProgress(peri_agent::agent::events::WorkflowProgressPayload {
        run_id: "r-1".into(),
        workflow_name: "review".into(),
        event_type: "run_started".into(),
        agent_id: None,
        phase: None,
        label: None,
        agent_status: None,
        token_count: None,
        tool_count: None,
        run_status: None,
        message: None,
    });
    assert!(route(&ev).is_none());
}

#[test]
fn test_todo_update_discarded() {
    let ev = ExecutorEvent::TodoUpdate(vec![]);
    assert!(route(&ev).is_none());
}

#[test]
fn test_state_snapshot_meta_discarded() {
    let ev = ExecutorEvent::StateSnapshotMeta {
        message_count: 10,
        total_tokens: 5000,
        current_step: 3,
        consecutive_failures: 0,
        budget_pct: Some(0.5),
        context_total_tokens: Some(200_000),
    };
    assert!(route(&ev).is_none());
}

// ── Helper tests ─────────────────────────────────────────────────────────
// summarize_input / summarize_output / truncate_text 的单元测试
// 已随实现一起迁移至 `super::truncate` 模块，这里不再重复。
