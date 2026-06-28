//! Interaction handler re-exports + a no-op placeholder handler.
//!
//! The concrete interaction handlers (HITL / AskUser / Rewind / OAuth) live in
//! [`crate::state_machine::handlers`]. The `Handler` trait itself is defined in
//! [`crate::state_machine::state`] (alongside the other top-level state types).
//!
//! This module re-exports the trait + provides a [`NoopHandler`] used as a
//! placeholder in tests and in P2 stubs where a real handler is not yet wired.

pub use super::state::{Handler, HandlerOutput};

/// A no-op interaction handler.
///
/// Used as a placeholder in Modal-state tests and P2 stubs where a concrete
/// handler (HITL / AskUser / Rewind / OAuth) is not yet constructed. Renders
/// nothing and consumes every key without action.
#[derive(Debug, Default)]
pub struct NoopHandler;

impl Handler for NoopHandler {
    fn render(&self, _area: (u16, u16)) {}

    fn handle_key(&mut self, _key: char) -> HandlerOutput {
        HandlerOutput::Nothing
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_noop_handler_default_returns_nothing() {
        let mut h = NoopHandler;
        assert_eq!(h.handle_key('a'), HandlerOutput::Nothing);
        assert_eq!(h.handle_key('\n'), HandlerOutput::Nothing);
    }

    #[test]
    fn test_noop_handler_render_does_not_panic() {
        let h = NoopHandler;
        h.render((80, 24));
    }
}
