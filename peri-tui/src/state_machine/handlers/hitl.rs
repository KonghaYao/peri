//! HITL approval handler.
//!
//! Wraps a [`peri_acp_types::event_data::HitlPending`] payload. The P2 stub
//! implements [`crate::state_machine::state::Handler`] with no real key
//! dispatch -- the actual approval UI logic lands in P3.

use peri_acp_types::event_data::HitlPending;

use super::super::state::{Handler, HandlerOutput};

/// Handler for a `"hitl-pending"` event. Holds the pending approval payload.
#[derive(Debug)]
pub struct HitlHandler {
    /// The pending approval request received from the ACP layer.
    pub pending: HitlPending,
}

impl HitlHandler {
    /// Create a new handler from a pending approval payload.
    pub fn new(pending: HitlPending) -> Self {
        Self { pending }
    }
}

impl Handler for HitlHandler {
    fn render(&self, _area: (u16, u16)) {}

    fn handle_key(&mut self, _key: char) -> HandlerOutput {
        // P3 will dispatch y / n / Tab / Enter for batch approvals.
        HandlerOutput::Nothing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_pending() -> HitlPending {
        HitlPending {
            tool_name: "Edit".into(),
            tool_input: serde_json::json!({"path": "foo.rs"}),
            batch: None,
        }
    }

    #[test]
    fn test_handler_stores_payload() {
        let h = HitlHandler::new(make_pending());
        assert_eq!(h.pending.tool_name, "Edit");
    }

    #[test]
    fn test_handle_key_returns_nothing() {
        let mut h = HitlHandler::new(make_pending());
        assert_eq!(h.handle_key('y'), HandlerOutput::Nothing);
    }
}
