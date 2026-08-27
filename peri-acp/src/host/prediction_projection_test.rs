use peri_acp_types::messages::{BaseMessage, ToolCallRequest};

use super::{project_prediction_history, PREDICTION_HISTORY_WINDOW};

fn call(id: &str) -> ToolCallRequest {
    ToolCallRequest::new(id, "Read", serde_json::json!({}))
}

fn assert_closed(messages: &[BaseMessage]) {
    for message in messages {
        if let BaseMessage::Tool { tool_call_id, .. } = message {
            assert!(
                messages.iter().any(|candidate| {
                    candidate
                        .tool_calls()
                        .iter()
                        .any(|call| call.id == *tool_call_id)
                }),
                "orphan tool result: {tool_call_id}"
            );
        }
    }
}

#[test]
fn single_call_crossing_window_soft_expands_complete_group() {
    let mut history = vec![
        BaseMessage::ai_with_tool_calls("calling", vec![call("one")]),
        BaseMessage::tool_result("one", "ok"),
    ];
    history.extend(
        (0..PREDICTION_HISTORY_WINDOW - 1).map(|index| BaseMessage::human(index.to_string())),
    );

    let projected = project_prediction_history(&history);

    assert_eq!(projected.len(), PREDICTION_HISTORY_WINDOW + 1);
    assert!(projected[0].has_tool_calls());
    assert_closed(&projected);
}

#[test]
fn parallel_calls_crossing_window_keep_all_results() {
    let mut history = vec![
        BaseMessage::ai_with_tool_calls("calling", vec![call("a"), call("b")]),
        BaseMessage::tool_result("a", "ok"),
        BaseMessage::tool_result("b", "ok"),
    ];
    history.extend(
        (0..PREDICTION_HISTORY_WINDOW - 2).map(|index| BaseMessage::human(index.to_string())),
    );

    let projected = project_prediction_history(&history);

    assert_eq!(projected.len(), PREDICTION_HISTORY_WINDOW + 1);
    assert_eq!(
        projected
            .iter()
            .filter(|message| matches!(message, BaseMessage::Tool { .. }))
            .count(),
        2
    );
    assert_closed(&projected);
}

#[test]
fn error_tool_result_is_kept_with_its_call() {
    let mut history = vec![
        BaseMessage::ai_with_tool_calls("calling", vec![call("failed")]),
        BaseMessage::tool_error("failed", "boom"),
    ];
    history.extend(
        (0..PREDICTION_HISTORY_WINDOW - 1).map(|index| BaseMessage::human(index.to_string())),
    );

    let projected = project_prediction_history(&history);

    assert!(projected
        .iter()
        .any(|message| matches!(message, BaseMessage::Tool { is_error: true, .. })));
    assert_closed(&projected);
}

#[test]
fn orphan_tool_result_at_boundary_is_dropped() {
    let mut history = vec![BaseMessage::tool_result("missing", "orphan")];
    history.extend(
        (0..PREDICTION_HISTORY_WINDOW - 1).map(|index| BaseMessage::human(index.to_string())),
    );

    let projected = project_prediction_history(&history);

    assert_eq!(projected.len(), PREDICTION_HISTORY_WINDOW - 1);
    assert!(!projected
        .iter()
        .any(|message| matches!(message, BaseMessage::Tool { .. })));
}

#[test]
fn duplicate_call_id_does_not_pair_result_with_later_declaration() {
    let mut history = vec![
        BaseMessage::ai_with_tool_calls("first", vec![call("duplicate")]),
        BaseMessage::tool_result("duplicate", "first result"),
        BaseMessage::ai_with_tool_calls("second", vec![call("duplicate")]),
    ];
    history.extend(
        (0..PREDICTION_HISTORY_WINDOW - 2).map(|index| BaseMessage::human(index.to_string())),
    );

    let projected = project_prediction_history(&history);

    assert!(matches!(projected[0], BaseMessage::Ai { .. }));
    assert!(matches!(projected[1], BaseMessage::Tool { .. }));
    assert_eq!(
        projected
            .iter()
            .filter(|message| matches!(message, BaseMessage::Ai { .. }))
            .count(),
        1
    );
    assert_closed(&projected);
}

#[test]
fn incomplete_parallel_group_is_dropped_as_a_whole() {
    let history = vec![
        BaseMessage::ai_with_tool_calls("calling", vec![call("a"), call("b")]),
        BaseMessage::tool_result("a", "ok"),
        BaseMessage::human("next"),
    ];

    let projected = project_prediction_history(&history);

    assert_eq!(projected.len(), 1);
    assert!(matches!(projected[0], BaseMessage::Human { .. }));
}
