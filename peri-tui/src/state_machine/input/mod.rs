//! Aggregated input state -- textarea buffer + cursor + at-mention popup +
//! slash-completion popup + attachments + prediction.
//!
//! This is a pure data structure: the `transitions::idle` module mutates it in
//! response to `Event::Key` / `Event::Paste`, and the rendering layer reads it
//! to draw the input area.
//!
//! Reference: `docs/design/peri-tui-architecture.md` section 8.5.

/// Aggregated input-box state.
///
/// Holds the raw buffer, cursor position, history navigation, prediction text
/// (grey placeholder), at-mention / slash-completion popup state, and the list
/// of attachments (images / files).
#[derive(Debug, Clone, Default)]
pub struct InputState {
    /// Raw text buffer content.
    pub buffer: String,

    /// Byte-offset cursor position within the buffer.
    ///
    /// Byte offset (not char offset) for parity with `tui_textarea`. The
    /// transitions layer must keep this within `buffer.len()`; CJK safety is
    /// the caller's responsibility (CLAUDE.md "字符串截断用字符级" rule).
    pub cursor: usize,

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

impl InputState {
    /// Create a new empty input state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset the buffer + cursor + prediction after submission.
    ///
    /// History and attachments are intentionally NOT cleared by this method:
    /// the transitions layer decides whether to push the submitted text into
    /// history, and attachments are flushed separately.
    pub fn clear_buffer(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
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
        assert!(s.buffer.is_empty());
        assert_eq!(s.cursor, 0);
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
        assert_eq!(a.buffer, b.buffer);
        assert_eq!(a.cursor, b.cursor);
    }

    #[test]
    fn test_clear_buffer_resets_transient_fields() {
        let mut s = InputState {
            buffer: "hello".into(),
            cursor: 3,
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
        assert!(s.buffer.is_empty());
        assert_eq!(s.cursor, 0);
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
