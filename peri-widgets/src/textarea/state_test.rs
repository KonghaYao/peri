use crate::textarea::{classify_char, prev_word_boundary, CharCategory};
use crate::textarea::{display_width_before, render_multiline_with_cursor, TextAreaState};
use ratatui::style::{Color, Style};

#[test]
fn test_editor_state_replace_all_sets_cursor_to_end() {
    let mut s = TextAreaState::default();
    s.replace_all("hello".to_string());
    assert_eq!(s.text, "hello");
    assert_eq!(s.cursor, 5);
}

#[test]
fn test_editor_state_clear() {
    let mut s = TextAreaState::default();
    s.insert_str("abc");
    s.cursor = 2;
    s.clear();
    assert!(s.text.is_empty());
    assert_eq!(s.cursor, 0);
}

#[test]
fn test_editor_delete_forward_removes_char_after_cursor() {
    let mut s = TextAreaState::default();
    s.insert_str("ab你c");
    s.cursor = 2;
    s.delete_forward();
    assert_eq!(s.text, "abc");
    assert_eq!(s.cursor, 2);
}

#[test]
fn test_editor_delete_word_forward_removes_next_word() {
    let mut s = TextAreaState::default();
    s.insert_str("hello   world next");
    s.cursor = 5;
    s.delete_word_forward();
    // 新词边界：next_word_boundary 跳过同类空格后停在 'w'，删除 3 个空格
    assert_eq!(s.text, "helloworld next");
    assert_eq!(s.cursor, 5);
}

#[test]
fn test_editor_cursor_word_left_and_right() {
    let mut s = TextAreaState::default();
    s.insert_str("hello world next");
    s.cursor = 0; // insert_str 后光标在末尾，需手动归零
    s.cursor_word_right();
    assert_eq!(s.cursor, 6);
    s.cursor_word_right();
    assert_eq!(s.cursor, 12);
    s.cursor_word_left();
    assert_eq!(s.cursor, 6);
}

#[test]
fn test_editor_cursor_line_up_and_down() {
    let mut s = TextAreaState::default();
    s.insert_str("abc\nd\nefgh");
    s.cursor = 2;
    assert!(!s.cursor_line_up());
    assert!(s.cursor_line_down());
    assert_eq!(s.cursor, 5);
    assert!(s.cursor_line_down());
    assert_eq!(s.cursor, 7);
    assert!(s.cursor_line_up());
    assert_eq!(s.cursor, 5);
}

#[test]
fn test_char_to_byte_boundaries() {
    // ASCII
    assert_eq!(TextAreaState::char_to_byte("hello", 0), 0);
    assert_eq!(TextAreaState::char_to_byte("hello", 3), 3);
    assert_eq!(TextAreaState::char_to_byte("hello", 5), 5);
    assert_eq!(TextAreaState::char_to_byte("hello", 99), 5); // 越界回退

    // CJK
    assert_eq!(TextAreaState::char_to_byte("你好", 0), 0);
    assert_eq!(TextAreaState::char_to_byte("你好", 1), 3); // '你' 占 3 字节
    assert_eq!(TextAreaState::char_to_byte("你好", 2), 6);
}

#[test]
fn test_editor_cursor_line_home_and_end() {
    let mut s = TextAreaState::default();
    s.insert_str("abc\nde你f");
    s.cursor = 6;
    s.cursor_line_home();
    assert_eq!(s.cursor, 4);
    s.cursor_line_end();
    assert_eq!(s.cursor, 8);
}

fn cursor_style() -> Style {
    Style::default().fg(Color::Blue).bg(Color::Yellow)
}

fn sel_style() -> Style {
    Style::default().bg(Color::DarkGray)
}

fn ph_style() -> Style {
    Style::default().fg(Color::Gray)
}

#[test]
fn test_render_multiline_empty_shows_cursor() {
    let lines = render_multiline_with_cursor(
        "",
        0,
        cursor_style(),
        None,
        sel_style(),
        None,
        ph_style(),
        12,
        1,
        false,
        true,
    );
    assert_eq!(lines.len(), 1);
    // 空态：单行，光标为反色 space（字符反色风格）
    let spans = &lines[0].spans;
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].content, " ");
}

#[test]
fn test_render_multiline_empty_loading_shows_blank() {
    let lines = render_multiline_with_cursor(
        "",
        0,
        cursor_style(),
        None,
        sel_style(),
        None,
        ph_style(),
        12,
        1,
        true,
        true,
    );
    assert_eq!(lines.len(), 1);
    assert!(lines[0].spans.is_empty() || lines[0].spans.iter().all(|s| s.content.is_empty()));
}

#[test]
fn test_render_multiline_cjk_cursor_mid_line() {
    // "你好世界" (4 CJK chars, 8 display cols), cursor 在位置 2（"好"之后即"世"上）
    let lines = render_multiline_with_cursor(
        "你好世界",
        2,
        cursor_style(),
        None,
        sel_style(),
        None,
        ph_style(),
        12,
        12,
        false,
        true,
    );
    assert_eq!(lines.len(), 1);
    // spans: [Span("你好"), Span("世", cursor_style), Span("界")]
    assert_eq!(lines[0].spans.len(), 3);
    assert_eq!(lines[0].spans[0].content, "你好");
    // 光标字符应为反色高亮
    assert!(
        !lines[0].spans[1].style.bg.is_none() || !lines[0].spans[1].style.fg.is_none(),
        "cursor span should have non-default style (reversed fg/bg)"
    );
    assert_eq!(lines[0].spans[2].content, "界");
}

#[test]
fn test_render_multiline_cjk_cursor_at_start() {
    // "你好", cursor 在位置 0（"你"上）
    let lines = render_multiline_with_cursor(
        "你好",
        0,
        cursor_style(),
        None,
        sel_style(),
        None,
        ph_style(),
        12,
        12,
        false,
        true,
    );
    assert_eq!(lines.len(), 1);
    // spans: [Span("你", cursor_style), Span("好")]
    assert_eq!(lines[0].spans.len(), 2);
    assert!(
        !lines[0].spans[0].style.bg.is_none(),
        "cursor span should have background (reversed)"
    );
    assert_eq!(lines[0].spans[1].content, "好");
}

#[test]
fn test_render_multiline_cjk_cursor_at_end() {
    // "你好", cursor 在位置 2（末尾）
    let lines = render_multiline_with_cursor(
        "你好",
        2,
        cursor_style(),
        None,
        sel_style(),
        None,
        ph_style(),
        12,
        12,
        false,
        true,
    );
    assert_eq!(lines.len(), 1);
    // spans: [Span("你好"), Span(" ", cursor_style)]
    assert_eq!(lines[0].spans.len(), 2);
    assert_eq!(lines[0].spans[0].content, "你好");
    assert_eq!(lines[0].spans[1].content, " ");
}

#[test]
fn test_render_multiline_cjk_cursor_second_line() {
    // "abc\n你好", cursor 在位置 5（第二行"好"上）
    let lines = render_multiline_with_cursor(
        "abc\n你好",
        5,
        cursor_style(),
        None,
        sel_style(),
        None,
        ph_style(),
        12,
        12,
        false,
        true,
    );
    assert_eq!(lines.len(), 2);
    // 第一行无光标
    assert_eq!(lines[0].spans.len(), 1);
    assert_eq!(lines[0].spans[0].content, "abc");
    // 第二行：["你", Span("好", cursor_style)]
    assert_eq!(lines[1].spans.len(), 2);
    assert_eq!(lines[1].spans[0].content, "你");
    assert!(
        !lines[1].spans[1].style.bg.is_none(),
        "cursor span should have background"
    );
}

#[test]
fn test_display_width_before_cjk() {
    assert_eq!(display_width_before("abc", 2), 2);
    assert_eq!(display_width_before("abc", 0), 0);
    assert_eq!(display_width_before("你好世界", 2), 4); // 2 CJK chars = 4 cols
    assert_eq!(display_width_before("你好世界", 1), 2);
    assert_eq!(display_width_before("你好", 3), 4); // 超出 char 数返回全宽
}

#[test]
fn test_render_multiline_splits_newlines() {
    let lines = render_multiline_with_cursor(
        "a\nb\nc",
        0,
        cursor_style(),
        None,
        sel_style(),
        None,
        ph_style(),
        12,
        12,
        false,
        true,
    );
    assert_eq!(lines.len(), 3);
}

// ── Step 2: word.rs 词边界测试 ──────────────────────────────

#[test]
fn test_word_boundary_punct_vs_alpha() {
    // "fn foo(a)": f(0) n(1) ' '(2) f(3) o(4) o(5) ( (6) a(7) ) (8)
    let s = "fn foo(a)";
    // 末尾 cursor=9: 跳过 ')'→Punct，然后跳过 'a'→Other，停在 '('之后 → 7
    assert_eq!(prev_word_boundary(s, s.chars().count()), 7);
    // cursor=3 在空格之后：跳过空格→0
    assert_eq!(prev_word_boundary(s, 3), 0);
}

#[test]
fn test_word_boundary_cjk_punct() {
    let s = "你好好。世界";
    let total = s.chars().count(); // 6
                                   // 末尾 cursor=6: 跳过 世界(Other)→pos=4；'。'为全角 Punct 被当"词"跳过→3
                                   // prev_word_boundary 在 step3 将 '。' 作为 Punct 词整体回退
    assert_eq!(prev_word_boundary(s, total), 3);
}

#[test]
fn test_classify_char_basics() {
    assert!(matches!(classify_char(' '), CharCategory::Space));
    assert!(matches!(classify_char('.'), CharCategory::Punct));
    assert!(matches!(classify_char('a'), CharCategory::Other));
    assert!(matches!(classify_char('你'), CharCategory::Other));
    assert!(matches!(classify_char('。'), CharCategory::Punct)); // U+3002 全角句号
}

// ── Step 3: 选区基础操作 ────────────────────────────────────

#[test]
fn test_selection_basic() {
    let mut s = TextAreaState::default();
    s.insert_str("hello world");
    assert!(!s.has_selection());
    assert!(s.selection_range().is_none());
}

#[test]
fn test_selection_range_after_set() {
    let mut s = TextAreaState::default();
    s.insert_str("hello");
    s.selection_start = Some(0);
    s.cursor = 3;
    assert!(s.has_selection());
    assert_eq!(s.selection_range(), Some((0, 3)));
}

#[test]
fn test_delete_selection_removes_text() {
    let mut s = TextAreaState::default();
    s.insert_str("hello world");
    s.selection_start = Some(0);
    s.cursor = 6; // "hello "
    let deleted = s.delete_selection();
    assert_eq!(deleted.as_deref(), Some("hello "));
    assert_eq!(s.text, "world");
    assert_eq!(s.cursor, 0);
    assert!(!s.has_selection());
}

#[test]
fn test_type_with_selection_replaces() {
    let mut s = TextAreaState::default();
    s.insert_str("hello");
    s.selection_start = Some(0);
    s.cursor = 5;
    // insert_char 在操作前会 delete_selection
    s.insert_char('x');
    assert_eq!(s.text, "x");
    assert_eq!(s.cursor, 1);
    assert!(!s.has_selection());
}

#[test]
fn test_cancel_selection() {
    let mut s = TextAreaState::default();
    s.insert_str("hello");
    s.selection_start = Some(0);
    s.cancel_selection();
    assert!(!s.has_selection());
}

// ── Step 4: 撤销/重做 ──────────────────────────────────────

#[test]
fn test_undo_insert_char() {
    let mut s = TextAreaState::default();
    s.insert_char('a');
    s.insert_char('b');
    assert_eq!(s.text, "ab");
    assert!(s.undo()); // undo 'b'
    assert_eq!(s.text, "a");
    assert!(s.undo()); // undo 'a'
    assert_eq!(s.text, "");
    assert!(!s.undo()); // stack empty
}

#[test]
fn test_redo_after_undo() {
    let mut s = TextAreaState::default();
    s.insert_char('a');
    s.undo();
    assert_eq!(s.text, "");
    assert!(s.redo());
    assert_eq!(s.text, "a");
}

#[test]
fn test_redo_cleared_on_new_edit() {
    let mut s = TextAreaState::default();
    s.insert_char('a');
    s.undo();
    s.insert_char('b'); // new edit clears redo stack
    assert!(!s.redo()); // redo stack was cleared
    assert_eq!(s.text, "b");
}

#[test]
fn test_undo_delete_word() {
    let mut s = TextAreaState::default();
    s.insert_str("hello world");
    s.cursor = 11; // end
    s.delete_word_backward(); // 从末尾回退到词边界 0，删除整个文本
    assert_eq!(s.text, "");
    s.undo();
    assert_eq!(s.text, "hello world");
}

// ── Step 5: yank 粘贴 ──────────────────────────────────────

#[test]
fn test_yank_after_delete_selection() {
    let mut s = TextAreaState::default();
    s.insert_str("hello world");
    s.selection_start = Some(0);
    s.cursor = 6;
    s.delete_selection();
    assert!(s.yank.is_some());
    assert_eq!(s.yank.as_ref().unwrap().text(), "hello ");
}

#[test]
fn test_paste_yank_restores_deleted() {
    let mut s = TextAreaState::default();
    s.insert_str("hello world");
    s.cursor = 5; // after "hello"
    s.delete_word_backward(); // delete "hello"
    s.paste_yank();
    assert_eq!(s.text, "hello world");
}

#[test]
fn test_yank_reset_on_typing() {
    let mut s = TextAreaState::default();
    s.insert_str("hello");
    s.cursor = 5;
    s.delete_word_backward(); // delete "hello"
    assert!(s.yank.is_some());
    s.insert_char('x'); // typing clears yank
    assert!(s.yank.is_none());
}

// ── Step 6: 跨行退格/删除 ──────────────────────────────────

#[test]
fn test_backspace_at_line_start_merges_with_previous() {
    let mut s = TextAreaState::default();
    s.insert_str("abc\ndef");
    s.cursor = 4; // at start of "def"
    s.backspace(); // remove '\n' merging to "abcdef"
    assert_eq!(s.text, "abcdef");
    assert_eq!(s.cursor, 3);
}

#[test]
fn test_delete_forward_at_line_end_merges_next() {
    let mut s = TextAreaState::default();
    s.insert_str("abc\ndef");
    s.cursor = 3; // on '\n' (end of first line)
    s.delete_forward(); // delete '\n' merging to "abcdef"
    assert_eq!(s.text, "abcdef");
    assert_eq!(s.cursor, 3);
}

// ── Step 7: 带选区的渲染 ───────────────────────────────────

#[test]
fn test_render_selection_single_line() {
    let lines = render_multiline_with_cursor(
        "hello world",
        0,
        cursor_style(),
        Some((0, 5)),
        sel_style(),
        None,
        ph_style(),
        12,
        12,
        false,
        true,
    );
    assert_eq!(lines.len(), 1);
    // 应该有带 bg=DarkGray 的 Span（选区部分）
    let has_selection_bg = lines[0]
        .spans
        .iter()
        .any(|sp| sp.style.bg == Some(Color::DarkGray));
    assert!(has_selection_bg, "selection span should have DarkGray bg");
}

#[test]
fn test_render_selection_none_renders_normally() {
    let lines = render_multiline_with_cursor(
        "hello",
        0,
        cursor_style(),
        None,
        sel_style(),
        None,
        ph_style(),
        12,
        12,
        false,
        true,
    );
    assert_eq!(lines.len(), 1);
    // 无选区时，不应有 DarkGray bg span
    let has_dark_bg = lines[0]
        .spans
        .iter()
        .any(|sp| sp.style.bg == Some(Color::DarkGray));
    assert!(!has_dark_bg);
}

#[test]
fn test_render_show_cursor_false_no_cursor_highlight() {
    // show_cursor=false：光标位置字符不应用 cursor_style，无行尾 styled space
    let lines = render_multiline_with_cursor(
        "hello",
        2,
        cursor_style(),
        None,
        sel_style(),
        None,
        ph_style(),
        12,
        12,
        false,
        false,
    );
    assert_eq!(lines.len(), 1);
    // 所有 span 都应是默认 style（无 cursor_style 高亮）
    for span in &lines[0].spans {
        assert_eq!(
            span.style,
            Style::default(),
            "show_cursor=false 时文本不应有光标高亮"
        );
    }
}

#[test]
fn test_render_show_cursor_false_empty_with_placeholder() {
    // show_cursor=false + 空文本 + placeholder：渲染占位文本，无光标 space
    let lines = render_multiline_with_cursor(
        "",
        0,
        cursor_style(),
        None,
        sel_style(),
        Some("输入消息..."),
        ph_style(),
        12,
        1,
        false,
        false,
    );
    assert_eq!(lines.len(), 1);
    // 第一个 span 是 placeholder 文本（作为当前行显示的 placeholder 不被 cursor_style 干扰）
    // 不应该有 cursor_style 的 space
    let has_cursor_styled = lines[0]
        .spans
        .iter()
        .any(|s| s.content == " " && s.style != Style::default());
    assert!(!has_cursor_styled, "show_cursor=false 空态不应有光标 space");
}

// ── 视觉行移动测试（soft wrapping）──────────────────

#[test]
fn test_visual_down_cjk_wrapped() {
    let mut s = TextAreaState::default();
    s.insert_str("你好世界");
    s.cursor = 0;
    assert!(s.cursor_visual_down(4));
    assert_eq!(s.cursor, 2);
}

#[test]
fn test_visual_down_at_last_visual_row_returns_false() {
    let mut s = TextAreaState::default();
    s.insert_str("你好世界");
    s.cursor = 4;
    assert!(!s.cursor_visual_down(4));
    assert_eq!(s.cursor, 4);
}

#[test]
fn test_visual_up_at_first_visual_row_returns_false() {
    let mut s = TextAreaState::default();
    s.insert_str("你好世界");
    s.cursor = 2;
    assert!(s.cursor_visual_up(4));
    assert_eq!(s.cursor, 0);
    assert!(!s.cursor_visual_up(4));
}

#[test]
fn test_desired_col_cleared_on_edit() {
    let mut s = TextAreaState::default();
    s.insert_str("你好世界");
    s.cursor = 0;
    s.cursor_visual_down(4);
    assert!(s.desired_col.is_some());
    s.insert_char('x');
    assert!(s.desired_col.is_none());
}

#[test]
fn test_desired_col_cleared_on_horizontal_move() {
    let mut s = TextAreaState::default();
    s.insert_str("你好世界");
    s.cursor = 0;
    s.cursor_visual_down(4);
    assert!(s.desired_col.is_some());
    s.cursor_left();
    assert!(s.desired_col.is_none());
}

/// undo 清除 desired_col
#[test]
fn test_desired_col_cleared_on_undo() {
    let mut s = TextAreaState::default();
    s.insert_str("你好世界");
    s.cursor = 0;
    s.cursor_visual_down(4);
    assert!(s.desired_col.is_some());
    s.undo();
    assert!(s.desired_col.is_none());
}
