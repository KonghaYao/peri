//! Aggregated input state -- textarea buffer + cursor + selection + at-mention popup +
//! slash-completion popup + attachments + prediction.
//!
//! This is a pure data structure: the `transitions::idle` module mutates it in
//! response to `Event::Key` / `Event::Paste`, and the rendering layer reads it
//! to draw the input area.
//!
//! Reference: `docs/design/peri-tui-architecture.md` section 8.5.

pub mod clipboard;
pub mod cursor;
pub mod edit;
pub mod selection;
pub mod sync;

#[cfg(test)]
mod clipboard_test;
#[cfg(test)]
mod cursor_test;
#[cfg(test)]
mod edit_test;
#[cfg(test)]
mod selection_test;
#[cfg(test)]
mod sync_test;

pub use clipboard::InputClipboard;
pub use cursor::CursorPos;
pub use edit::InputEdit;
pub use selection::{Selection, SelectionRange};
pub use sync::{from_textarea, to_textarea};

/// Aggregated input-box state.
///
/// Holds multi-line buffer, cursor position, selection, history navigation,
/// prediction text, at-mention / slash-completion popup state, and attachments.
#[derive(Debug, Clone)]
pub struct InputState {
    /// Multi-line text buffer (always >= 1 line; empty buffer is `vec![String::new()]`).
    pub lines: Vec<String>,

    /// Cursor position (row, col_byte).
    pub cursor: CursorPos,

    /// Current selection range, if any. Triggered by mouse drag / Shift+arrows / Ctrl+A.
    pub selection: Option<Selection>,

    /// Previously submitted input strings (newest at the back).
    pub history: Vec<String>,

    /// Current position in `history` while navigating with Up/Down.
    ///
    /// `None` means "not navigating, edits apply to the live buffer".
    pub history_index: Option<usize>,

    /// Greyed-out prediction text shown after the cursor (from
    /// `"prediction"` events).
    pub prediction: Option<String>,

    /// Active `@mention` popup state, if any.
    pub at_mention: Option<AtMentionState>,

    /// Active `/slash` completion popup state, if any.
    pub slash_completion: Option<SlashCompletionState>,

    /// Pending attachments (images, files) for the next submission.
    pub attachments: Vec<Attachment>,
}

impl Default for InputState {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
            cursor: CursorPos::default(),
            selection: None,
            history: Vec::new(),
            history_index: None,
            prediction: None,
            at_mention: None,
            slash_completion: None,
            attachments: Vec::new(),
        }
    }
}

impl InputState {
    /// Create a new empty input state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Full buffer text (lines joined by '\n').
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Insert a string at the cursor position (supports '\n' for multi-line).
    pub fn insert_str(&mut self, s: &str) {
        let parts: Vec<&str> = s.split('\n').collect();
        let CursorPos { row, col_byte } = self.cursor;

        let right_part: String = self.lines[row].drain(col_byte..).collect();
        self.lines[row].push_str(parts[0]);

        if parts.len() == 1 {
            // Single-line insert: append the right part back.
            self.lines[row].push_str(&right_part);
        }

        for (i, part) in parts.iter().enumerate().skip(1) {
            let mut new_line = String::new();
            new_line.push_str(part);
            if i == parts.len() - 1 {
                new_line.push_str(&right_part);
            }
            self.lines.insert(row + i, new_line);
        }

        // Update cursor.
        let new_row = row + parts.len() - 1;
        let new_col = if parts.len() == 1 {
            col_byte + parts[0].len()
        } else {
            parts.last().unwrap().len()
        };
        self.cursor = CursorPos::new(new_row, new_col);
    }

    /// Backspace. At line start, merges with the previous line.
    pub fn backspace(&mut self) {
        let CursorPos { row, col_byte } = self.cursor;
        if col_byte == 0 {
            if row > 0 {
                let prev_len = self.lines[row - 1].len();
                let current = self.lines.remove(row);
                self.lines[row - 1].push_str(&current);
                self.cursor = CursorPos::new(row - 1, prev_len);
            }
            return;
        }
        // CJK-safe delete: find previous char boundary.
        let line = &mut self.lines[row];
        let chars_before: Vec<(usize, char)> = line[..col_byte].char_indices().collect();
        if let Some(&(prev_byte, _)) = chars_before.last() {
            line.replace_range(prev_byte..col_byte, "");
            self.cursor = CursorPos::new(row, prev_byte);
        }
    }

    /// Clear buffer to empty single-line state.
    pub fn clear_buffer(&mut self) {
        self.lines = vec![String::new()];
        self.cursor = CursorPos::default();
        self.selection = None;
        self.prediction = None;
        self.at_mention = None;
        self.slash_completion = None;
    }
}

/// `@mention` file-completion popup state.
#[derive(Debug, Clone)]
pub struct AtMentionState {
    /// Candidate file paths returned by `"file-suggestions"`.
    pub candidates: Vec<String>,
    /// Currently highlighted candidate index.
    pub selected: usize,
}

/// `/slash` command-completion popup state.
#[derive(Debug, Clone)]
pub struct SlashCompletionState {
    /// Candidate command names (e.g. `["compact", "clear", "rewind"]`).
    pub candidates: Vec<String>,
    /// Currently highlighted candidate index.
    pub selected: usize,
}

/// A pending attachment to be submitted with the next message.
#[derive(Debug, Clone)]
pub struct Attachment {
    /// Raw attachment bytes (image / file content).
    pub data: Vec<u8>,
    /// MIME type (e.g. `"image/png"`).
    pub mime: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_empty() {
        let s = InputState::default();
        assert_eq!(s.lines.len(), 1);
        assert!(s.lines[0].is_empty());
        assert_eq!(s.cursor, CursorPos::default());
        assert!(s.history.is_empty());
        assert!(s.history_index.is_none());
        assert!(s.prediction.is_none());
        assert!(s.at_mention.is_none());
        assert!(s.slash_completion.is_none());
        assert!(s.attachments.is_empty());
    }

    #[test]
    fn test_new_equals_default() {
        let a = InputState::new();
        let b = InputState::default();
        assert_eq!(a.lines, b.lines);
        assert_eq!(a.cursor, b.cursor);
    }

    #[test]
    fn test_clear_buffer_resets_transient_fields() {
        let mut s = InputState {
            lines: vec!["hello".to_string()],
            cursor: CursorPos::new(0, 3),
            prediction: Some("world".into()),
            at_mention: Some(AtMentionState {
                candidates: vec!["a".into()],
                selected: 0,
            }),
            slash_completion: Some(SlashCompletionState {
                candidates: vec!["x".into()],
                selected: 0,
            }),
            ..Default::default()
        };

        s.clear_buffer();
        assert_eq!(s.lines.len(), 1);
        assert!(s.lines[0].is_empty());
        assert_eq!(s.cursor, CursorPos::default());
        assert!(s.prediction.is_none());
        assert!(s.at_mention.is_none());
        assert!(s.slash_completion.is_none());
    }

    #[test]
    fn test_clear_buffer_keeps_history_and_attachments() {
        let mut s = InputState {
            history: vec!["old".into()],
            attachments: vec![Attachment {
                data: vec![1, 2, 3],
                mime: "image/png".into(),
            }],
            ..Default::default()
        };

        s.clear_buffer();
        assert_eq!(s.history.len(), 1);
        assert_eq!(s.attachments.len(), 1);
    }
}
