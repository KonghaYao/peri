use super::*;

// ── Vertical cursor movement across lines with mixed ASCII/CJK ──────────────

/// 复现崩溃场景：ASCII 行中 cursor 在 byte 4 → 移到只有 CJK 字符的行 →
/// col_byte=4 落在 '好'（字节 3-5）中间 → 后续操作 panic。
#[test]
fn test_move_cursor_down_snaps_col_to_char_boundary() {
    let mut state = InputState {
        lines: vec!["abcdefgh".to_string(), "你好".to_string()],
        cursor: CursorPos::new(0, 4), // "abcdefgh" 上 byte 4 = 'e' 之后
        ..Default::default()
    };
    state.move_cursor_down(false);
    // "你好" char boundaries: 0, 3, 6. byte 4 → snap to 3.
    assert_eq!(state.cursor.row, 1);
    assert_eq!(state.cursor.col_byte, 3);
    assert!(state.lines[1].is_char_boundary(state.cursor.col_byte));
}

#[test]
fn test_move_cursor_up_snaps_col_to_char_boundary() {
    let mut state = InputState {
        lines: vec!["你好".to_string(), "abcdefgh".to_string()],
        cursor: CursorPos::new(1, 4), // "abcdefgh" 上 byte 4 = 'e' 之后
        ..Default::default()
    };
    state.move_cursor_up(false);
    // "你好" char boundaries: 0, 3, 6. byte 4 → snap to 3.
    assert_eq!(state.cursor.row, 0);
    assert_eq!(state.cursor.col_byte, 3);
    assert!(state.lines[0].is_char_boundary(state.cursor.col_byte));
}

/// 上下移动后立即输入字符不应 panic（原 bug 会在 insert_str 的 drain 处 panic）。
#[test]
fn test_vertical_move_then_insert_does_not_panic() {
    let mut state = InputState {
        lines: vec!["abcdefgh".to_string(), "你好".to_string()],
        cursor: CursorPos::new(0, 4),
        ..Default::default()
    };
    state.move_cursor_down(false);
    // 在 "你好" 的第 2 个字符前插入 "X" → "你X好"
    state.insert_str("X");
    assert_eq!(state.lines[1], "你X好");
    assert_eq!(state.cursor.col_byte, 4); // '你'(3) + 'X'(1) = 4
}

/// 上下移动后立即 backspace 不应 panic。
#[test]
fn test_vertical_move_then_backspace_does_not_panic() {
    let mut state = InputState {
        lines: vec!["abcdefgh".to_string(), "你好".to_string()],
        cursor: CursorPos::new(0, 4),
        ..Default::default()
    };
    state.move_cursor_down(false); // cursor 在 col_byte=3, "你好" 中 = '好' 前
    state.move_cursor_right(false); // cursor 移到 '好' 后, col_byte=6
    state.backspace(); // 删除 '好'
    assert_eq!(state.lines[1], "你");
    assert_eq!(state.cursor.col_byte, 3);
}

/// 应该放在中间的 CJK 字符边界上（col_byte < 目标行中间 char 起始位置）。
#[test]
fn test_move_cursor_down_mid_cjk_snaps_left() {
    let mut state = InputState {
        lines: vec!["ab".to_string(), "你好世界".to_string()],
        cursor: CursorPos::new(0, 1), // "ab" 中 'b' 之前，byte 1
        ..Default::default()
    };
    state.move_cursor_down(false);
    // "你好世界" boundaries: 0, 3, 6, 9, 12. byte 1 → snap to 0.
    assert_eq!(state.cursor.row, 1);
    assert_eq!(state.cursor.col_byte, 0);
}

/// cursor 在目标行的第一个 char 之后、char 中间 → 应 snap 到 char 起始。
#[test]
fn test_move_cursor_down_inside_first_cjk_char() {
    let mut state = InputState {
        lines: vec!["abcdefg".to_string(), "你好".to_string()],
        cursor: CursorPos::new(0, 2), // byte 2 in ASCII
        ..Default::default()
    };
    state.move_cursor_down(false);
    // "你好" boundaries: 0, 3, 6. byte 2 is inside '你'(0-2) → snap to 0.
    assert_eq!(state.cursor.row, 1);
    assert_eq!(state.cursor.col_byte, 0);
}

/// cursor 已经在合法边界上时不应被修改。
#[test]
fn test_move_cursor_up_already_on_boundary() {
    let mut state = InputState {
        lines: vec!["你好世界".to_string(), "abcdefgh".to_string()],
        cursor: CursorPos::new(1, 6), // byte 6 on "abcdefgh" = 'g' 之后
        ..Default::default()
    };
    state.move_cursor_up(false);
    // "你好世界" boundaries: 0, 3, 6, 9, 12. byte 6 IS a boundary.
    assert_eq!(state.cursor.row, 0);
    assert_eq!(state.cursor.col_byte, 6);
    assert!(state.lines[0].is_char_boundary(state.cursor.col_byte));
}

/// clamped() 在 CJK 行中也应 snap 到 char boundary。
#[test]
fn test_clamped_cjk_snaps_char_boundary() {
    let lines = vec!["你好".to_string()];
    let c = CursorPos {
        row: 0,
        col_byte: 5,
    }; // 在 '好' (bytes 3-5) 内
    let clamped = c.clamped(&lines);
    assert_eq!(clamped.col_byte, 3);
    assert!(lines[0].is_char_boundary(clamped.col_byte));
}
