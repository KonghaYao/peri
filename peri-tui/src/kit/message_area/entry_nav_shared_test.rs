use super::*;
use crate::kit::tui_render_unit::{EntryStatus, TuiAssistantBubble, TuiReasoningBlock};
use std::sync::Arc;

#[test]
fn fold_override_copies_only_changed_payload_and_preserves_published_snapshot() {
    let mut unit = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            text: "body".into(),
            reasoning: Some(TuiReasoningBlock {
                text: "reasoning".into(),
                fold: FoldState::Collapsed,
                status: EntryStatus::Completed,
                is_running: false,
                started_at: None,
                duration_ms: None,
            }),
            message_id: Some("message".into()),
            started_at: None,
            duration_ms: None,
            content_hash: 0,
        }
        .into(),
    );
    let published = unit.clone();
    apply_fold_override(&mut unit, FoldState::Expanded);
    let TuiRenderUnit::TuiAssistantBubble(original) = &published else {
        panic!("expected original");
    };
    let TuiRenderUnit::TuiAssistantBubble(current) = &unit else {
        panic!("expected current");
    };
    assert!(!Arc::ptr_eq(original, current));
    assert_eq!(
        original.reasoning.as_ref().unwrap().fold,
        FoldState::Collapsed
    );
    assert_eq!(
        current.reasoning.as_ref().unwrap().fold,
        FoldState::Expanded
    );
    let unchanged = unit.clone();
    apply_fold_override(&mut unit, FoldState::Expanded);
    let TuiRenderUnit::TuiAssistantBubble(previous) = &unchanged else {
        panic!("expected previous");
    };
    let TuiRenderUnit::TuiAssistantBubble(current) = &unit else {
        panic!("expected current");
    };
    assert!(Arc::ptr_eq(previous, current));
}
