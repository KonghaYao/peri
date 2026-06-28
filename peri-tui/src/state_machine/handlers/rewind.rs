//! Rewind preview handler.
//!
//! Wraps a [`peri_acp_types::event_data::RewindPreview`] payload. The P2 stub
//! implements [`crate::state_machine::state::Handler`] with no real key
//! dispatch -- the actual rewind-confirmation UI logic lands in P3.

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

    fn handle_key(&mut self, _key: char) -> HandlerOutput {
        // P3 will dispatch Enter / Esc for confirm / cancel.
        HandlerOutput::Nothing
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
    fn test_handle_key_returns_nothing() {
        let mut h = RewindHandler::new(make_preview());
        assert_eq!(h.handle_key('\n'), HandlerOutput::Nothing);
    }
}
