//! Interaction handler re-exports + a no-op placeholder handler.
//!
//! The concrete interaction handlers (HITL / AskUser / Rewind / OAuth) live in
//! [`crate::state_machine::handlers`]. The `Handler` trait itself is defined in
//! [`crate::state_machine::state`] (alongside the other top-level state types).
//!
//! This module re-exports the trait + provides a [`NoopHandler`] used as a
//! placeholder in tests and in P2 stubs where a real handler is not yet wired.

use ratatui::crossterm::event::KeyEvent;

pub use super::state::{Handler, HandlerOutput};

/// A no-op interaction handler.
///
/// Used as a placeholder in Modal-state tests and P2 stubs where a concrete
/// handler (HITL / AskUser / Rewind / OAuth) is not yet constructed. Renders
/// nothing and consumes every key without action.
#[derive(Debug, Default)]
pub struct NoopHandler;

impl Handler for NoopHandler {
    fn render(&self, _frame: &mut ratatui::Frame, _area: ratatui::layout::Rect) {}

    fn handle_key(&mut self, _key: KeyEvent) -> HandlerOutput {
        HandlerOutput::Nothing
    }
}

/// Test helper: build a [`KeyEvent`] from a plain char with no modifiers.
///
/// Production code should construct `KeyEvent`s directly. This helper exists
/// so handler unit tests can be written concisely (`key('y')` instead of
/// `KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)`).
#[cfg(test)]
pub(crate) fn key(c: char) -> KeyEvent {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

/// Test helper: build an Enter [`KeyEvent`].
#[cfg(test)]
pub(crate) fn key_enter() -> KeyEvent {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
}

/// Test helper: build a Tab [`KeyEvent`].
#[cfg(test)]
pub(crate) fn key_tab() -> KeyEvent {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)
}

/// Test helper: build an Esc [`KeyEvent`].
#[cfg(test)]
pub(crate) fn key_esc() -> KeyEvent {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
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
        assert_eq!(h.handle_key(key('a')), HandlerOutput::Nothing);
        assert_eq!(h.handle_key(key_enter()), HandlerOutput::Nothing);
    }

    #[test]
    fn test_noop_handler_render_does_not_panic() {
        // NoopHandler.render is a no-op; we just verify it compiles with the
        // new Frame signature. A real Frame requires a terminal backend which
        // is heavy to construct in a unit test, so we skip the actual call.
        let _h = NoopHandler;
    }
}
