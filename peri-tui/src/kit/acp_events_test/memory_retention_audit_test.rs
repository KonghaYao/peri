//! Regression evidence through the real bridge; synthetic provider events, not RSS.
use super::*;
use crate::kit::history_window::{HistoryWindow, HistoryWindowConfig, WindowIdentity};
use std::sync::Arc;

fn start(state: &mut BridgeState, id: &str) {
    dispatch_and_notify(
        state,
        &AcpEventData::SubagentStarted {
            agent_id: id.into(),
            agent_name: "coder".into(),
            is_background: false,
            parent_tool_call_id: None,
        },
    );
}
fn stop(state: &mut BridgeState, id: &str) {
    dispatch_and_notify(
        state,
        &AcpEventData::SubagentStopped {
            agent_id: id.into(),
            result: "cancelled".into(),
            is_error: true,
        },
    );
}
fn text(state: &mut BridgeState, id: &str, message: &str, payload: String) {
    dispatch_and_notify(
        state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: payload,
            message_id: Some(message.into()),
            agent_id: Some(id.into()),
        }),
    );
}
fn evidence(name: &str, value: serde_json::Value) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/memory-regression");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(name), serde_json::to_vec_pretty(&value).unwrap()).unwrap();
}

#[test]
#[serial]
fn memory_retention_regression_duplicate_stop_keeps_frozen_body() {
    let mut state = make_fold_test_state();
    start(&mut state, "once");
    text(&mut state, "once", "message", "final child body".into());
    stop(&mut state, "once");
    let before = state.current_turn.subagents[0].view_model();
    stop(&mut state, "once");
    assert_eq!(state.current_turn.subagents[0].view_model(), before);
    state.current_turn.mark_subagent_preview_omitted("once");
    let TuiRenderUnit::TuiSubAgentGroup(after) = state.current_turn.subagents[0].view_model()
    else {
        panic!("child group");
    };
    let TuiRenderUnit::TuiSubAgentGroup(before) = before else {
        unreachable!()
    };
    assert_eq!(after.view_models, before.view_models);
    assert!(after.preview_omitted);
    crate::kit::acp_events::push_view_models_for_reset();
}

#[test]
#[serial]
fn memory_retention_regression_long_parent_releases_actual_child_allocations() {
    let mut state = make_fold_test_state();
    let mut first_body = None;
    let mut first_fold = None;
    let mut samples = Vec::new();
    for index in 0..1000 {
        let id = format!("child-{index}");
        start(&mut state, &id);
        text(
            &mut state,
            &id,
            &format!("m-{index}"),
            "x".repeat(1024 * 1024),
        );
        stop(&mut state, &id);
        assert!(
            state
                .current_turn
                .subagents
                .iter()
                .all(|s| s.child_turn.raw_memory_bytes().reserved == 0)
        );
        if index == 0 {
            let TuiRenderUnit::TuiSubAgentGroup(group) =
                state.current_turn.subagents[0].view_model()
            else {
                panic!("first child must publish a subagent group");
            };
            first_body = group.view_models.iter().find_map(|unit| match unit {
                TuiRenderUnit::TuiAssistantBubble(body) => Some(Arc::downgrade(body)),
                _ => None,
            });
            assert!(group.preview_omitted);
            let key = FoldKey::SubAgent(group.instance_id.clone());
            FOLD_OVERRIDES
                .state()
                .write()
                .insert(key.clone(), FoldState::Expanded);
            first_fold = Some(key);
        }
        if index % 100 == 0 {
            samples.push(json!({"runs":index+1,
                "retained_owner_bytes":state.memory_snapshot().unique_reserved_bytes,
                "groups":state.current_turn.subagents.len()}));
        }
    }
    assert!(!state.current_turn.has_running_subagent());
    assert!(state.current_turn.subagents.len() <= 256);
    assert!(
        first_body.expect("first preview").upgrade().is_none(),
        "old raw, VM and publication owners must release the allocation"
    );
    assert!(
        !FOLD_OVERRIDES
            .state()
            .read()
            .contains_key(&first_fold.unwrap()),
        "expired preview must release its fold directory key"
    );
    assert!(state.memory_snapshot().unique_reserved_bytes < 8 * 1024 * 1024);
    evidence(
        "long-parent.json",
        json!({"runs":1000,"parent_turn_done":false,"samples":samples}),
    );
    crate::kit::acp_events::push_view_models_for_reset();
}

#[test]
#[serial]
fn memory_retention_regression_committed_preview_directory_is_bounded() {
    let mut state = make_fold_test_state();
    state.active_session_id = "memory-regression-window".into();
    state.window = HistoryWindow::new(HistoryWindowConfig::enabled());
    state.window.begin_window(WindowIdentity::new(
        "memory-regression-window",
        "gen",
        1,
        "snapshot",
    ));
    for index in 0..1000 {
        let id = format!("window-child-{index}");
        start(&mut state, &id);
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ReasoningChunk(TuiReasoningChunk {
                text: "思考".repeat(32768),
                message_id: Some(format!("r-{index}")),
                agent_id: Some(id.clone()),
            }),
        );
        stop(&mut state, &id);
        dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    }
    let groups = state
        .committed
        .iter()
        .filter(|u| matches!(u, TuiRenderUnit::TuiSubAgentGroup(_)))
        .count();
    assert!(groups <= 256);
    assert_eq!(
        state.window.budget().unattributed_live_bytes,
        0,
        "preview expiry must not borrow or return canonical admission credit"
    );
    assert!(state.memory_snapshot().committed_body_bytes < 2 * 1024 * 1024);
    evidence(
        "committed.json",
        json!({"runs":1000,"groups":groups,
        "retained_owner_bytes":state.memory_snapshot().unique_reserved_bytes}),
    );
    crate::kit::acp_events::push_view_models_for_reset();
}

#[test]
#[serial]
fn memory_retention_regression_unknown_child_never_enters_root() {
    let mut state = make_fold_test_state();
    text(
        &mut state,
        "evicted-child",
        "old-message",
        "late child text".into(),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ReasoningChunk(TuiReasoningChunk {
            text: "late reasoning".into(),
            message_id: Some("old-message".into()),
            agent_id: Some("evicted-child".into()),
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "old-tool".into(),
            tool_name: "Bash".into(),
            input_summary: "late input".into(),
            raw_input: json!({"command":"echo old"}),
            agent_id: Some("evicted-child".into()),
            origin_message_id: None,
            origin_readable: None,
        }),
    );
    assert!(state.current_turn.text.is_empty());
    assert!(state.current_turn.reasoning.is_empty());
    assert!(state.current_turn.tool_cards.is_empty());
    crate::kit::acp_events::push_view_models_for_reset();
}

#[test]
#[serial]
fn memory_retention_regression_many_small_message_ids_are_bounded() {
    let mut state = make_fold_test_state();
    start(&mut state, "many-members");
    for index in 0..5000 {
        text(
            &mut state,
            "many-members",
            &format!("message-{index:016}"),
            "a".into(),
        );
    }
    let child = &state.current_turn.subagents[0];
    assert!(child.preview_omitted);
    assert!(child.preview_reserved() <= 256 * 1024);
    stop(&mut state, "many-members");
    assert_eq!(
        state.current_turn.subagents[0]
            .child_turn
            .raw_memory_bytes()
            .reserved,
        0
    );
    crate::kit::acp_events::push_view_models_for_reset();
}

#[test]
#[serial]
fn memory_retention_regression_cancelled_todo_inputs_are_bounded() {
    let mut state = make_fold_test_state();
    let input = json!({"todos":[{"content":"todo ".repeat(512),"status":"pending","activeForm":"working"}]});
    for index in 0..1000 {
        let id = format!("todo-child-{index}");
        start(&mut state, &id);
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolStarted(TuiToolStarted {
                tool_id: format!("todo-{index}"),
                tool_name: "TodoWrite".into(),
                input_summary: "todo".into(),
                raw_input: input.clone(),
                agent_id: Some(id.clone()),
                origin_message_id: None,
                origin_readable: None,
            }),
        );
        stop(&mut state, &id); // No ToolEnded arrives after cancellation.
    }
    assert!(state.todo_call_inputs.len() <= 128);
    assert!(
        state
            .todo_call_inputs
            .values()
            .map(|(_, v)| crate::kit::retained_bytes::json_value_bytes(v).reserved)
            .sum::<usize>()
            <= 1024 * 1024
    );
    crate::kit::acp_events::push_view_models_for_reset();
}
