use super::*;
use crate::kit::acp_types::AcpEventData;
use crate::kit::atoms::{BG_DISPLAY, BG_LIVE_DETAIL};
use crate::kit::stream_data::{TuiTextChunk, TuiToolEnded, TuiToolStarted};
use crate::kit::tui_render_unit::TuiRenderUnit;
use serial_test::serial;

#[test]
#[serial]
fn test_bg_task_cancelled_persists_reason_on_live_detail() {
    crate::kit::atoms::init_atoms();
    BG_LIVE_DETAIL.state().write().clear();
    dispatch_and_notify(
        &mut make_state(),
        &AcpEventData::BgTaskStarted(crate::kit::acp_types::BgTaskEntry {
            task_id: "task-shell".into(),
            kind: "shell".into(),
            summary: "echo".into(),
            started_at: String::new(),
            pid: None,
        }),
    );
    dispatch_and_notify(
        &mut make_state(),
        &AcpEventData::BgTaskCancelled {
            task_id: "task-shell".into(),
            reason: "user cancelled".into(),
        },
    );
    let live_store = BG_LIVE_DETAIL.state();
    let live_guard = live_store.read();
    let detail = live_guard.get("task-shell").expect("live detail");
    assert_eq!(detail.cancel_reason.as_deref(), Some("user cancelled"));
}

#[test]
#[serial]
fn test_bg_text_chunk_appends_live_detail_not_view_models() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    BG_DISPLAY.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    let mut state = make_state();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskStarted(crate::kit::acp_types::BgTaskEntry {
            task_id: "task-1".into(),
            kind: "agent".into(),
            summary: "bg".into(),
            started_at: String::new(),
            pid: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "bg-agent".into(),
            agent_name: "coder".into(),
            is_background: true,
        },
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnSuspended);
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "live line".into(),
            message_id: None,
            agent_id: Some("bg-agent".into()),
        }),
    );
    let live_store = BG_LIVE_DETAIL.state();
    let live_guard = live_store.read();
    let detail = live_guard.get("task-1").expect("projection");
    assert!(
        !detail.nested_units.is_empty(),
        "bg chunk should append to BG_LIVE_DETAIL"
    );
    assert!(state.current_turn.text.is_empty());
}

#[test]
#[serial]
fn test_bg_tool_sync_preserves_event_order() {
    crate::kit::atoms::init_atoms();
    BG_LIVE_DETAIL.state().write().clear();
    BG_DISPLAY
        .state()
        .write()
        .push(crate::kit::atoms::BgDisplayEntry {
            id: "task-order".into(),
            linked_agent_id: Some("bg-order".into()),
            agent_type: "agent".into(),
            desc: "bg".into(),
            current_tool: None,
            tool_count: 0,
            is_active: true,
            is_error: false,
            created_at: std::time::Instant::now(),
            completed_at: None,
        });
    crate::kit::bg_task_live::seed_live_from_started("task-order", "agent", "bg", None);
    crate::kit::bg_task_live::init_agent_live_detail("task-order", "bg-order", "coder");

    crate::kit::bg_task_live::handle_bg_tool_started(
        "bg-order",
        &TuiToolStarted {
            tool_id: "tool-1".into(),
            tool_name: "Bash".into(),
            input_summary: "echo hi".into(),
            raw_input: serde_json::Value::Null,
            agent_id: Some("bg-order".into()),
        },
        None,
    );
    crate::kit::bg_task_live::append_bg_text_chunk(
        "bg-order",
        &TuiTextChunk {
            text: "after".into(),
            message_id: Some("m1".into()),
            agent_id: Some("bg-order".into()),
        },
    );
    crate::kit::bg_task_live::handle_bg_tool_ended(
        "bg-order",
        &TuiToolEnded {
            tool_id: "tool-1".into(),
            output_summary: "done".into(),
            is_error: false,
            agent_id: Some("bg-order".into()),
        },
    );

    let live = BG_LIVE_DETAIL.state();
    let guard = live.read();
    let units = &guard.get("task-order").expect("detail").nested_units;
    assert!(matches!(units.get(0), Some(TuiRenderUnit::TuiToolCard(_))));
    assert!(matches!(
        units.get(1),
        Some(TuiRenderUnit::TuiAssistantBubble(_))
    ));
}

fn make_state() -> BridgeState {
    BridgeState {
        variant: 0,
        committed: im::Vector::new(),
        current_turn: CurrentTurn::new(),
        phase: SessionPhase::Idle,
        popup_kind: None,
        generation: 0,
        active_session_id: String::new(),
        compact_just_completed: false,
        last_submitted_text: None,
        last_pushed_text_len: 0,
        last_pushed_reasoning_len: 0,
        last_successful_todos: None,
        last_successful_todo_sequence: None,
        next_todo_sequence: 0,
        todo_call_inputs: std::collections::HashMap::new(),
        turn_generation: 0,
        last_prompt_generation: 0,
        current_request_id: None,
        pending_cache_usage: None,
    }
}
