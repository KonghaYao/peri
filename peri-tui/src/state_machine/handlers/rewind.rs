//! Rewind preview handler.
//!
//! Wraps a [`peri_acp_types::event_data::RewindPreview`] payload and
//! dispatches confirm/cancel keys (Enter/y = submit, Esc/n/q = dismiss).

use peri_acp_types::event_data::RewindPreview;
use ratatui::crossterm::event::{KeyCode, KeyEvent};

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
    fn render(&self, _frame: &mut ratatui::Frame, _area: ratatui::layout::Rect) {
        // Phase 1.3: render rewind preview popup (file/message change list +
        // confirm/cancel hints). For now the legacy popup system handles it.
    }

    fn handle_key(&mut self, key: KeyEvent) -> HandlerOutput {
        match key.code {
            // Enter, y, Y → confirm rewind
            KeyCode::Enter => HandlerOutput::Submit("confirmed".to_string()),
            KeyCode::Char('y' | 'Y') => HandlerOutput::Submit("confirmed".to_string()),
            // Esc, n, N, q, Q → dismiss
            KeyCode::Esc => HandlerOutput::Dismiss,
            KeyCode::Char('n' | 'N' | 'q' | 'Q') => HandlerOutput::Dismiss,
            _ => HandlerOutput::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::handler::{key, key_enter, key_esc};
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
            h.handle_key(key_enter()),
            HandlerOutput::Submit("confirmed".to_string())
        );
    }

    #[test]
    fn test_handle_key_y_confirms() {
        let mut h = RewindHandler::new(make_preview());
        assert_eq!(
            h.handle_key(key('y')),
            HandlerOutput::Submit("confirmed".to_string())
        );
    }

    #[test]
    fn test_handle_key_esc_dismisses() {
        let mut h = RewindHandler::new(make_preview());
        assert_eq!(h.handle_key(key_esc()), HandlerOutput::Dismiss);
    }

    #[test]
    fn test_handle_key_n_dismisses() {
        let mut h = RewindHandler::new(make_preview());
        assert_eq!(h.handle_key(key('n')), HandlerOutput::Dismiss);
    }

    #[test]
    fn test_handle_key_other_is_nothing() {
        let mut h = RewindHandler::new(make_preview());
        assert_eq!(h.handle_key(key('x')), HandlerOutput::Nothing);
    }
}
