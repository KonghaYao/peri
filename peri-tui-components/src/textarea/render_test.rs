use super::{TextAreaRenderOptions, TextAreaStyles};
use ratatui::style::{Color, Style};

fn cursor_style() -> Style {
    Style::default().fg(Color::Blue).bg(Color::Yellow)
}

fn sel_style() -> Style {
    Style::default().bg(Color::DarkGray)
}

fn ph_style() -> Style {
    Style::default().fg(Color::Gray)
}

/// 辅助：构建 N 行 "line0".."line{N-1}" 的文本，返回 (text, char_idx_of_line_col)。
fn build_lines_text(n: usize) -> (String, Vec<usize>) {
    let mut line_starts: Vec<usize> = Vec::with_capacity(n);
    let mut text = String::new();
    let mut offset = 0usize;
    for i in 0..n {
        line_starts.push(offset);
        let s = format!("line{i}");
        text.push_str(&s);
        offset += s.chars().count();
        if i + 1 < n {
            text.push('\n');
            offset += 1;
        }
    }
    (text, line_starts)
}

/// 50 行文本，光标在第 25 行——验证渲染只返回 10 行且包含光标行。
#[test]
fn test_viewport_50_lines_cursor_at_25_returns_10_lines() {
    let (text, line_starts) = build_lines_text(50);

    // 光标在 line25 的第 2 个字符（'i' 位置）
    let cursor = line_starts[25] + 2;
    let viewport_height = 10;

    let result = super::render_multiline_with_cursor(TextAreaRenderOptions {
        text: &text,
        cursor,
        selection_range: None,
        placeholder: None,
        styles: TextAreaStyles {
            cursor: cursor_style(),
            selection: sel_style(),
            placeholder: ph_style(),
            base: Style::default(),
        },
        max_width: 80,
        viewport_height,
        show_cursor: true,
    });

    assert_eq!(
        result.len(),
        10,
        "视口窗口应包含恰好 10 行（viewport_height={viewport_height}）"
    );

    // 窗口：half=5, center_start=20, end=min(30,50)=30, start=30-10=20 → [20,30)
    // "line25" 应在索引 25-20=5
    let cursor_line_idx = 25 - 20;
    let cursor_span_text: String = result[cursor_line_idx]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert!(
        cursor_span_text.contains("line25"),
        "第 {cursor_line_idx} 行应包含 'line25'，实际内容: {cursor_span_text:?}"
    );

    // 验证光标样式已应用（至少有 1 个 span 使用 cursor_style）
    let has_cursor_style = result[cursor_line_idx]
        .spans
        .iter()
        .any(|s| s.style == cursor_style());
    assert!(
        has_cursor_style,
        "光标行应包含至少一个带有 cursor_style 的 span"
    );
}

/// 光标在最后一行时，窗口应从底部对齐，确保光标可见。
#[test]
fn test_viewport_cursor_at_last_line() {
    let (text, line_starts) = build_lines_text(50);

    // 光标在 line49 的第 2 个字符
    let cursor = line_starts[49] + 2;
    let viewport_height = 10;

    let result = super::render_multiline_with_cursor(TextAreaRenderOptions {
        text: &text,
        cursor,
        selection_range: None,
        placeholder: None,
        styles: TextAreaStyles {
            cursor: cursor_style(),
            selection: sel_style(),
            placeholder: ph_style(),
            base: Style::default(),
        },
        max_width: 80,
        viewport_height,
        show_cursor: true,
    });

    assert_eq!(result.len(), 10, "最后一行场景，窗口应返回 10 行");

    // 窗口应为 [40, 50)，最后一行 "line49" 在索引 49-40=9
    let last_line_text: String = result[9].spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        last_line_text.contains("line49"),
        "最后一行应包含 'line49'，实际: {last_line_text:?}"
    );

    let has_cursor = result[9].spans.iter().any(|s| s.style == cursor_style());
    assert!(has_cursor, "最后一行的光标样式应存在");
}

/// 光标在第一行时，窗口从顶部对齐。
#[test]
fn test_viewport_cursor_at_first_line() {
    let (text, line_starts) = build_lines_text(50);

    // 光标在 line0 的第 2 个字符
    let cursor = line_starts[0] + 2;
    let viewport_height = 10;

    let result = super::render_multiline_with_cursor(TextAreaRenderOptions {
        text: &text,
        cursor,
        selection_range: None,
        placeholder: None,
        styles: TextAreaStyles {
            cursor: cursor_style(),
            selection: sel_style(),
            placeholder: ph_style(),
            base: Style::default(),
        },
        max_width: 80,
        viewport_height,
        show_cursor: true,
    });

    assert_eq!(result.len(), 10, "第一行场景，窗口应返回 10 行");

    // 窗口应为 [0, 10)
    let first_line_text: String = result[0].spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        first_line_text.contains("line0"),
        "第一行应包含 'line0'，实际: {first_line_text:?}"
    );

    let has_cursor = result[0].spans.iter().any(|s| s.style == cursor_style());
    assert!(has_cursor, "第一行的光标样式应存在");
}

/// 当总行数 ≤ viewport_height 时，展示全部行，不进行窗口裁剪。
#[test]
fn test_viewport_small_text_returns_all_lines() {
    let text = "line0\nline1\nline2";
    let cursor = 8; // line1 的 'n' 位置: "line0\n"=6 chars, "li"=2 chars → 6+2=8
    let viewport_height = 10;

    let result = super::render_multiline_with_cursor(TextAreaRenderOptions {
        text,
        cursor,
        selection_range: None,
        placeholder: None,
        styles: TextAreaStyles {
            cursor: cursor_style(),
            selection: sel_style(),
            placeholder: ph_style(),
            base: Style::default(),
        },
        max_width: 80,
        viewport_height,
        show_cursor: true,
    });

    assert_eq!(
        result.len(),
        3,
        "3 行文本 + viewport_height=10 时，应返回全部 3 行"
    );
}

/// 验证光标在 viewport 窗口中正确偏移的场景（光标居中逻辑）。
#[test]
fn test_viewport_cursor_centered_in_window() {
    let (text, line_starts) = build_lines_text(30);

    // 光标在 line15 的第 2 个字符
    let cursor = line_starts[15] + 2;
    let viewport_height = 10;

    let result = super::render_multiline_with_cursor(TextAreaRenderOptions {
        text: &text,
        cursor,
        selection_range: None,
        placeholder: None,
        styles: TextAreaStyles {
            cursor: cursor_style(),
            selection: sel_style(),
            placeholder: ph_style(),
            base: Style::default(),
        },
        max_width: 80,
        viewport_height,
        show_cursor: true,
    });

    assert_eq!(result.len(), 10);

    // window: half=5, center_start=10, end=20, start=10 → [10, 20)
    // line15 在窗口索引 15-10=5
    let line_15_text: String = result[5].spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        line_15_text.contains("line15"),
        "居中场景，索引 5 应包含 'line15'，实际: {line_15_text:?}"
    );

    let has_cursor = result[5].spans.iter().any(|s| s.style == cursor_style());
    assert!(has_cursor, "居中场景光标样式应存在");
}

/// viewport 不变时连续移动光标，窗口应平滑移动而非跳跃。
#[test]
fn test_viewport_smooth_scroll_on_cursor_move() {
    let (text, line_starts) = build_lines_text(50);
    let viewport_height = 10;

    // 光标从 line23 → line22 → line21，窗口每次只移动 1 行
    for target in [23usize, 22, 21].windows(2) {
        let from = target[0];
        let to = target[1];

        let result_from = super::render_multiline_with_cursor(TextAreaRenderOptions {
            text: &text,
            cursor: line_starts[from] + 2,
            selection_range: None,
            placeholder: None,
            styles: TextAreaStyles {
                cursor: cursor_style(),
                selection: sel_style(),
                placeholder: ph_style(),
                base: Style::default(),
            },
            max_width: 80,
            viewport_height,
            show_cursor: true,
        });
        let result_to = super::render_multiline_with_cursor(TextAreaRenderOptions {
            text: &text,
            cursor: line_starts[to] + 2,
            selection_range: None,
            placeholder: None,
            styles: TextAreaStyles {
                cursor: cursor_style(),
                selection: sel_style(),
                placeholder: ph_style(),
                base: Style::default(),
            },
            max_width: 80,
            viewport_height,
            show_cursor: true,
        });

        // 两次渲染的窗口起始偏移差应为 1
        let first_text_from: String = result_from[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        let first_text_to: String = result_to[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        let line_num_from: usize = first_text_from
            .trim_start_matches("line")
            .parse()
            .expect("viewport smooth scroll on cursor move: fixture text must be valid input");
        let line_num_to: usize = first_text_to
            .trim_start_matches("line")
            .parse()
            .expect("viewport smooth scroll on cursor move: fixture text must be valid input");

        let diff = (line_num_from as isize - line_num_to as isize).unsigned_abs();
        assert!(
            diff <= 1,
            "窗口跳动不应超过 1 行: from line={line_num_from}, to line={line_num_to}"
        );
    }
}

// ── wrap_text 折行测试 ──────────────────────────────

/// ASCII 文本 6 字符，max_width=3，应折成 2 行 "abc" / "def"
#[test]
fn test_wrap_text_ascii_splits_at_width() {
    let result = super::wrap_text("abcdef", 4, 3); // cursor at 'e' (index 4)
    assert_eq!(result.total_visual_rows, 2);
    assert_eq!(result.visual_lines[0].text, "abc");
    assert_eq!(result.visual_lines[1].text, "def");
    assert_eq!(result.cursor_visual_row, 1);
    assert_eq!(result.cursor_visual_col, 1);
}

/// CJK 文本 "你好世界" (4 chars, 8 cols)，max_width=4，应折成 "你好"/"世界"
#[test]
fn test_wrap_text_cjk_splits_at_half() {
    let result = super::wrap_text("你好世界", 2, 4); // cursor at '世' (index 2)
    assert_eq!(result.total_visual_rows, 2);
    assert_eq!(result.visual_lines[0].text, "你好");
    assert_eq!(result.visual_lines[1].text, "世界");
    assert_eq!(result.cursor_visual_row, 1);
    assert_eq!(result.cursor_visual_col, 0);
}

/// 短文本不折行
#[test]
fn test_wrap_text_short_no_wrap() {
    let result = super::wrap_text("abc", 1, 10);
    assert_eq!(result.total_visual_rows, 1);
    assert_eq!(result.visual_lines[0].text, "abc");
    assert_eq!(result.cursor_visual_row, 0);
    assert_eq!(result.cursor_visual_col, 1);
}

/// max_width=1 时只折行不截断（CJK char 宽 2 也容纳）
#[test]
fn test_wrap_text_min_width_does_not_truncate() {
    let result = super::wrap_text("你", 1, 1);
    assert_eq!(result.total_visual_rows, 1);
    assert_eq!(result.visual_lines[0].text, "你");
}

/// 空文本返回 1 个空视觉行
#[test]
fn test_wrap_text_empty_returns_one_empty_line() {
    let result = super::wrap_text("", 0, 10);
    assert_eq!(result.total_visual_rows, 1);
    assert_eq!(result.visual_lines[0].text, "");
    assert_eq!(result.cursor_visual_row, 0);
    assert_eq!(result.cursor_visual_col, 0);
}

/// 多逻辑行 + 折行混合
#[test]
fn test_wrap_text_multi_logical_with_wrap() {
    let text = "abc\ndefgh";
    let result = super::wrap_text(text, 5, 3);
    assert_eq!(result.total_visual_rows, 3);
    assert_eq!(result.visual_lines[0].text, "abc");
    assert_eq!(result.visual_lines[1].text, "def");
    assert_eq!(result.visual_lines[2].text, "gh");
    assert_eq!(result.cursor_visual_row, 1);
    assert_eq!(result.cursor_visual_col, 1);
}

/// 光标在文本末尾
#[test]
fn test_wrap_text_cursor_at_text_end() {
    let result = super::wrap_text("你好", 2, 4);
    assert_eq!(result.cursor_visual_row, 0);
    assert_eq!(result.cursor_visual_col, 4);
}

/// 空行 + 非空行混合
#[test]
fn test_wrap_text_empty_lines_preserved() {
    let text = "a\n\nb";
    let result = super::wrap_text(text, 2, 10); // cursor = 2 (在空行)
    assert_eq!(result.total_visual_rows, 3);
    assert_eq!(result.visual_lines[1].text, "");
    assert_eq!(result.cursor_visual_row, 1); // 光标在空行
    assert_eq!(result.cursor_visual_col, 0);
}

/// `viewport_window` 是渲染与命中测试共用的滚动口径：
/// 全部行放得下时窗口从 0 开始，否则以光标视觉行为中心。
#[test]
fn test_viewport_window_centers_on_cursor_only_when_needed() {
    assert_eq!(super::viewport_window(6, 5, 10), (0, 6));
    assert_eq!(super::viewport_window(6, 5, 2), (4, 6));
    assert_eq!(super::viewport_window(6, 0, 2), (0, 2));
    // 视口高度 0 被 clamp 到 1，光标行仍落在窗口内
    assert_eq!(super::viewport_window(6, 5, 0), (5, 6));
}

fn area_styles() -> TextAreaStyles {
    TextAreaStyles {
        cursor: cursor_style(),
        selection: sel_style(),
        placeholder: ph_style(),
        base: Style::default(),
    }
}

#[test]
fn test_render_text_area_with_cursor_returns_cursor_cell() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    let mut state = super::TextAreaState::default();
    state.insert_str("ab\ncd");
    let mut buf = Buffer::empty(Rect::new(0, 0, 10, 5));
    let rect = Rect::new(0, 0, 10, 5);
    let position = super::render_text_area_with_cursor(
        &mut buf,
        rect,
        &state,
        true,
        Style::default(),
        area_styles(),
        None,
    );
    // 边框内起点 (1,1)，光标在第 2 个视觉行、第 2 列。
    assert_eq!(position, Some(ratatui::layout::Position::new(3, 2)));
}

#[test]
fn test_render_text_area_with_selection_does_not_panic() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    let mut state = super::TextAreaState::default();
    state.insert_str("hello\nworld");
    state.cursor = 7;
    state.cursor_word_left_extend();
    assert!(state.has_selection());
    let mut buf = Buffer::empty(Rect::new(0, 0, 20, 6));
    let rect = Rect::new(0, 0, 20, 6);
    let focused = super::render_text_area_with_cursor(
        &mut buf,
        rect,
        &state,
        true,
        Style::default(),
        area_styles(),
        None,
    );
    assert!(focused.is_some());
    // "world" starts on the inner second row; its selected first character
    // carries the selection background.
    assert_eq!(buf[(1, 2)].style().bg, Some(Color::DarkGray));
    let unfocused = super::render_text_area_with_cursor(
        &mut buf,
        rect,
        &state,
        false,
        Style::default(),
        area_styles(),
        None,
    );
    assert_eq!(unfocused, None);
}

#[test]
fn test_render_text_area_with_cursor_rejects_tiny_rects() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    let state = super::TextAreaState::default();
    let mut buf = Buffer::empty(Rect::new(0, 0, 4, 4));
    assert_eq!(
        super::render_text_area_with_cursor(
            &mut buf,
            Rect::new(0, 0, 1, 4),
            &state,
            true,
            Style::default(),
            area_styles(),
            None,
        ),
        None
    );
    assert_eq!(
        super::render_text_area_with_cursor(
            &mut buf,
            Rect::new(0, 0, 4, 2),
            &state,
            true,
            Style::default(),
            area_styles(),
            None,
        ),
        None
    );
}
