use super::*;

#[test]
fn finish_is_shared_and_idempotent() {
    let mut card = ToolCardAccumulator::new("tool".into(), "Shell".into(), "pwd".into());
    assert!(card.finish("original".into(), false));
    let duration = card.completed_duration_ms;
    assert!(!card.finish("duplicate".into(), true));
    assert_eq!(card.output_summary.as_deref(), Some("original"));
    assert!(!card.is_error);
    assert_eq!(card.completed_duration_ms, duration);
}

#[test]
fn current_turn_duplicate_end_preserves_result_and_duration() {
    let mut turn = CurrentTurn::new();
    turn.start_tool(ToolCardAccumulator::new(
        "tool".into(),
        "Shell".into(),
        "pwd".into(),
    ));
    assert!(turn.end_tool("tool", "original".into(), false));
    let duration = turn.tool_cards[0].completed_duration_ms;
    assert!(!turn.end_tool("tool", "duplicate".into(), true));
    assert_eq!(
        turn.tool_cards[0].output_summary.as_deref(),
        Some("original")
    );
    assert_eq!(turn.tool_cards[0].completed_duration_ms, duration);
    assert!(!turn.tool_cards[0].is_error);
}

#[test]
fn empty_stopped_turn_keeps_late_tool_auditable_without_reviving() {
    let mut turn = CurrentTurn::new();
    turn.deactivate();
    turn.start_tool(ToolCardAccumulator::new(
        "tool".into(),
        "Shell".into(),
        "null".into(),
    ));
    turn.start_tool(ToolCardAccumulator::with_input(
        "tool".into(),
        "Shell".into(),
        "pwd".into(),
        serde_json::json!({"command": "pwd"}),
        None,
    ));
    assert!(!turn.active);
    assert_eq!(turn.tool_cards.len(), 1);
    assert_eq!(turn.tool_cards[0].input_summary, "pwd");
    assert!(matches!(&turn.view_models()[0], TuiRenderUnit::TuiToolCard(card) if !card.is_running));
}
