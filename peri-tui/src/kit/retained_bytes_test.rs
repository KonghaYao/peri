use std::sync::Arc;

use super::*;
use crate::kit::tui_render_unit::{FoldState, TuiToolCard, TuiUserBubble};
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
fn shared_arc_bubble_is_counted_once_across_units() {
    let shared = bubble("same allocation");
    let first = TuiRenderUnit::TuiAssistantBubble(Arc::clone(&shared));
    let second = TuiRenderUnit::TuiAssistantBubble(Arc::clone(&shared));
    let mut seen = SeenAllocations::default();
    let bytes = retained_unit_bytes(&first, &mut seen) + retained_unit_bytes(&second, &mut seen);
    assert!(bytes > 0);
    assert_eq!(seen.allocations(), 1, "same Arc allocation counted once");
    let separate = TuiRenderUnit::TuiAssistantBubble(bubble("same allocation"));
    let before = bytes;
    let combined = before + retained_unit_bytes(&separate, &mut SeenAllocations::default());
    assert!(combined > before);
}

#[test]
fn group_deep_copy_is_identified() {
    let shared = bubble("hidden child");
    let mut group = TuiCollapsedGroup {
        title: "group".into(),
        count: 1,
        failed_count: 0,
        view_models: vec![TuiRenderUnit::TuiAssistantBubble(Arc::clone(&shared))],
        fold: FoldState::Collapsed,
        content_hash: 0,
    };
    let mut seen = SeenAllocations::default();
    let shared_bytes = retained_collapsed_group_bytes(&group, &mut seen);
    let mut seen2 = SeenAllocations::default();
    assert_eq!(
        shared_bytes,
        retained_collapsed_group_bytes(&group, &mut seen2)
    );
    assert!(group_deep_copy_bytes(&group) > 0);

    group
        .view_models
        .push(TuiRenderUnit::TuiToolCard(TuiToolCard {
            tool_id: "t".into(),
            tool_name: "Read".into(),
            input_summary: "x".into(),
            output_summary: "y".into(),
            is_error: false,
            is_running: false,
            running_duration_ms: None,
            completed_duration_ms: None,
            diff: None,
            presentation: Default::default(),
            fold: FoldState::Collapsed,
            user_modified: false,
            content_hash: 0,
            tool_calls_count: 0,
        }));
    let mut seen3 = SeenAllocations::default();
    assert!(retained_collapsed_group_bytes(&group, &mut seen3) > shared_bytes);
}

#[test]
fn shared_line_allocation_is_counted_once() {
    let lines = Arc::new(vec![Line::from("hello"), Line::from("world")]);
    let mut seen = SeenAllocations::default();
    let first = retained_arc_lines_bytes(&lines, &mut seen);
    let second = retained_arc_lines_bytes(&lines.clone(), &mut seen);
    assert!(first > 0);
    assert_eq!(second, 0);
    let other = Arc::new(vec![Line::from("hello"), Line::from("world")]);
    assert!(retained_arc_lines_bytes(&other, &mut seen) > 0);
}

#[test]
fn body_and_reserved_helpers_do_not_use_rss() {
    let user = TuiUserBubble::new("text".into());
    let unit = TuiRenderUnit::TuiUserBubble(user);
    let mut seen = SeenAllocations::default();
    let bytes = retained_unit_bytes(&unit, &mut seen);
    assert!(bytes >= std::mem::size_of::<TuiUserBubble>());
    assert_eq!(
        seen.allocations(),
        0,
        "plain values are not Arc allocations"
    );
}
