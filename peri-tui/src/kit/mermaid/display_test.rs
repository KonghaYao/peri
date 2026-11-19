use std::sync::Arc;
use std::time::Instant;

use ratatui_kit::prelude::Palette;
use ratatui_kit::ratatui::style::Color;
use ratatui_kit::ratatui::text::Line;

use crate::kit::markdown::{MarkdownSegment, MermaidSegment, parse_markdown};
use crate::kit::mermaid::PreviewScope;
use crate::kit::tui_render_unit::{TuiAssistantBubble, TuiRenderUnit};

use super::display_lines;
use super::vm_might_have_mermaid;

fn segment(markdown: &str) -> MermaidSegment {
    parse_markdown(markdown, 60, Palette::default(), Color::Reset)
        .into_iter()
        .find_map(|segment| match segment {
            MarkdownSegment::Mermaid(segment) => Some(segment),
            _ => None,
        })
        .expect("fixture contains a Mermaid fence")
}

fn line_text(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn text(lines: &[Line<'static>]) -> String {
    lines.iter().map(line_text).collect::<Vec<_>>().join("\n")
}

#[test]
#[serial_test::serial]
fn source_is_visible_until_the_first_frame_publishes() {
    crate::kit::mermaid::reset_for_test();
    let segment = segment("```mermaid\ngraph TD\nA-->B\n```");
    let scope = PreviewScope::for_bubble("session", Some("msg-1"), Some(Instant::now()), 7);

    let before = text(&display_lines(&segment, &scope, 60, false, Color::Reset));
    assert!(before.contains("graph TD"), "source missing: {before}");
    assert!(before.contains("A-->B"), "source missing: {before}");

    crate::kit::mermaid::drain_preview_jobs_now();
    let after = text(&display_lines(&segment, &scope, 60, false, Color::Reset));
    assert!(
        !after.contains("graph TD"),
        "final frame should replace source: {after}"
    );
    assert!(
        after.contains('┌') || after.contains('╭'),
        "no box drawing: {after}"
    );
}

#[test]
#[serial_test::serial]
fn unconfirmed_source_stays_below_the_last_frame() {
    crate::kit::mermaid::reset_for_test();
    let scope = PreviewScope::for_bubble("session", Some("msg-2"), Some(Instant::now()), 8);
    let closed = segment("```mermaid\ngraph TD\nA-->B\n```");
    display_lines(&closed, &scope, 60, false, Color::Reset);
    crate::kit::mermaid::drain_preview_jobs_now();

    let open = segment("```mermaid\ngraph TD\nA-->B\nB--");
    let rendered = display_lines(&open, &scope, 60, false, Color::Reset);
    let rendered = text(&rendered);
    assert!(rendered.contains("B--"), "source tail missing: {rendered}");
    assert!(
        !rendered.contains("graph TD"),
        "confirmed source should be replaced by the frame: {rendered}"
    );
}

#[test]
#[serial_test::serial]
fn frozen_unclosed_fence_marks_the_preview_unclosed() {
    crate::kit::mermaid::reset_for_test();
    crate::i18n::init(Some("en"));
    let scope = PreviewScope::for_bubble("session", Some("msg-3"), None, 9);
    let open = segment("```mermaid\ngraph TD\nA-->B\nC--");
    display_lines(&open, &scope, 60, true, Color::Reset);
    crate::kit::mermaid::drain_preview_jobs_now();
    let rendered = text(&display_lines(&open, &scope, 60, true, Color::Reset));
    assert!(
        rendered.to_lowercase().contains("unclosed"),
        "missing unclosed notice: {rendered}"
    );
    assert!(rendered.contains("C--"), "source tail missing: {rendered}");
}

#[test]
fn vm_detection_only_matches_mermaid_fences() {
    let vm = |text: &str| {
        TuiRenderUnit::TuiAssistantBubble(Arc::new(TuiAssistantBubble {
            text: text.to_string(),
            reasoning: None,
            message_id: None,
            started_at: None,
            duration_ms: None,
            content_hash: 1,
        }))
    };
    assert!(vm_might_have_mermaid(&vm(
        "```mermaid\ngraph TD\nA-->B\n```"
    )));
    assert!(vm_might_have_mermaid(&vm(
        "~~~Mermaid\nsequenceDiagram\n~~~"
    )));
    assert!(!vm_might_have_mermaid(&vm("```rust\nmermaid\n```")));
    assert!(vm_might_have_mermaid(&vm(
        "> ```mermaid\n> graph TD\n> A-->B\n> ```"
    )));
}

#[test]
#[serial_test::serial]
fn tab_selection_highlights_the_action_and_invalidates_the_message() {
    crate::kit::mermaid::reset_for_test();
    let segment = segment("```mermaid\ngraph TD\nA-->B\n```");
    let scope = PreviewScope::for_bubble("session", Some("msg-select"), None, 41);
    let key = scope.key(segment.fence.open_start);
    let vm = TuiRenderUnit::TuiAssistantBubble(Arc::new(TuiAssistantBubble {
        text: "```mermaid\ngraph TD\nA-->B\n```".to_string(),
        reasoning: None,
        message_id: Some("msg-select".to_string()),
        started_at: None,
        duration_ms: None,
        content_hash: 41,
    }));
    let before_revision = crate::kit::mermaid::message_revision_for_vm(&vm)
        .expect("message contains a Mermaid fence");

    crate::kit::mermaid::select_action(Some(key.clone()));
    let rendered = text(&display_lines(&segment, &scope, 60, false, Color::Reset));
    assert!(
        rendered.contains("[View diagram]") || rendered.contains("[查看图]"),
        "Tab 选中后入口应有可见标记: {rendered}"
    );
    let after_revision = crate::kit::mermaid::message_revision_for_vm(&vm)
        .expect("message contains a Mermaid fence");
    assert_ne!(
        before_revision, after_revision,
        "选中态变化必须让所属消息重新渲染"
    );

    crate::kit::mermaid::select_action(None);
    let cleared = text(&display_lines(&segment, &scope, 60, false, Color::Reset));
    assert!(
        !cleared.contains("[View diagram]") && !cleared.contains("[查看图]"),
        "清除选中后不能保留高亮: {cleared}"
    );
}
