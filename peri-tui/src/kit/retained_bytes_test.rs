use std::sync::Arc;

use super::SeenAllocations;
use super::TuiAssistantBubble;
use super::TuiRenderUnit;

use crate::kit::tui_render_unit::TuiUserBubble;
use ratatui_kit::ratatui::text::Line;

fn bubble(text: &str) -> Arc<TuiAssistantBubble> {
    Arc::new(TuiAssistantBubble {
        text: text.to_string(),
        reasoning: None,
        message_id: Some("m1".into()),
        started_at: None,
        duration_ms: None,
        content_hash: 0,
    })
}

#[test]
fn account_arc_with_only_measures_unseen_arcs() {
    let first = Arc::new(vec![1u8; 32]);
    let second = Arc::new(vec![2u8; 32]);
    let mut seen = SeenAllocations::default();
    let mut calls = 0;
    assert_eq!(
        seen.account_arc_with(&first, || {
            calls += 1;
            64
        }),
        64
    );
    assert_eq!(calls, 1);
    assert_eq!(
        seen.account_arc_with(&first, || {
            calls += 1;
            64
        }),
        0
    );
    assert_eq!(calls, 1);
    assert_eq!(
        seen.account_arc_with(&second, || {
            calls += 1;
            32
        }),
        32
    );
    assert_eq!(calls, 2);
}

#[test]
fn shared_arc_bubble_is_counted_once_across_units() {
    let shared = bubble("same allocation");
    let first = TuiRenderUnit::TuiAssistantBubble(Arc::clone(&shared));
    let second = TuiRenderUnit::TuiAssistantBubble(Arc::clone(&shared));
    let mut seen = SeenAllocations::default();
    let bytes = super::retained_unit_bytes(&first, &mut seen)
        + super::retained_unit_bytes(&second, &mut seen);
    assert!(bytes > 0);
    assert_eq!(seen.allocations(), 1, "same Arc allocation counted once");
    let separate = TuiRenderUnit::TuiAssistantBubble(bubble("same allocation"));
    let before = bytes;
    let combined = before + super::retained_unit_bytes(&separate, &mut SeenAllocations::default());
    assert!(combined > before);
}

#[test]
fn shared_line_allocation_is_counted_once() {
    let lines = Arc::new(vec![Line::from("hello"), Line::from("world")]);
    let mut seen = SeenAllocations::default();
    let first = super::retained_arc_lines_bytes(&lines, &mut seen);
    let second = super::retained_arc_lines_bytes(&lines.clone(), &mut seen);
    assert!(first > 0);
    assert_eq!(second, 0);
    let other = Arc::new(vec![Line::from("hello"), Line::from("world")]);
    assert!(super::retained_arc_lines_bytes(&other, &mut seen) > 0);
}

#[test]
fn body_and_reserved_helpers_do_not_use_rss() {
    let user = TuiUserBubble::new("text".into());
    let unit = TuiRenderUnit::TuiUserBubble(user);
    let mut seen = SeenAllocations::default();
    let bytes = super::retained_unit_bytes(&unit, &mut seen);
    assert!(bytes >= std::mem::size_of::<TuiUserBubble>());
    assert_eq!(
        seen.allocations(),
        0,
        "plain values are not Arc allocations"
    );
}
