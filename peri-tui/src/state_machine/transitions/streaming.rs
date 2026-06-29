//! Streaming-state transition: `(StreamingState, Event) -> (State, Vec<Effect>)`.
//!
//! The agent is actively producing output. Text/tool/reasoning chunks extend
//! `current_turn`; `view-commit` replaces the base view and resets the turn;
//! `turn-done` transitions back to Idle.
//!
//! Reference: `docs/design/peri-tui-architecture.md` section 8.6.

use peri_acp_types::event_data::{TextChunk, ToolEnded, ToolStarted};

use super::super::current_turn::ToolCardAccumulator;
use super::super::event::{AcpEventData, Event};
use super::super::state::{IdleState, State, StreamingState};
use crate::runtime::effect::Effect;

/// Streaming-state transition entry point.
pub fn handle(mut state: StreamingState, event: Event) -> (State, Vec<Effect>) {
    match event {
        // -- §4.1 Streaming (extend `current_turn`) --------------------------
        Event::AcpEvent(AcpEventData::TextChunk(TextChunk { text, .. })) => {
            state.current_turn.append_text(&text);
            (State::Streaming(state), vec![Effect::Render])
        }

        Event::AcpEvent(AcpEventData::ReasoningChunk(rc)) => {
            state.current_turn.append_reasoning(&rc.text);
            (State::Streaming(state), vec![Effect::Render])
        }

        Event::AcpEvent(AcpEventData::ToolStarted(ToolStarted {
            tool_id,
            tool_name,
            input_summary,
            ..
        })) => {
            state.current_turn.start_tool(ToolCardAccumulator::new(
                tool_id,
                tool_name,
                input_summary,
            ));
            (State::Streaming(state), vec![Effect::Render])
        }

        Event::AcpEvent(AcpEventData::ToolEnded(ToolEnded {
            tool_id,
            output_summary,
            is_error,
            ..
        })) => {
            state
                .current_turn
                .end_tool(&tool_id, output_summary, is_error);
            (State::Streaming(state), vec![Effect::Render])
        }

        // -- §4.2 Boundary ---------------------------------------------------
        Event::AcpEvent(AcpEventData::ViewCommit(vc)) => {
            // Full-snapshot replacement semantics (CLAUDE.md P2-C):
            // base view becomes the committed list, current_turn is reset.
            state.view = vc.view_models;
            state.current_turn = Default::default();
            (State::Streaming(state), vec![Effect::Render])
        }

        Event::AcpEvent(AcpEventData::TurnDone) => {
            // Streaming -> Idle. The buffered input is carried over so the
            // user's in-progress typing is not lost.
            let idle = state.into_idle();
            (State::Idle(idle), Vec::new())
        }

        Event::AcpEvent(AcpEventData::TurnInterrupted(_)) => {
            // Treat like turn-done: stop streaming, return to Idle.
            state.current_turn.deactivate();
            let idle = state.into_idle();
            (State::Idle(idle), Vec::new())
        }

        // -- §4.3 Status (no message-area change -- drop in P2 stub) --------
        Event::AcpEvent(AcpEventData::TokenUsage(_))
        | Event::AcpEvent(AcpEventData::ToolCount(_))
        | Event::AcpEvent(AcpEventData::Progress(_))
        | Event::AcpEvent(AcpEventData::BudgetWarning(_))
        | Event::AcpEvent(AcpEventData::SystemNotification(_)) => {
            // Status-bar only -- no Streaming-state mutation in P2.
            (State::Streaming(state), Vec::new())
        }

        // -- §4.4 Input assist ----------------------------------------------
        Event::AcpEvent(AcpEventData::Prediction(p)) => {
            state.input.prediction = Some(p.text);
            (State::Streaming(state), vec![Effect::Render])
        }

        Event::AcpEvent(AcpEventData::FileSuggestions(_)) => {
            // @-mention popup is driven by Idle; in Streaming we just drop.
            (State::Streaming(state), Vec::new())
        }

        // -- §4.5 Interaction requests (transition to Modal) ----------------
        Event::AcpEvent(AcpEventData::HitlPending(_))
        | Event::AcpEvent(AcpEventData::AskUser(_))
        | Event::AcpEvent(AcpEventData::RewindPreview(_))
        | Event::AcpEvent(AcpEventData::OauthNeeded(_)) => {
            // P3 will build a real Handler from the payload and enter Modal.
            // For P2 we just keep streaming.
            (State::Streaming(state), Vec::new())
        }

        // -- §4.6 Structure --------------------------------------------------
        Event::AcpEvent(AcpEventData::SubagentStarted(_))
        | Event::AcpEvent(AcpEventData::SubagentStopped(_)) => {
            // Sub-agent grouping is rendered from ViewCommit in P5.
            (State::Streaming(state), Vec::new())
        }

        Event::AcpEvent(AcpEventData::Unknown { .. }) => {
            // Forward-compat: silently ignore unknown events.
            (State::Streaming(state), Vec::new())
        }

        // -- Tick: advance spinner -------------------------------------------
        Event::Tick => {
            state.current_turn.advance_spinner();
            (State::Streaming(state), vec![Effect::Render])
        }

        // -- Other terminal events -------------------------------------------
        Event::Resize { .. } | Event::Mouse(_) => (State::Streaming(state), vec![Effect::Render]),

        Event::Key(_) | Event::Paste(_) => {
            // P3 will fully wire input editing. For P2 we accept and re-render
            // so the user can see typed text (input is preserved as-is).
            (State::Streaming(state), Vec::new())
        }

        Event::AcpDisconnected | Event::SessionLoaded { .. } | Event::Shutdown => {
            // System signals handled by main loop; Streaming state unchanged.
            (State::Streaming(state), Vec::new())
        }
    }
}

impl StreamingState {
    /// Transition helper: collapse Streaming into Idle, carrying over the
    /// input buffer / view / scroll so the user's context is preserved.
    pub fn into_idle(self) -> IdleState {
        IdleState {
            input: self.input,
            scroll_offset: self.scroll_offset,
            view: self.view,
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
    use crate::state_machine::current_turn::CurrentTurn;
    use crate::state_machine::input::InputState;

    fn make_state() -> StreamingState {
        StreamingState {
            current_turn: CurrentTurn::new(),
            input: InputState::default(),
            view: vec![],
            scroll_offset: 0,
        }
    }

    #[test]
    fn test_text_chunk_appends_and_renders() {
        let state = make_state();
        let (next, effects) = handle(
            state,
            Event::AcpEvent(AcpEventData::TextChunk(TextChunk {
                text: "hello".into(),
                agent_id: None,
            })),
        );
        match next {
            State::Streaming(s) => {
                assert_eq!(s.current_turn.text, "hello");
                assert!(s.current_turn.active);
            }
            _ => panic!("expected Streaming"),
        }
        assert_eq!(effects, vec![Effect::Render]);
    }

    #[test]
    fn test_tick_advances_spinner_and_renders() {
        let state = make_state();
        let (next, effects) = handle(state, Event::Tick);
        match next {
            State::Streaming(s) => assert_eq!(s.current_turn.spinner_frame, 1),
            _ => panic!("expected Streaming"),
        }
        assert_eq!(effects, vec![Effect::Render]);
    }

    #[test]
    fn test_turn_done_transitions_to_idle() {
        let mut state = make_state();
        state.input.insert_str("typed during streaming");
        let (next, _effects) = handle(state, Event::AcpEvent(AcpEventData::TurnDone));
        match next {
            State::Idle(idle) => {
                // Input is preserved across Streaming -> Idle.
                assert_eq!(idle.input.text(), "typed during streaming");
            }
            _ => panic!("expected Idle after TurnDone"),
        }
    }

    #[test]
    fn test_unknown_event_is_noop() {
        let state = make_state();
        let (next, effects) = handle(
            state,
            Event::AcpEvent(AcpEventData::Unknown {
                event: "future-event".into(),
                data: serde_json::json!({}),
            }),
        );
        assert!(matches!(next, State::Streaming(_)));
        assert!(effects.is_empty());
    }

    #[test]
    fn test_status_events_are_silent() {
        use peri_acp_types::event_data::TokenUsage;
        let state = make_state();
        let (next, effects) = handle(
            state,
            Event::AcpEvent(AcpEventData::TokenUsage(TokenUsage {
                input: 10,
                output: 5,
            })),
        );
        assert!(matches!(next, State::Streaming(_)));
        assert!(effects.is_empty());
    }
}
