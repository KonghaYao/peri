//! tui_textarea::TextArea <-> InputState sync bridge.
//!
//! Converts between the state machine's canonical `InputState` and the terminal
//! widget `TextArea<'static>` used by the keyboard module and rendering layer.
//!
//! # Sync direction
//!
//! - `to_textarea`: Before rendering, this pushes `InputState` changes
//!   (from history navigation, rewind, etc.) back into the TextArea widget.

/// Apply an `InputState` snapshot into a TextArea widget.
///
/// Replaces text content and cursor position. Does **not** touch selection,
/// prediction, at-mention, slash-completion, history, or attachments — those
/// are consumed by the rendering layer independently.
///
/// **Important**: `CursorPos.col_byte` is a **byte offset**, but
/// `tui_textarea::CursorMove::Jump` expects a **character index**.
/// The conversion: `chars().count()` on the byte prefix.
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
    // Convert byte offset → character index for tui_textarea.
    let col_char = state
        .lines
        .get(state.cursor.row)
        .map(|line| {
            let safe_byte = state.cursor.col_byte.min(line.len());
            line[..safe_byte].chars().count()
        })
        .unwrap_or(0);
    let (current_row, current_col) = ta.cursor();
    if current_row != state.cursor.row || current_col != col_char {
        ta.move_cursor(tui_textarea::CursorMove::Jump(
            state.cursor.row as u16,
            col_char as u16,
        ));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_machine::input::CursorPos;

    fn make_textarea(text: &str) -> tui_textarea::TextArea<'static> {
        let mut ta = tui_textarea::TextArea::default();
        ta.insert_str(text);
        ta
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
}