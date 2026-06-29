//! Switching-state transition: `(SwitchingState, Event) -> (State, Vec<Effect>)`.
//!
//! Session-switching transition state. The view is cleared and a loading
//! indicator is shown. When the first `"view-commit"` arrives, the state
//! transitions to Idle with the new ViewModel list.
//!
//! Reference: `docs/design/peri-tui-architecture.md` section 8.6.

use super::super::event::{AcpEventData, Event};
use super::super::state::{IdleState, InputState, State, SwitchingState};
use crate::runtime::effect::Effect;

/// Switching-state transition entry point.
pub fn handle(state: SwitchingState, event: Event) -> (State, Vec<Effect>) {
    match event {
        // -- view-commit: adopt snapshot, transition to Idle ----------------
        Event::AcpEvent(AcpEventData::ViewCommit(vc)) => {
            let idle = state.into_idle(vc.view_models);
            (State::Idle(idle), vec![Effect::Render])
        }

        // -- SessionLoaded (alt entry): stay in Switching until first commit -
        Event::SessionLoaded { .. } => (State::Switching(state), Vec::new()),

        // -- Tick: re-render for loading-indicator animation -----------------
        Event::Tick => (State::Switching(state), vec![Effect::Render]),

        // -- Mouse / Resize: re-render ---------------------------------------
        Event::Mouse(_) | Event::Resize { .. } => (State::Switching(state), vec![Effect::Render]),

        // -- Drop everything else --------------------------------------------
        Event::Key(_)
        | Event::Paste(_)
        | Event::AcpEvent(_)
        | Event::AcpDisconnected
        | Event::Shutdown => (State::Switching(state), Vec::new()),
    }
}

impl SwitchingState {
    /// Transition helper: collapse Switching into Idle with the new
    /// ViewModel list. Input state is fresh (empty buffer, empty history).
    pub fn into_idle(self, view_models: Vec<peri_acp_types::view_model::ViewModel>) -> IdleState {
        let _ = self; // explicit drop of the switching-state shell
        IdleState {
            input: InputState::default(),
            scroll_offset: 0,
            view: view_models,
            double_esc_timer: None,
            history_index: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp_types::event_data::ViewCommit;
    use peri_acp_types::view_model::{UserBubbleData, ViewModel};

    #[test]
    fn test_tick_renders_for_loading_indicator() {
        let state = SwitchingState { view: vec![] };
        let (_next, effects) = handle(state, Event::Tick);
        assert_eq!(effects, vec![Effect::Render]);
    }

    #[test]
    fn test_view_commit_transitions_to_idle() {
        let state = SwitchingState { view: vec![] };
        let (next, _effects) = handle(
            state,
            Event::AcpEvent(AcpEventData::ViewCommit(ViewCommit {
                view_models: vec![ViewModel::UserBubble(UserBubbleData {
                    text: "session data".into(),
                })],
            })),
        );
        match next {
            State::Idle(idle) => {
                assert_eq!(idle.view.len(), 1);
                assert!(idle.input.text().is_empty());
            }
            _ => panic!("expected Idle after ViewCommit"),
        }
    }

    #[test]
    fn test_keys_are_dropped_during_switching() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let state = SwitchingState { view: vec![] };
        let (next, effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
        );
        assert!(matches!(next, State::Switching(_)));
        assert!(effects.is_empty());
    }
}
