use super::input::{CursorPos, InputState};

#[test]
fn test_input_state_default_is_empty_single_line() {
    let s = InputState::default();
    assert_eq!(s.lines.len(), 1);
    assert!(s.lines[0].is_empty());
    assert_eq!(s.cursor, CursorPos::default());
    assert!(s.selection.is_none());
}

#[test]
fn test_input_state_insert_char_at_cursor() {
    let mut s = InputState::default();
    s.insert_str("hi");
    assert_eq!(s.lines, vec!["hi".to_string()]);
    assert_eq!(s.cursor, CursorPos::new(0, 2));
}

#[test]
fn test_input_state_insert_newline_splits_line() {
    let mut s = InputState::default();
    s.insert_str("ab\ncd");
    assert_eq!(s.lines, vec!["ab".to_string(), "cd".to_string()]);
    assert_eq!(s.cursor, CursorPos::new(1, 2));
}

#[test]
fn test_input_state_insert_at_middle_pushes_rest_right() {
    let mut s = InputState::default();
    s.insert_str("hello");
    s.cursor = CursorPos::new(0, 2);
    s.insert_str("XX");
    assert_eq!(s.lines, vec!["heXXllo".to_string()]);
    assert_eq!(s.cursor, CursorPos::new(0, 4));
}

#[test]
fn test_input_state_backspace_at_line_start_merges_with_prev() {
    let mut s = InputState::default();
    s.insert_str("ab\ncd");
    s.cursor = CursorPos::new(1, 0);
    s.backspace();
    assert_eq!(s.lines, vec!["abcd".to_string()]);
    assert_eq!(s.cursor, CursorPos::new(0, 2));
}

#[test]
fn test_input_state_backspace_in_middle_deletes_char() {
    let mut s = InputState::default();
    s.insert_str("hello");
    s.cursor = CursorPos::new(0, 3);
    s.backspace();
    assert_eq!(s.lines, vec!["helo".to_string()]);
    assert_eq!(s.cursor, CursorPos::new(0, 2));
}

#[test]
fn test_input_state_clear_resets_to_empty_single_line() {
    let mut s = InputState::default();
    s.insert_str("multi\nline\ntext");
    s.clear_buffer();
    assert_eq!(s.lines, vec![String::new()]);
    assert_eq!(s.cursor, CursorPos::default());
    assert!(s.selection.is_none());
}
