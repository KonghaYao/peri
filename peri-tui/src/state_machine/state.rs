//! Top-level state enum for the TUI state machine.
//!
//! Defines four mutually-exclusive top-level states (Idle / Streaming / Modal /
//! Switching) as specified in `docs/design/peri-tui-architecture.md` section 8.4.
//!
//! Each state holds exactly the data it needs -- no shared mutable context,
//! no `&mut` references to external services. The state machine is a pure
//! function `(State, Event) -> (State, Vec<Effect>)`.

use peri_acp_types::view_model::ViewModel;

// ---------------------------------------------------------------------------
// Re-exports from sibling modules
// ---------------------------------------------------------------------------

// `CurrentTurn` and `InputState` were placeholders in this file during early
// P2 scaffolding. They now live in their dedicated modules and are re-exported
// here so existing references (`super::CurrentTurn`, `state::InputState`) keep
// working.
pub use crate::state_machine::current_turn::CurrentTurn;
pub use crate::state_machine::input::InputState;

// ---------------------------------------------------------------------------
// Re-exports from the panel module (Phase 3 infrastructure)
// ---------------------------------------------------------------------------

/// v2 `PanelState` trait, `PanelReadContext`, and `PanelEffect`.
///
/// These types are defined in `crate::panel` and re-exported here so that
/// the state machine's `ModalState::Panel(Box<dyn PanelState>)` and the
/// `transitions::modal` module can reference them without reaching across
/// the crate.
pub use crate::panel::{PanelEffect, PanelReadContext, PanelState};

/// Interface implemented by every interaction handler (HITL / AskUser / Rewind /
/// OAuth).
///
/// The state machine injects the concrete handler when entering
/// `Modal::Interaction`. Key dispatch delegates to the active handler
/// without knowing its type.
///
/// Signatures are simplified stubs; refined in Phase 3.
pub trait Handler: Send + std::fmt::Debug {
    /// Render the interaction popup.
    fn render(&self, area: (u16, u16));

    /// Handle a key event. Returns the handler's output which the state
    /// machine translates to standard effects.
    fn handle_key(&mut self, key: char) -> HandlerOutput;
}

/// Result of a handler key-press. Simplified stub.
#[derive(Debug, Clone, PartialEq)]
pub enum HandlerOutput {
    /// No action (key consumed but no decision yet).
    Nothing,
    /// User approved / answered / confirmed.
    Submit(String),
    /// User dismissed the popup.
    Dismiss,
}

// ---------------------------------------------------------------------------
// Double-Esc tracker
// ---------------------------------------------------------------------------

/// Tracks whether the user pressed Esc twice within a short window.
///
/// Used to implement the "press Esc twice to quit" gesture. The timer
/// resets on the first press; the second press within the threshold
/// triggers a quit effect.
#[derive(Debug, Clone)]
pub struct DoubleEscTracker {
    /// Instant of the first Esc press, if any.
    pub first_press_at: Option<std::time::Instant>,
}

impl DoubleEscTracker {
    /// Maximum duration between two Esc presses that still counts as
    /// a "double press" (500 ms).
    pub const THRESHOLD_MS: u64 = 500;

    /// Create a new tracker in the idle state.
    pub fn new() -> Self {
        Self {
            first_press_at: None,
        }
    }

    /// Record an Esc press. Returns `true` if this is the second press
    /// within the threshold (meaning the application should quit).
    pub fn press_esc(&mut self) -> bool {
        let now = std::time::Instant::now();
        if let Some(first) = self.first_press_at {
            let elapsed = now.duration_since(first);
            self.first_press_at = None;
            if elapsed.as_millis() < Self::THRESHOLD_MS as u128 {
                return true; // double-press detected
            }
        }
        self.first_press_at = Some(now);
        false
    }
}

impl Default for DoubleEscTracker {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Top-level State enum
// ---------------------------------------------------------------------------

/// The four mutually-exclusive top-level states of the TUI.
///
/// Reference: `docs/design/peri-tui-architecture.md` section 8.4.
#[derive(Debug)]
pub enum State {
    /// Waiting for user input.
    ///
    /// Holds the input box, scroll position, the last committed ViewModel
    /// snapshot, double-Esc tracker, and input-history navigation index.
    Idle(IdleState),

    /// Agent is actively producing output (text chunks, tool calls).
    ///
    /// Holds the current turn's incremental data (`CurrentTurn`) alongside
    /// the input box so the user can type during streaming. Submitted text
    /// is buffered and sent automatically when the turn completes.
    Streaming(StreamingState),

    /// A panel or interaction popup is active, capturing all keyboard input.
    ///
    /// If entered from Streaming, the caller must save and restore
    /// `CurrentTurn` so that streaming progress is not lost.
    Modal(ModalState),

    /// Session-switching transition state.
    ///
    /// Clears the view, shows a loading indicator, and transitions to
    /// Idle once the first batch of ViewModels arrives.
    Switching(SwitchingState),
}

// ---------------------------------------------------------------------------
// Idle
// ---------------------------------------------------------------------------

/// State when the agent is not running and the user is interacting with the
/// input box.
#[derive(Debug, Default)]
pub struct IdleState {
    /// Input box state (buffer, cursor, at-mention, slash-completion,
    /// attachments, prediction).
    pub input: InputState,

    /// Vertical scroll offset in the message area.
    pub scroll_offset: u16,

    /// Last committed ViewModel snapshot from the ACP layer.
    ///
    /// Updated on `"view-commit"` events. Rendering derives the final view
    /// from this list.
    pub view: Vec<ViewModel>,

    /// Double-Esc quit tracker.
    pub double_esc_timer: Option<DoubleEscTracker>,

    /// Current position in the input-history list (None = latest entry).
    pub history_index: Option<usize>,
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

/// State when the agent is actively producing output.
///
/// The `view` field holds the last committed snapshot (same data that was in
/// Idle). The `current_turn` field accumulates incremental text and tool cards
/// for the in-progress turn. Rendering concatenates `view + current_turn`.
///
/// When `"view-commit"` arrives, the state machine replaces `view` with the
/// new full snapshot and clears `current_turn`.
/// When `"turn-done"` arrives, the state machine transitions back to Idle.
#[derive(Debug)]
pub struct StreamingState {
    /// Incremental data for the in-progress agent turn.
    pub current_turn: CurrentTurn,

    /// Input box -- user can type during streaming.
    pub input: InputState,

    /// Last committed ViewModel snapshot.
    pub view: Vec<ViewModel>,

    /// Vertical scroll offset in the message area.
    pub scroll_offset: u16,
}

// ---------------------------------------------------------------------------
// Modal
// ---------------------------------------------------------------------------

/// A panel or interaction popup is active, capturing all keyboard input.
///
/// **Panels** (14 types) implement `PanelState` and are user-initiated
/// (shortcut or `/command`). Panels are half-screen; the message area
/// remains scrollable behind them.
///
/// **Interactions** (4 types) implement `Handler` and are agent-initiated
/// (HITL approval, AskUser, Rewind, OAuth). They are centered popups
/// that fully overlay the message area.
///
/// Entering Modal from Streaming must preserve `CurrentTurn` so that
/// streaming progress is not lost when the popup closes.
#[derive(Debug)]
pub enum ModalState {
    /// A user-initiated panel (config, model selector, etc.).
    Panel(Box<dyn PanelState>),

    /// An agent-initiated interaction popup (HITL, AskUser, etc.).
    Interaction(Box<dyn Handler>),
}

// ---------------------------------------------------------------------------
// Switching
// ---------------------------------------------------------------------------

/// Session-switching transition state.
///
/// The view is cleared and a loading indicator is shown. When the first
/// batch of ViewModels for the new session arrives (via `"view-commit"`),
/// the state machine transitions to `Idle`.
#[derive(Debug)]
pub struct SwitchingState {
    /// Empty or partial ViewModel list during transition.
    pub view: Vec<ViewModel>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_idle_state_fields() {
        let idle = IdleState {
            input: InputState::default(),
            scroll_offset: 0,
            view: vec![],
            double_esc_timer: Some(DoubleEscTracker::new()),
            history_index: None,
        };
        assert!(idle.view.is_empty());
        assert!(idle.history_index.is_none());
        assert!(idle.double_esc_timer.is_some());
    }

    #[test]
    fn test_streaming_state_fields() {
        let current_turn = CurrentTurn {
            text: "hello".into(),
            active: true,
            ..Default::default()
        };
        let streaming = StreamingState {
            current_turn,
            input: InputState::default(),
            view: vec![],
            scroll_offset: 0,
        };
        assert_eq!(streaming.current_turn.text, "hello");
        assert!(streaming.current_turn.active);
    }

    #[test]
    fn test_modal_state_panel_variant() {
        // We can't construct a real PanelState without a concrete impl,
        // but we can verify the variant exists by matching.
        fn _assert_variant(state: &ModalState) -> bool {
            matches!(state, ModalState::Panel(_))
        }
        // Compile-time check only.
        let _ = _assert_variant;
    }

    #[test]
    fn test_modal_state_interaction_variant() {
        fn _assert_variant(state: &ModalState) -> bool {
            matches!(state, ModalState::Interaction(_))
        }
        let _ = _assert_variant;
    }

    #[test]
    fn test_switching_state_fields() {
        let switching = SwitchingState { view: vec![] };
        assert!(switching.view.is_empty());
    }

    #[test]
    fn test_double_esc_tracker_single_press() {
        let mut tracker = DoubleEscTracker::new();
        assert!(!tracker.press_esc()); // first press -- no quit
        assert!(tracker.first_press_at.is_some());
    }

    #[test]
    fn test_double_esc_tracker_double_press_quick() {
        let mut tracker = DoubleEscTracker::new();
        tracker.press_esc(); // first press
        assert!(tracker.press_esc()); // second press within threshold -> quit
        assert!(tracker.first_press_at.is_none()); // reset after detection
    }

    #[test]
    fn test_double_esc_tracker_default() {
        let tracker = DoubleEscTracker::default();
        assert!(tracker.first_press_at.is_none());
    }

    #[test]
    fn test_state_enum_variants() {
        let idle = State::Idle(IdleState {
            input: InputState::default(),
            scroll_offset: 0,
            view: vec![],
            double_esc_timer: None,
            history_index: None,
        });
        assert!(matches!(idle, State::Idle(_)));

        let streaming = State::Streaming(StreamingState {
            current_turn: CurrentTurn::default(),
            input: InputState::default(),
            view: vec![],
            scroll_offset: 0,
        });
        assert!(matches!(streaming, State::Streaming(_)));

        let switching = State::Switching(SwitchingState { view: vec![] });
        assert!(matches!(switching, State::Switching(_)));
    }
}
