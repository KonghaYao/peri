use super::*;

#[test]
fn test_message_content_empty_text() {
    assert!(is_message_content_empty(MessageContentShape::Text("")));
}

#[test]
fn test_message_content_whitespace_text_is_not_empty() {
    assert!(!is_message_content_empty(MessageContentShape::Text(" \n")));
}

#[test]
fn test_message_content_block_shapes() {
    assert!(is_message_content_empty(MessageContentShape::Blocks(0)));
    assert!(!is_message_content_empty(MessageContentShape::Blocks(1)));
}

#[test]
fn test_message_content_raw_shapes() {
    assert!(is_message_content_empty(MessageContentShape::Raw(0)));
    assert!(!is_message_content_empty(MessageContentShape::Raw(1)));
}

#[test]
fn test_select_compact_action_below_threshold() {
    assert_eq!(
        select_compact_action(0.74, 0.75, false),
        CompactAction::Skip
    );
}

#[test]
fn test_select_compact_action_at_threshold() {
    assert_eq!(
        select_compact_action(0.75, 0.75, false),
        CompactAction::Micro
    );
}

#[test]
fn test_select_compact_action_above_threshold_with_smart_enabled() {
    assert_eq!(
        select_compact_action(0.80, 0.75, true),
        CompactAction::Smart
    );
}

#[test]
fn test_select_compact_action_preserves_non_finite_comparison_behavior() {
    assert_eq!(
        select_compact_action(f64::NAN, 0.75, false),
        CompactAction::Skip
    );
    assert_eq!(
        select_compact_action(f64::INFINITY, 0.75, false),
        CompactAction::Micro
    );
    assert_eq!(
        select_compact_action(f64::NEG_INFINITY, 0.75, false),
        CompactAction::Skip
    );

    assert_eq!(
        select_compact_action(0.75, f64::NAN, false),
        CompactAction::Skip
    );
    assert_eq!(
        select_compact_action(0.75, f64::NAN, true),
        CompactAction::Skip
    );
    assert_eq!(
        select_compact_action(0.75, f64::INFINITY, false),
        CompactAction::Skip
    );
    assert_eq!(
        select_compact_action(0.75, f64::INFINITY, true),
        CompactAction::Skip
    );
    assert_eq!(
        select_compact_action(0.75, f64::NEG_INFINITY, false),
        CompactAction::Micro
    );
    assert_eq!(
        select_compact_action(0.75, f64::NEG_INFINITY, true),
        CompactAction::Smart
    );
}
