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

#[test]
fn test_snap_col_to_char_boundary_already_on_boundary() {
    // "你好世界" = 12 bytes, boundaries at 0, 3, 6, 9, 12
    let line = "你好世界";
    assert_eq!(CursorPos::snap_col_to_char_boundary(line, 0), 0);
    assert_eq!(CursorPos::snap_col_to_char_boundary(line, 3), 3);
    assert_eq!(CursorPos::snap_col_to_char_boundary(line, 6), 6);
    assert_eq!(CursorPos::snap_col_to_char_boundary(line, 9), 9);
    assert_eq!(CursorPos::snap_col_to_char_boundary(line, 12), 12);
}

#[test]
fn test_snap_col_to_char_boundary_mid_char() {
    // "你好" = 6 bytes. '你'=0..3, '好'=3..6
    let line = "你好";
    // byte 1 is inside '你' → snap to 0
    assert_eq!(CursorPos::snap_col_to_char_boundary(line, 1), 0);
    // byte 2 is inside '你' → snap to 0
    assert_eq!(CursorPos::snap_col_to_char_boundary(line, 2), 0);
    // byte 4 is inside '好' → snap to 3
    assert_eq!(CursorPos::snap_col_to_char_boundary(line, 4), 3);
    // byte 5 is inside '好' → snap to 3
    assert_eq!(CursorPos::snap_col_to_char_boundary(line, 5), 3);
}

#[test]
fn test_snap_col_to_char_boundary_past_end() {
    let line = "你好";
    // past end snaps to line len (6), which IS a char boundary
    assert_eq!(CursorPos::snap_col_to_char_boundary(line, 100), 6);
}

#[test]
fn test_snap_col_to_char_boundary_empty_line() {
    assert_eq!(CursorPos::snap_col_to_char_boundary("", 5), 0);
}

#[test]
fn test_clamped_snaps_to_char_boundary() {
    // ASCII line with cursor at byte 4 → CJK line: byte 4 is mid-character in "你好"
    let lines = vec!["你好".to_string()];
    let c = CursorPos {
        row: 0,
        col_byte: 4, // in the middle of '好' (bytes 3-5)
    };
    let clamped = c.clamped(&lines);
    assert_eq!(clamped.col_byte, 3); // should snap to start of '好'
    assert!(lines[0].is_char_boundary(clamped.col_byte));
}
