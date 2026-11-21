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

    let before = text(&display_lines(&segment, &scope, 60, false, Color::Reset).lines);
    assert!(before.contains("graph TD"), "source missing: {before}");
    assert!(before.contains("A-->B"), "source missing: {before}");

    crate::kit::mermaid::drain_preview_jobs_now();
    let after = text(&display_lines(&segment, &scope, 60, false, Color::Reset).lines);
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
    let rendered = text(&rendered.lines);
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
    let rendered = text(&display_lines(&open, &scope, 60, true, Color::Reset).lines);
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
    let rendered = text(&display_lines(&segment, &scope, 60, false, Color::Reset).lines);
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
    let cleared = text(&display_lines(&segment, &scope, 60, false, Color::Reset).lines);
    assert!(
        !cleared.contains("[View diagram]") && !cleared.contains("[查看图]"),
        "清除选中后不能保留高亮: {cleared}"
    );
}

const WIDE_SEQUENCE_SOURCE: &str = "sequenceDiagram\n  participant Client\n  participant Server\n  participant Loom\n  participant Rivet\n  Client->>Server: POST /v1/events with a fairly long request name\n  Server->>Loom: send(action), key=agent/<logicalId>\n  Loom->>Rivet: locate actor and forward";

fn wide_markdown() -> String {
    format!("```mermaid\n{WIDE_SEQUENCE_SOURCE}\n```")
}

fn published(segment: &MermaidSegment, scope: &PreviewScope, width: usize) -> super::FenceRender {
    let _ = display_lines(segment, scope, width, false, Color::Reset);
    crate::kit::mermaid::drain_preview_jobs_now();
    display_lines(segment, scope, width, false, Color::Reset)
}

fn bar_text(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

#[test]
#[serial_test::serial]
fn wide_sequence_diagram_clips_to_the_viewport_and_shows_a_bar() {
    crate::kit::mermaid::reset_for_test();
    crate::i18n::init(Some("en"));
    let segment = segment(&wide_markdown());
    let scope = PreviewScope::for_bubble("session", Some("msg-wide"), None, 71);
    let rendered = published(&segment, &scope, 60);

    let view = rendered.h_view.expect("60 列放不下官方 sequence 图");
    assert_eq!(view.viewport, 60);
    assert!(view.max_offset > 0);
    assert!(
        rendered.lines.iter().all(|line| line.width() <= 60),
        "帧行不得越过正文列宽（不折行、不溢出）: {:#?}",
        rendered
            .lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
    );
    let bar_line = view.bar_line.expect("60 列足够绘制 ◄ 轨道 ►");
    let bar = bar_text(&rendered.lines[bar_line]);
    assert!(
        bar.contains('◄') && bar.contains('►'),
        "缺少横向条: {bar:?}"
    );
    assert_eq!(rendered.lines[bar_line].width(), 60);
    assert_eq!(view.rows, bar_line + 1, "帧行数包含条行，不含 tail/动作行");
}

#[test]
#[serial_test::serial]
fn normal_width_diagram_keeps_todays_layout_without_a_bar() {
    crate::kit::mermaid::reset_for_test();
    let segment = segment("```mermaid\ngraph TD\nA-->B\n```");
    let scope = PreviewScope::for_bubble("session", Some("msg-normal"), None, 72);
    let rendered = published(&segment, &scope, 80);

    assert!(rendered.h_view.is_none(), "正常宽度图不应出现横向视口");
    let body = text(&rendered.lines);
    assert!(
        !body.contains('◄') && !body.contains('►'),
        "无多余滚动条: {body}"
    );
    assert!(body.contains('┌') || body.contains('╭'), "帧仍按原样渲染");
}

#[test]
#[serial_test::serial]
fn horizontal_offset_slices_the_frame_and_survives_rerender() {
    crate::kit::mermaid::reset_for_test();
    let segment = segment(&wide_markdown());
    let scope = PreviewScope::for_bubble("session", Some("msg-scroll"), None, 73);
    let key = scope.key(segment.fence.open_start);
    let before = published(&segment, &scope, 60);
    let view = before.h_view.expect("wide frame");
    let first_before = line_text(&before.lines[0]);

    assert!(crate::kit::mermaid::scroll_h(
        &key,
        40,
        view.viewport,
        view.viewport + view.max_offset
    ));
    let after = display_lines(&segment, &scope, 60, false, Color::Reset);
    let first_after = line_text(&after.lines[0]);
    assert_ne!(first_before, first_after, "偏移后首行应显示更靠右的列");
    assert!(after.lines.iter().all(|line| line.width() <= 60));

    // 再次渲染同一身份仍保留列位置（不因重绘回到 0）。
    let again = display_lines(&segment, &scope, 60, false, Color::Reset);
    assert_eq!(line_text(&again.lines[0]), first_after);
}

#[test]
#[serial_test::serial]
fn fitting_frame_clears_a_stale_horizontal_offset() {
    crate::kit::mermaid::reset_for_test();
    let segment = segment(&wide_markdown());
    let scope = PreviewScope::for_bubble("session", Some("msg-fit-clear"), None, 75);
    let key = scope.key(segment.fence.open_start);
    let frame = |columns: usize| crate::kit::mermaid::preview::PreviewContent::Frame {
        lines: Arc::new(vec![
            (0..columns)
                .map(|i| char::from(b'a' + (i % 26) as u8))
                .collect(),
        ]),
        tail: String::new(),
        layout_bytes: 0,
    };
    let render = |columns: usize| {
        super::render_content(
            &segment,
            &key,
            frame(columns),
            None,
            60,
            Color::Reset,
            false,
        )
    };

    let left = line_text(&render(120).lines[0]);
    assert!(crate::kit::mermaid::scroll_h(&key, 40, 60, 120));
    assert_ne!(
        line_text(&render(120).lines[0]),
        left,
        "偏移后应显示更靠右的列"
    );

    // 同一围栏身份出现不溢出帧（流式内容变窄）：旧列必须被清掉。
    let fitted = render(40);
    assert!(fitted.h_view.is_none());
    assert_eq!(
        line_text(&render(120).lines[0]),
        left,
        "帧变宽后应从最左列重新开始"
    );
}

#[test]
#[serial_test::serial]
fn resize_clamps_the_offset_to_the_new_maximum() {
    crate::kit::mermaid::reset_for_test();
    let segment = segment(&wide_markdown());
    let scope = PreviewScope::for_bubble("session", Some("msg-resize"), None, 74);
    let key = scope.key(segment.fence.open_start);
    let rendered = published(&segment, &scope, 60);
    let view = rendered.h_view.expect("wide frame");
    let content = view.viewport + view.max_offset;

    assert!(crate::kit::mermaid::scroll_h(
        &key,
        i32::try_from(view.max_offset).unwrap_or(i32::MAX),
        view.viewport,
        content
    ));
    // 窗口变宽：最大值变小，旧偏移被 clamp 到新的右端。
    let wider = display_lines(&segment, &scope, 100, false, Color::Reset);
    let wide_view = wider.h_view.expect("100 列仍放不下");
    assert_eq!(wide_view.viewport, 100);
    let expected = crate::kit::mermaid::frame::render_source(WIDE_SEQUENCE_SOURCE, 100)
        .expect("sequence source renders");
    let expected_first = peri_tui_components::text::slice_by_width(
        expected
            .lines()
            .first()
            .map(String::as_str)
            .unwrap_or_default(),
        content.saturating_sub(100),
        100,
    );
    assert_eq!(
        line_text(&wider.lines[0]),
        expected_first,
        "clamp 后应停在新的最大偏移处，而不是旧的越界值"
    );
}
