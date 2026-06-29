//! tui_textarea::TextArea <-> InputState sync bridge.
//!
//! Converts between the state machine's canonical `InputState` and the terminal
//! widget `TextArea<'static>` used by the keyboard module and rendering layer.
//!
//! # Sync direction
//!
//! - `from_textarea`: After keyboard processing mutates the TextArea widget,
//!   this extracts the new state so the state machine stays in sync.
//! - `to_textarea`: Before rendering, this pushes `InputState` changes
//!   (from history navigation, rewind, etc.) back into the TextArea widget.

use super::cursor::CursorPos;

/// Extract an `InputState` snapshot from the current TextArea widget state.
///
/// Copies text content and cursor position. Selection, prediction, at-mention,
/// slash-completion, history, and attachments are **not** read from TextArea —
/// they are managed independently by the state machine.
pub fn from_textarea(ta: &tui_textarea::TextArea) -> super::InputState {
    let lines: Vec<String> = ta.lines().to_vec();
    let (row, col_byte) = ta.cursor();
    super::InputState {
        lines,
        cursor: CursorPos::new(row, col_byte),
        ..Default::default()
    }
}

/// Apply an `InputState` snapshot into a TextArea widget.
///
/// Replaces text content and cursor position. Does **not** touch selection,
/// prediction, at-mention, slash-completion, history, or attachments — those
/// are consumed by the rendering layer independently.
pub fn to_textarea(state: &super::InputState, ta: &mut tui_textarea::TextArea) {
    // Avoid clearing if content hasn't changed (preserves scroll position).
    let current_lines = ta.lines();
    let same_content = current_lines.len() == state.lines.len()
        && current_lines
            .iter()
            .zip(state.lines.iter())
            .all(|(a, b)| a == b);
    if !same_content {
        // Rebuild: clear + insert each line back.
        let line_count = current_lines.len();
        for _ in 0..line_count.saturating_sub(1) {
            ta.delete_line_by_end();
        }
        ta.move_cursor(tui_textarea::CursorMove::Head);
        ta.delete_line_by_end(); // clear the last/only line
        if state.lines.is_empty() {
            return;
        }
        ta.insert_str(&state.lines[0]);
        for line in &state.lines[1..] {
            ta.insert_newline();
            ta.insert_str(line);
        }
    }
    // Always sync cursor position (cheap, idempotent).
    let (current_row, current_col) = ta.cursor();
    if current_row != state.cursor.row || current_col != state.cursor.col_byte {
        ta.move_cursor(tui_textarea::CursorMove::Jump(
            state.cursor.row as u16,
            state.cursor.col_byte as u16,
        ));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_textarea(text: &str) -> tui_textarea::TextArea<'static> {
        let mut ta = tui_textarea::TextArea::default();
        ta.insert_str(text);
        ta
    }

    #[test]
    fn test_from_textarea_single_line() {
        let ta = make_textarea("hello world");
        let state = from_textarea(&ta);
        assert_eq!(state.lines, vec!["hello world"]);
        assert_eq!(state.cursor.row, 0);
        assert_eq!(state.cursor.col_byte, 11);
    }

    #[test]
    fn test_from_textarea_multi_line() {
        let mut ta = make_textarea("line1");
        ta.insert_newline();
        ta.insert_str("line2");
        let state = from_textarea(&ta);
        assert_eq!(state.lines, vec!["line1", "line2"]);
    }

    #[test]
    fn test_from_textarea_preserves_non_textarea_fields() {
        let ta = make_textarea("x");
        let state = from_textarea(&ta);
        // Fields not derived from TextArea stay at their defaults.
        assert!(state.selection.is_none());
        assert!(state.prediction.is_none());
        assert!(state.at_mention.is_none());
        assert!(state.slash_completion.is_none());
        assert!(state.history.is_empty());
        assert!(state.history_index.is_none());
        assert!(state.attachments.is_empty());
    }

    #[test]
    fn test_to_textarea_single_line() {
        let mut ta = make_textarea("");
        let state = super::super::InputState {
            lines: vec!["hello".into()],
            cursor: CursorPos::new(0, 3),
            ..Default::default()
        };
        to_textarea(&state, &mut ta);
        assert_eq!(ta.lines(), ["hello"]);
        assert_eq!(ta.cursor(), (0, 3));
    }

    #[test]
    fn test_to_textarea_multi_line() {
        let mut ta = make_textarea("");
        let state = super::super::InputState {
            lines: vec!["a".into(), "b".into(), "c".into()],
            cursor: CursorPos::new(1, 0),
            ..Default::default()
        };
        to_textarea(&state, &mut ta);
        assert_eq!(ta.lines(), ["a", "b", "c"]);
        assert_eq!(ta.cursor(), (1, 0));
    }

    #[test]
    fn test_to_textarea_empty_buffer() {
        let mut ta = make_textarea("old");
        let state = super::super::InputState {
            lines: vec![String::new()],
            cursor: CursorPos::default(),
            ..Default::default()
        };
        to_textarea(&state, &mut ta);
        assert_eq!(ta.lines(), [""]);
    }

    #[test]
    fn test_roundtrip() {
        let mut ta = make_textarea("hello\nworld");
        ta.move_cursor(tui_textarea::CursorMove::Jump(1, 2));
        let state = from_textarea(&ta);

        let mut ta2 = make_textarea("");
        to_textarea(&state, &mut ta2);
        assert_eq!(ta2.lines(), ["hello", "world"]);
        assert_eq!(ta2.cursor(), (1, 2));
    }

    #[test]
    fn test_roundtrip_cjk() {
        let mut ta = make_textarea("你好世界\n中文测试");
        ta.move_cursor(tui_textarea::CursorMove::Jump(0, 6)); // mid-CJK
        let state = from_textarea(&ta);

        let mut ta2 = make_textarea("");
        to_textarea(&state, &mut ta2);
        assert_eq!(ta2.lines(), ["你好世界", "中文测试"]);
    }
}
