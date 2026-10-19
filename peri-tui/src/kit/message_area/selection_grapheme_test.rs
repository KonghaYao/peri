//! [F7] 字素簇列宽映射与高亮边界。

use super::*;
use ratatui_kit::ratatui::style::{Color, Style};
use ratatui_kit::ratatui::text::Span;

fn make_line(s: &str) -> Line<'static> {
    Line::from(vec![Span::raw(s.to_string())])
}

const TEST_BG: Color = Color::Rgb(1, 2, 3);

fn test_style() -> Style {
    Style::default().bg(TEST_BG)
}

/// 提取 line 中带 `bg` 背景的 span 内容拼接，用于断言"实际被高亮的字符"。
fn highlighted_text(line: &Line<'_>, bg: Color) -> String {
    line.spans
        .iter()
        .filter(|s| s.style.bg == Some(bg))
        .map(|s| s.content.as_ref())
        .collect::<Vec<&str>>()
        .concat()
}

// ── [F7] 字素簇列宽映射 / 高亮不拆字素 ──────────────────────────────────

const FAMILY: &str = "👨‍👩‍👧‍👦";

#[test]
fn test_visual_column_mapping_uses_grapheme_width() {
    let text = format!("{FAMILY}abcdef");
    let family_bytes = FAMILY.len();
    assert_eq!(text_selection::visual_col_to_byte_offset(&text, 0), 0);
    assert_eq!(
        text_selection::visual_col_to_byte_offset(&text, 1),
        family_bytes,
        "双宽字素右半 → 整个字素之后（含）"
    );
    assert_eq!(
        text_selection::visual_col_to_byte_offset(&text, 2),
        family_bytes
    );
    assert_eq!(
        text_selection::visual_col_to_byte_offset(&text, 5),
        family_bytes + 3
    );
}

#[test]
fn test_extract_after_zwj_grapheme_selects_ascii() {
    let lines = vec![make_line(&format!("{FAMILY}abcdef"))];
    let slots = vec![Arc::new(lines.clone())];
    let offsets = vec![0usize];
    let (_, wrap_map) = build_wrap_map(&lines, 20);
    assert_eq!(
        extract_visual_range(&slots, &offsets, &wrap_map, (0, 2), (0, 5), 20, None, None)
            .as_deref(),
        Some("abc"),
        "family emoji 占 2 列，视觉列 2..5 对应 abc"
    );
}

fn render_symbols(line: &Line<'static>, width: u16) -> Vec<String> {
    use ratatui_kit::ratatui::buffer::Buffer;
    use ratatui_kit::ratatui::layout::Rect;
    use ratatui_kit::ratatui::widgets::Widget;
    let area = Rect::new(0, 0, width, 1);
    let mut buffer = Buffer::empty(area);
    line.clone().render(area, &mut buffer);
    (0..width)
        .map(|x| buffer.cell((x, 0)).unwrap().symbol().to_string())
        .collect()
}

#[test]
fn test_highlight_does_not_split_zwj_grapheme_or_change_layout() {
    let line = make_line(&format!("{FAMILY}abcdef"));
    let entry = WrappedLineInfo {
        slot_index: 0,
        logical_idx: 0,
        visual_start: 0,
        visual_end: 1,
    };
    let highlighted = highlight_line_in_selection(&line, &entry, 0, 0, 2, 5, 20, test_style());

    assert_eq!(render_symbols(&line, 10), render_symbols(&highlighted, 10));
    assert_eq!(highlighted_text(&highlighted, TEST_BG), "abc");
    let full: String = highlighted
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<&str>>()
        .concat();
    assert_eq!(full, format!("{FAMILY}abcdef"));
}

#[test]
fn test_highlight_combined_marks_and_flags_keep_grapheme_boundaries() {
    // 组合音标（e + U+0301）、旗帜（两区域指示符组成一个字素）与肤色
    // 修饰符（emoji modifier 与基字符同属一个字素）。
    for text in ["e\u{301}abc", "🇨🇳abc", "👍🏽abc"] {
        let line = make_line(text);
        let entry = WrappedLineInfo {
            slot_index: 0,
            logical_idx: 0,
            visual_start: 0,
            visual_end: 1,
        };
        let highlighted = highlight_line_in_selection(&line, &entry, 0, 0, 1, 4, 20, test_style());
        assert_eq!(
            render_symbols(&line, 8),
            render_symbols(&highlighted, 8),
            "{text}"
        );
        let full: String = highlighted
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<&str>>()
            .concat();
        assert_eq!(full, text);
    }
}
