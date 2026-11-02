use super::*;
use crate::kit::acp_events::acp_events_test::make_fold_test_state;
use crate::kit::acp_events::tool;
use crate::kit::atoms::{
    BG_AGENT_IDS, BG_DISPLAY, BG_LIVE_DETAIL, BG_TASK_IDENTITY, BgDisplayEntry, BgLiveDetail,
    BgTaskIdentity,
};
use crate::kit::stream_data::TuiToolStarted;
use crate::kit::tui_render_unit::{TuiRenderUnit, TuiToolPresentation};
use serde_json::json;
use serial_test::serial;
use std::collections::{HashMap, HashSet};

fn todos(status: &str) -> Value {
    json!({"todos": [{"content": "task", "status": status}]})
}

#[test]
#[serial]
fn oversized_input_blocks_older_success_until_a_complete_new_success() {
    let mut state = make_fold_test_state();
    state.record_todo_started("baseline".into(), &todos("pending"), INPUT_BYTES);
    assert!(state.complete_todo_if_current("baseline", false));
    state.record_todo_started("old-pending".into(), &todos("in_progress"), INPUT_BYTES);
    let oversized = json!({"todos": [{"content": "x".repeat(INPUT_BYTES), "status": "completed"}]});
    state.record_todo_started("omitted".into(), &oversized, INPUT_BYTES);
    assert!(state.todo_baseline_unknown());
    assert_eq!(state.todo_baseline_sequence, Some(2));
    assert!(!state.todo_call_inputs.contains_key("omitted"));
    assert!(!state.complete_todo_if_current("old-pending", false));
    assert!(!state.complete_todo_if_current("omitted", false));
    assert!(state.todo_baseline_unknown());

    state.record_todo_started("failed".into(), &todos("completed"), INPUT_BYTES);
    assert!(!state.complete_todo_if_current("failed", true));
    assert!(state.todo_baseline_unknown());
    state.record_todo_started("recovered".into(), &todos("completed"), INPUT_BYTES);
    assert!(state.complete_todo_if_current("recovered", false));
    assert!(!state.todo_baseline_unknown());
    assert_eq!(state.todo_baseline_sequence, Some(4));
    assert_eq!(
        state.last_successful_todos,
        TodoSnapshot::parse(&todos("completed"))
    );
}

#[test]
#[serial]
fn evicted_and_older_completions_cannot_replace_a_newer_baseline() {
    let mut state = make_fold_test_state();
    for index in 0..=CACHE_ENTRIES {
        state.record_todo_started(format!("pending-{index}"), &todos("pending"), INPUT_BYTES);
    }
    assert_eq!(state.todo_call_inputs.len(), CACHE_ENTRIES);
    assert!(state.todo_call_inputs.capacity() <= CACHE_ENTRIES * 2);
    assert!(state.todo_baseline_unknown());
    assert!(!state.complete_todo_if_current("pending-0", false));
    assert!(state.todo_baseline_unknown());

    state.record_todo_started("new-success".into(), &todos("completed"), INPUT_BYTES);
    assert!(state.complete_todo_if_current("new-success", false));
    let recovered = state.last_successful_todos.clone();
    let frontier = state.todo_baseline_sequence;
    assert!(!state.complete_todo_if_current("pending-2", false));
    assert_eq!(state.last_successful_todos, recovered);
    assert_eq!(state.todo_baseline_sequence, frontier);

    // Evict more older pending calls after recovery. Their loss must not erase a
    // newer complete baseline, nor may their late success roll it back.
    for index in 0..4 {
        state.record_todo_started(format!("after-{index}"), &todos("pending"), INPUT_BYTES);
    }
    assert!(!state.todo_baseline_unknown());
    assert_eq!(state.last_successful_todos, recovered);
    assert!(!state.complete_todo_if_current("pending-3", false));
    assert_eq!(state.last_successful_todos, recovered);
}

#[test]
#[serial]
fn missing_and_malformed_inputs_do_not_restore_initial_state() {
    let mut state = make_fold_test_state();
    state.record_todo_started("missing".into(), &Value::Null, INPUT_BYTES);
    assert!(state.todo_baseline_unknown());
    assert!(state.todo_call_inputs.is_empty());
    state.record_todo_started("malformed".into(), &json!({"todos": [false]}), INPUT_BYTES);
    assert!(!state.complete_todo_if_current("malformed", false));
    assert!(state.todo_baseline_unknown());
    assert_eq!(state.todo_baseline_sequence, Some(1));
}

#[test]
#[serial]
fn pending_comparison_inputs_have_a_byte_limit_as_well_as_an_entry_limit() {
    let mut state = make_fold_test_state();
    let input = json!({"todos": [{"content": "x".repeat(INPUT_BYTES / 2), "status": "pending"}]});
    for index in 0..CACHE_ENTRIES {
        state.record_todo_started(format!("input-{index}"), &input, INPUT_BYTES);
    }
    let reserved: usize = state
        .todo_call_inputs
        .iter()
        .map(|(id, (_, input))| id.capacity() + json_value_bytes(input).reserved)
        .sum();
    assert!(reserved <= CACHE_BYTES);
    assert!(state.todo_call_inputs.len() < CACHE_ENTRIES);
    assert!(state.todo_baseline_unknown());
}

struct RestoreBackground {
    ids: HashSet<String>,
    rows: Vec<BgDisplayEntry>,
    details: HashMap<String, BgLiveDetail>,
    identities: HashMap<String, BgTaskIdentity>,
}

impl RestoreBackground {
    fn empty() -> Self {
        Self {
            ids: std::mem::take(&mut *BG_AGENT_IDS.state().write()),
            rows: std::mem::take(&mut *BG_DISPLAY.state().write()),
            details: std::mem::take(&mut *BG_LIVE_DETAIL.state().write()),
            identities: std::mem::take(&mut *BG_TASK_IDENTITY.state().write()),
        }
    }
}

impl Drop for RestoreBackground {
    fn drop(&mut self) {
        *BG_AGENT_IDS.state().write() = std::mem::take(&mut self.ids);
        *BG_DISPLAY.state().write() = std::mem::take(&mut self.rows);
        *BG_LIVE_DETAIL.state().write() = std::mem::take(&mut self.details);
        *BG_TASK_IDENTITY.state().write() = std::mem::take(&mut self.identities);
    }
}

fn started(id: &str, child: Option<&str>, input: Value) -> TuiToolStarted {
    TuiToolStarted {
        tool_id: id.into(),
        tool_name: "TodoWrite".into(),
        input_summary: "one task".into(),
        raw_input: input,
        agent_id: child.map(str::to_owned),
        origin_message_id: None,
        origin_readable: None,
    }
}

#[test]
#[serial]
fn unknown_baseline_uses_generic_cards_for_root_child_background_and_replay() {
    let mut state = make_fold_test_state();
    let _restore = RestoreBackground::empty();
    state.record_todo_started("missing".into(), &Value::Null, INPUT_BYTES);
    tool::handle_tool_started(&mut state, &started("root", None, todos("pending")));
    assert!(matches!(
        &state.current_turn.tool_cards[0].presentation,
        TuiToolPresentation::Generic
    ));

    state
        .current_turn
        .start_subagent("sync-child".into(), "coder".into(), None);
    tool::handle_tool_started(
        &mut state,
        &started("sync", Some("sync-child"), todos("pending")),
    );
    assert!(matches!(
        &state.current_turn.subagents[0].child_turn.tool_cards[0].presentation,
        TuiToolPresentation::Generic
    ));

    BG_AGENT_IDS.state().write().insert("bg-child".into());
    BG_TASK_IDENTITY.state().write().insert(
        "bg-task".into(),
        BgTaskIdentity {
            agent_id: Some("bg-child".into()),
            ..Default::default()
        },
    );
    crate::kit::bg_task_live::init_agent_live_detail(
        "bg-task",
        "bg-child",
        "coder",
        Some("bg-run"),
    );
    tool::handle_tool_started(
        &mut state,
        &started("bg", Some("bg-child"), todos("pending")),
    );
    assert!(matches!(
        &BG_LIVE_DETAIL.state().read()["bg-task"].tool_cards[0].presentation,
        TuiToolPresentation::Generic
    ));

    tool::handle_replay_tool_started(
        &mut state,
        "replay",
        "TodoWrite",
        "one task",
        &todos("pending"),
    );
    let TuiRenderUnit::TuiToolCard(card) = state.committed.back().unwrap() else {
        panic!("expected replay tool");
    };
    assert!(matches!(&card.presentation, TuiToolPresentation::Generic));
}

#[test]
#[serial]
fn oversized_child_input_marks_unknown_then_small_success_restores_semantics() {
    let mut state = make_fold_test_state();
    let _restore = RestoreBackground::empty();
    state.record_todo_started("known".into(), &todos("pending"), INPUT_BYTES);
    assert!(state.complete_todo_if_current("known", false));
    state
        .current_turn
        .start_subagent("child".into(), "coder".into(), None);
    let oversized = json!({"todos": [{"content": "x".repeat(20 * 1024), "status": "completed"}]});
    assert!(json_value_bytes(&oversized).reserved < INPUT_BYTES);
    tool::handle_tool_started(
        &mut state,
        &started("too-large-for-child", Some("child"), oversized),
    );
    assert!(state.todo_baseline_unknown());
    assert!(!state.todo_call_inputs.contains_key("too-large-for-child"));

    tool::handle_tool_started(
        &mut state,
        &started("restore", Some("child"), todos("completed")),
    );
    assert!(matches!(
        &state.current_turn.subagents[0].child_turn.tool_cards[1].presentation,
        TuiToolPresentation::Generic
    ));
    // A preview can be evicted before completion; its comparison input still settles.
    state.current_turn.subagents[0].release_preview();
    tool::handle_tool_ended(
        &mut state,
        &crate::kit::stream_data::TuiToolEnded {
            tool_id: "restore".into(),
            output_summary: "saved".into(),
            is_error: false,
            agent_id: Some("child".into()),
        },
    );
    assert!(!state.todo_baseline_unknown());
    tool::handle_tool_started(
        &mut state,
        &started("after", Some("child"), todos("in_progress")),
    );
    let TuiToolPresentation::Todo(presentation) =
        &state.current_turn.subagents[0].child_turn.tool_cards[0].presentation
    else {
        panic!("complete success should restore a known comparison baseline");
    };
    assert!(!presentation.is_initial);
}
