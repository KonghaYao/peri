//! HITL approval handler.
//!
//! Wraps a [`peri_acp_types::event_data::HitlPending`] payload and implements
//! interactive key dispatch for batch tool approvals:
//!   y/Enter = approve, n/Esc = dismiss, Tab = cycle between batch tools.

use peri_acp_types::event_data::HitlPending;

use super::super::state::{Handler, HandlerOutput};

/// Handler for a `"hitl-pending"` event. Holds the pending approval payload
/// and internal navigation state (selected batch index, approved flag).
#[derive(Debug)]
pub struct HitlHandler {
    /// The pending approval request received from the ACP layer.
    pub pending: HitlPending,
    /// Index of the currently selected tool in the batch (0 if no batch).
    selected: usize,
    /// Whether the user has confirmed approval.
    approved: bool,
}

impl HitlHandler {
    /// Create a new handler from a pending approval payload.
    pub fn new(pending: HitlPending) -> Self {
        Self {
            pending,
            selected: 0,
            approved: false,
        }
    }

    /// Total number of tools in the batch (1 for single, batch.len() for batch).
    fn total(&self) -> usize {
        self.pending
            .batch
            .as_ref()
            .map(|b| b.len())
            .unwrap_or(1)
            .max(1)
    }
}

impl Handler for HitlHandler {
    fn render(&self, _area: (u16, u16)) {
        // P5: rendering will use legacy popup system or a new v2 popup
    }

    fn handle_key(&mut self, key: char) -> HandlerOutput {
        match key {
            'y' | 'Y' => {
                self.approved = true;
                HandlerOutput::Submit("approved".to_string())
            }
            'n' | 'N' => HandlerOutput::Dismiss,
            '\t' => {
                let total = self.total();
                if total > 1 {
                    self.selected = (self.selected + 1) % total;
                }
                HandlerOutput::Nothing
            }
            '\n' | '\r' => {
                self.approved = true;
                HandlerOutput::Submit("approved".to_string())
            }
            _ => HandlerOutput::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp_types::event_data::ToolApproval;

    fn make_pending() -> HitlPending {
        HitlPending {
            tool_name: "Edit".into(),
            tool_input: serde_json::json!({"path": "foo.rs"}),
            batch: None,
        }
    }

    fn make_batch_pending() -> HitlPending {
        HitlPending {
            tool_name: "Edit".into(),
            tool_input: serde_json::json!({"path": "foo.rs"}),
            batch: Some(vec![
                ToolApproval {
                    tool_id: "1".into(),
                    tool_name: "Edit".into(),
                    input_summary: "edit foo.rs".into(),
                },
                ToolApproval {
                    tool_id: "2".into(),
                    tool_name: "Write".into(),
                    input_summary: "write bar.rs".into(),
                },
            ]),
        }
    }

    #[test]
    fn test_handler_stores_payload() {
        let h = HitlHandler::new(make_pending());
        assert_eq!(h.pending.tool_name, "Edit");
    }

    #[test]
    fn test_handler_initial_state() {
        let h = HitlHandler::new(make_pending());
        assert_eq!(h.selected, 0);
        assert!(!h.approved);
        assert_eq!(h.total(), 1);
    }

    #[test]
    fn test_total_single_tool() {
        let h = HitlHandler::new(make_pending());
        assert_eq!(h.total(), 1);
    }

    #[test]
    fn test_total_batch() {
        let h = HitlHandler::new(make_batch_pending());
        assert_eq!(h.total(), 2);
    }

    #[test]
    fn test_handle_key_y_submits() {
        let mut h = HitlHandler::new(make_pending());
        assert_eq!(
            h.handle_key('y'),
            HandlerOutput::Submit("approved".to_string())
        );
        assert!(h.approved);
    }

    #[test]
    fn test_handle_key_uppercase_y_submits() {
        let mut h = HitlHandler::new(make_pending());
        assert_eq!(
            h.handle_key('Y'),
            HandlerOutput::Submit("approved".to_string())
        );
        assert!(h.approved);
    }

    #[test]
    fn test_handle_key_n_dismisses() {
        let mut h = HitlHandler::new(make_pending());
        assert_eq!(h.handle_key('n'), HandlerOutput::Dismiss);
    }

    #[test]
    fn test_handle_key_uppercase_n_dismisses() {
        let mut h = HitlHandler::new(make_pending());
        assert_eq!(h.handle_key('N'), HandlerOutput::Dismiss);
    }

    #[test]
    fn test_handle_key_enter_submits() {
        let mut h = HitlHandler::new(make_pending());
        assert_eq!(
            h.handle_key('\n'),
            HandlerOutput::Submit("approved".to_string())
        );
        assert!(h.approved);
    }

    #[test]
    fn test_handle_key_carriage_return_submits() {
        let mut h = HitlHandler::new(make_pending());
        assert_eq!(
            h.handle_key('\r'),
            HandlerOutput::Submit("approved".to_string())
        );
        assert!(h.approved);
    }

    #[test]
    fn test_handle_key_tab_single_tool_noop() {
        let mut h = HitlHandler::new(make_pending());
        assert_eq!(h.handle_key('\t'), HandlerOutput::Nothing);
        assert_eq!(h.selected, 0);
    }

    #[test]
    fn test_handle_key_tab_cycles_batch() {
        let mut h = HitlHandler::new(make_batch_pending());
        assert_eq!(h.handle_key('\t'), HandlerOutput::Nothing);
        assert_eq!(h.selected, 1);
        assert_eq!(h.handle_key('\t'), HandlerOutput::Nothing);
        assert_eq!(h.selected, 0);
        assert_eq!(h.handle_key('\t'), HandlerOutput::Nothing);
        assert_eq!(h.selected, 1);
    }

    #[test]
    fn test_handle_key_unknown_returns_nothing() {
        let mut h = HitlHandler::new(make_pending());
        assert_eq!(h.handle_key('x'), HandlerOutput::Nothing);
    }

    #[test]
    fn test_handle_key_does_not_approve_on_tab() {
        let mut h = HitlHandler::new(make_batch_pending());
        h.handle_key('\t');
        assert!(!h.approved);
    }
}
