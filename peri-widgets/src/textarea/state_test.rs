use crate::textarea::{TextAreaState, display_width_before, render_multiline_with_cursor};
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
    let mut s = TextAreaState {
        text: "abc".into(),
        cursor: 2,
    };
    s.clear();
    assert!(s.text.is_empty());
    assert_eq!(s.cursor, 0);
}

#[test]
fn test_editor_delete_forward_removes_char_after_cursor() {
    let mut s = TextAreaState {
        text: "ab你c".into(),
        cursor: 2,
    };
    s.delete_forward();
    assert_eq!(s.text, "abc");
    assert_eq!(s.cursor, 2);
}

#[test]
fn test_editor_delete_word_forward_removes_next_word() {
    let mut s = TextAreaState {
        text: "hello   world next".into(),
        cursor: 5,
    };
    s.delete_word_forward();
    assert_eq!(s.text, "hello next");
    assert_eq!(s.cursor, 5);
}

#[test]
fn test_editor_cursor_word_left_and_right() {
    let mut s = TextAreaState {
        text: "hello world next".into(),
        cursor: 0,
    };
    s.cursor_word_right();
    assert_eq!(s.cursor, 6);
    s.cursor_word_right();
    assert_eq!(s.cursor, 12);
    s.cursor_word_left();
    assert_eq!(s.cursor, 6);
}

#[test]
fn test_editor_cursor_line_up_and_down() {
    let mut s = TextAreaState {
        text: "abc\nd\nefgh".into(),
        cursor: 2,
    };
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
    let mut s = TextAreaState {
        text: "abc\nde你f".into(),
        cursor: 6,
    };
    s.cursor_line_home();
    assert_eq!(s.cursor, 4);
    s.cursor_line_end();
    assert_eq!(s.cursor, 8);
}

fn cursor_style() -> Style {
    Style::default().fg(Color::Blue).bg(Color::Yellow)
}

#[test]
fn test_render_multiline_empty_shows_cursor() {
    let lines = render_multiline_with_cursor("", 0, cursor_style(), false);
    assert_eq!(lines.len(), 1);
    // 空态：单行，光标为反色 space（字符反色风格）
    let spans = &lines[0].spans;
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].content, " ");
}

#[test]
fn test_render_multiline_empty_loading_shows_blank() {
    let lines = render_multiline_with_cursor("", 0, cursor_style(), true);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].spans.is_empty() || lines[0].spans.iter().all(|s| s.content.is_empty()));
}

#[test]
fn test_render_multiline_cjk_cursor_mid_line() {
    // "你好世界" (4 CJK chars, 8 display cols), cursor 在位置 2（"好"之后即"世"上）
    let lines = render_multiline_with_cursor("你好世界", 2, cursor_style(), false);
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
    let lines = render_multiline_with_cursor("你好", 0, cursor_style(), false);
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
    let lines = render_multiline_with_cursor("你好", 2, cursor_style(), false);
    assert_eq!(lines.len(), 1);
    // spans: [Span("你好"), Span(" ", cursor_style)]
    assert_eq!(lines[0].spans.len(), 2);
    assert_eq!(lines[0].spans[0].content, "你好");
    assert_eq!(lines[0].spans[1].content, " ");
}

#[test]
fn test_render_multiline_cjk_cursor_second_line() {
    // "abc\n你好", cursor 在位置 5（第二行"好"上）
    let lines = render_multiline_with_cursor("abc\n你好", 5, cursor_style(), false);
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
    let lines = render_multiline_with_cursor("a\nb\nc", 0, cursor_style(), false);
    assert_eq!(lines.len(), 3);
}
