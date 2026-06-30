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
pub fn handle(mut state: SwitchingState, event: Event) -> (State, Vec<Effect>) {
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

        // -- System signals ---------------------------------------------------
        Event::AcpDisconnected => (
            State::Switching(state),
            vec![
                Effect::PushSystemNote(
                    "ACP connection lost during session switch. The UI may be stale.".to_string(),
                ),
                Effect::Render,
            ],
        ),
        Event::Shutdown => (State::Switching(state), vec![Effect::Quit]),

        // -- Phase 2.4: push system note into state.view (v2 source) -------
        Event::PushSystemNote(text) => {
            state
                .view
                .push(peri_acp_types::view_model::ViewModel::SystemNote(
                    peri_acp_types::view_model::SystemNoteData {
                        text,
                        level: peri_acp_types::view_model::NoteLevel::Info,
                    },
                ));
            (State::Switching(state), vec![Effect::Render])
        }

        // -- Cron #24 P1 #2: push user bubble into state.view (v2 source) ----
        // 切换会话期间到达的 AskUser 答案路由到 state.view（虽然实际场景罕见，
        // 但保持 4 个状态变体的 PushUserBubble 处理一致）。
        Event::PushUserBubble(text) => {
            state
                .view
                .push(peri_acp_types::view_model::ViewModel::UserBubble(
                    peri_acp_types::view_model::UserBubbleData { text },
                ));
            (State::Switching(state), vec![Effect::Render])
        }

        // -- Drop everything else --------------------------------------------
        Event::Key(_) | Event::Paste(_) | Event::AcpEvent(_) => {
            (State::Switching(state), Vec::new())
        }
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

    #[test]
    fn test_shutdown_emits_quit_in_switching() {
        // 用户会话切换期间 Ctrl+C 应能退出，之前会静默丢弃。
        let state = SwitchingState { view: vec![] };
        let (next, effects) = handle(state, Event::Shutdown);
        assert!(matches!(next, State::Switching(_)));
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Quit)),
            "Shutdown in Switching should emit Quit"
        );
    }

    #[test]
    fn test_acp_disconnected_emits_system_note_in_switching() {
        let state = SwitchingState { view: vec![] };
        let (next, effects) = handle(state, Event::AcpDisconnected);
        assert!(matches!(next, State::Switching(_)));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::PushSystemNote(_))),
            "AcpDisconnected in Switching should emit PushSystemNote"
        );
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Render)),
            "AcpDisconnected in Switching should emit Render"
        );
    }

    // ── Cron #24 P1 #2: PushUserBubble 推送到 state.view ─────────────────

    #[test]
    fn test_push_userbubble_adds_to_state_view_in_switching() {
        // Cron #24 P1 #2: 切换会话期间到达的 AskUser 答案（罕见但保持 4 状态
        // 处理一致性）必须追加到 state.view 并 emit Render。
        let state = SwitchingState { view: vec![] };
        let (next, effects) = handle(state, Event::PushUserBubble("answer".into()));
        match next {
            State::Switching(s) => {
                assert_eq!(s.view.len(), 1);
                match &s.view[0] {
                    ViewModel::UserBubble(d) => {
                        assert_eq!(d.text, "answer");
                    }
                    other => panic!("expected UserBubble, got {other:?}"),
                }
            }
            other => panic!("expected Switching, got {other:?}"),
        }
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Render)),
            "PushUserBubble in Switching should emit Render"
        );
    }
}
