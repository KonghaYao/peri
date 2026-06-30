//! Rewind preview handler.
//!
//! Wraps a [`peri_acp_types::event_data::RewindPreview`] payload and
//! dispatches confirm/cancel keys (Enter/y = submit, Esc/n/q = dismiss).

use peri_acp_types::event_data::RewindPreview;

use super::super::state::{Handler, HandlerOutput};

/// Handler for a `"rewind-preview"` event. Holds the change preview.
#[derive(Debug)]
pub struct RewindHandler {
    /// The file/message change preview received from the ACP layer.
    pub preview: RewindPreview,
}

impl RewindHandler {
    /// Create a new handler from a rewind-preview payload.
    pub fn new(preview: RewindPreview) -> Self {
        Self { preview }
    }
}

impl Handler for RewindHandler {
    fn render(&self, _area: (u16, u16)) {}

    fn handle_key(&mut self, key: char) -> HandlerOutput {
        match key {
            // Enter, y, Y → confirm rewind
            '\n' | '\r' | 'y' | 'Y' => HandlerOutput::Submit("confirmed".to_string()),
            // Esc, n, N, q, Q → dismiss
            '\x1b' | 'n' | 'N' | 'q' | 'Q' => HandlerOutput::Dismiss,
            _ => HandlerOutput::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_preview() -> RewindPreview {
        RewindPreview {
            files: vec![],
            messages: vec![],
        }
    }

    #[test]
    fn test_handler_stores_payload() {
        let h = RewindHandler::new(make_preview());
        assert!(h.preview.files.is_empty());
    }

    #[test]
    fn test_handle_key_enter_confirms() {
        let mut h = RewindHandler::new(make_preview());
        assert_eq!(
            h.handle_key('\n'),
            HandlerOutput::Submit("confirmed".to_string())
        );
    }

    #[test]
    fn test_handle_key_y_confirms() {
        let mut h = RewindHandler::new(make_preview());
        assert_eq!(
            h.handle_key('y'),
            HandlerOutput::Submit("confirmed".to_string())
        );
    }

    #[test]
    fn test_handle_key_esc_dismisses() {
        let mut h = RewindHandler::new(make_preview());
        assert_eq!(h.handle_key('\x1b'), HandlerOutput::Dismiss);
    }

    #[test]
    fn test_handle_key_n_dismisses() {
        let mut h = RewindHandler::new(make_preview());
        assert_eq!(h.handle_key('n'), HandlerOutput::Dismiss);
    }

    #[test]
    fn test_handle_key_other_is_nothing() {
        let mut h = RewindHandler::new(make_preview());
        assert_eq!(h.handle_key('x'), HandlerOutput::Nothing);
    }
}
