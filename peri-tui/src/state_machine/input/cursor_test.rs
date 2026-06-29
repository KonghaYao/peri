use super::cursor::CursorPos;

#[test]
fn test_cursor_default_is_origin() {
    let c = CursorPos::default();
    assert_eq!(c.row, 0);
    assert_eq!(c.col_byte, 0);
}

#[test]
fn test_cursor_from_byte_offset_single_line() {
    let lines = vec!["hello".to_string()];
    let c = CursorPos::from_byte_offset(&lines, 3);
    assert_eq!(c.row, 0);
    assert_eq!(c.col_byte, 3);
}

#[test]
fn test_cursor_from_byte_offset_multiline() {
    let lines = vec!["ab".to_string(), "cde".to_string()];
    // byte offset 3 = cross "ab\n" = (1, 0)
    let c = CursorPos::from_byte_offset(&lines, 3);
    assert_eq!(c.row, 1);
    assert_eq!(c.col_byte, 0);
}

#[test]
fn test_cursor_to_byte_offset_multiline() {
    let lines = vec!["ab".to_string(), "cde".to_string()];
    let c = CursorPos {
        row: 1,
        col_byte: 2,
    };
    assert_eq!(c.to_byte_offset(&lines), 3 + 2); // "ab\n" + "cd"[..2]
}

#[test]
fn test_cursor_clamp_to_line_end() {
    let lines = vec!["hi".to_string()];
    let c = CursorPos {
        row: 0,
        col_byte: 100,
    };
    let clamped = c.clamped(&lines);
    assert_eq!(clamped.col_byte, 2);
}
