//! Idle-state transition: `(IdleState, Event) -> (State, Vec<Effect>)`.
//!
//! Idle holds the input box and waits for user input. Key events edit the
//! buffer; Enter submits and transitions to Streaming; Esc drives the
//! double-Esc quit tracker. Tick produces no effect (power-save). AcpEvent
//! payloads that require interaction (HitlPending / AskUser / ...) will
//! transition to Modal in P3; for P2 they are accepted as no-ops.
//!
//! Reference: `docs/design/peri-tui-architecture.md` section 8.6.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};

use super::super::current_turn::CurrentTurn;
use super::super::event::{AcpEventData, Event};
use super::super::input::{CursorPos, InputEdit};
use super::super::state::{DoubleEscTracker, IdleState, State, StreamingState};
use crate::runtime::effect::Effect;

/// Idle-state transition entry point.
pub fn handle(mut state: IdleState, event: Event) -> (State, Vec<Effect>) {
    match event {
        // -- Key events -------------------------------------------------------
        Event::Key(key) => handle_key(state, key),

        // -- Paste: routed by main_loop (setup wizard → interaction popup
        //    → legacy textarea). State machine emits PasteText + Render;
        //    main_loop handles the routing to App-level state.
        Event::Paste(text) => (
            State::Idle(state),
            vec![Effect::PasteText { text }, Effect::Render],
        ),

        // -- Tick: advance spinner + poll agent + poll workflow + render. --
        Event::Tick => (
            State::Idle(state),
            vec![
                Effect::AdvanceSpinner,
                Effect::PollAgent,
                Effect::PollWorkflow,
                Effect::Render,
            ],
        ),

        // -- Mouse / Resize --------------------------------------------------
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollDown => (State::Idle(state), vec![Effect::Scroll { delta: 3 }]),
            MouseEventKind::ScrollUp => (State::Idle(state), vec![Effect::Scroll { delta: -3 }]),
            MouseEventKind::Down(MouseButton::Left) => (
                State::Idle(state),
                vec![
                    Effect::MouseTextareaClick {
                        row: mouse.row,
                        column: mouse.column,
                    },
                    Effect::Render,
                ],
            ),
            MouseEventKind::Drag(MouseButton::Left) => (
                State::Idle(state),
                vec![
                    Effect::MouseTextareaDrag {
                        row: mouse.row,
                        column: mouse.column,
                    },
                    Effect::Render,
                ],
            ),
            MouseEventKind::Up(MouseButton::Left) => (
                State::Idle(state),
                vec![Effect::MouseRelease, Effect::Render],
            ),
            _ => (State::Idle(state), vec![Effect::Render]),
        },
        Event::Resize { .. } => (
            State::Idle(state),
            vec![Effect::ClearTextSelection, Effect::Render],
        ),

        // -- ACP events ------------------------------------------------------
        Event::AcpEvent(AcpEventData::ViewCommit(vc)) => {
            state.view = vc.view_models;
            (State::Idle(state), vec![Effect::Render])
        }

        Event::AcpEvent(AcpEventData::Prediction(p)) => {
            state.input.prediction = Some(p.text);
            (State::Idle(state), vec![Effect::Render])
        }

        Event::AcpEvent(AcpEventData::FileSuggestions(fs)) => {
            use super::super::input::AtMentionState;
            state.input.at_mention = Some(AtMentionState {
                candidates: fs.files,
                selected: 0,
            });
            (State::Idle(state), vec![Effect::Render])
        }

        // Interaction requests: P3 will enter Modal here.
        Event::AcpEvent(AcpEventData::HitlPending(_))
        | Event::AcpEvent(AcpEventData::AskUser(_))
        | Event::AcpEvent(AcpEventData::RewindPreview(_))
        | Event::AcpEvent(AcpEventData::OauthNeeded(_))
        | Event::AcpEvent(AcpEventData::TextChunk(_))
        | Event::AcpEvent(AcpEventData::ReasoningChunk(_))
        | Event::AcpEvent(AcpEventData::ToolStarted(_))
        | Event::AcpEvent(AcpEventData::ToolEnded(_))
        | Event::AcpEvent(AcpEventData::TurnDone)
        | Event::AcpEvent(AcpEventData::TurnInterrupted(_))
        | Event::AcpEvent(AcpEventData::TokenUsage(_))
        | Event::AcpEvent(AcpEventData::ToolCount(_))
        | Event::AcpEvent(AcpEventData::Progress(_))
        | Event::AcpEvent(AcpEventData::BudgetWarning(_))
        | Event::AcpEvent(AcpEventData::SystemNotification(_))
        | Event::AcpEvent(AcpEventData::SubagentStarted(_))
        | Event::AcpEvent(AcpEventData::SubagentStopped(_))
        | Event::AcpEvent(AcpEventData::Unknown { .. }) => {
            // Either out-of-band (Agent resumed on its own -> should have
            // transitioned to Streaming via main loop) or irrelevant in Idle.
            (State::Idle(state), Vec::new())
        }

        // -- System signals ---------------------------------------------------
        Event::AcpDisconnected => (
            State::Idle(state),
            vec![
                Effect::PushSystemNote(
                    "ACP connection lost. Agent responses may not arrive.".to_string(),
                ),
                Effect::Render,
            ],
        ),
        Event::SessionLoaded { .. } => (State::Idle(state), Vec::new()),
        Event::Shutdown => (State::Idle(state), vec![Effect::Quit]),
    }
}

/// Key dispatch for the Idle state.
fn handle_key(mut state: IdleState, key: KeyEvent) -> (State, Vec<Effect>) {
    match key.code {
        // -- Enter: submit ---------------------------------------------------

        // Shift+Enter / Alt+Enter inserts a newline (multi-line input).
        // Plain Enter (no modifiers, or Control) submits.
        KeyCode::Enter
            if !key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
        {
            let text = state.input.text();
            state.input.clear_buffer();

            if text.trim().is_empty() {
                // Empty submit -- no-op, stay Idle.
                return (State::Idle(state), Vec::new());
            }

            // Save into history (newest at the back).
            state.input.history.push(text.clone());
            state.history_index = None;

            let params = serde_json::json!({ "text": text });
            let effects = vec![Effect::SendToAcp {
                method: "session/prompt".into(),
                params,
            }];

            // Enter Streaming -- empty current_turn, preserved view+input.
            let streaming = StreamingState {
                current_turn: CurrentTurn::new(),
                input: std::mem::take(&mut state.input),
                view: std::mem::take(&mut state.view),
                scroll_offset: state.scroll_offset,
            };
            // Restore view+input into Idle state we drop after; Streaming owns it now.
            let _ = state; // state is consumed
            (State::Streaming(streaming), effects)
        }

        // -- Esc: double-press quit tracker ---------------------------------
        KeyCode::Esc => {
            let tracker = state
                .double_esc_timer
                .get_or_insert_with(DoubleEscTracker::new);
            if tracker.press_esc() {
                // Double Esc -> quit.
                (State::Idle(state), vec![Effect::Quit])
            } else {
                // Single press -- clear at-mention / slash popup if any.
                if state.input.at_mention.take().is_some()
                    || state.input.slash_completion.take().is_some()
                {
                    (State::Idle(state), vec![Effect::Render])
                } else {
                    (State::Idle(state), Vec::new())
                }
            }
        }

        // -- BackTab: cycle permission mode ---------------------------------
        KeyCode::BackTab => (
            State::Idle(state),
            vec![Effect::CyclePermissionMode, Effect::Render],
        ),

        // -- Printable character --------------------------------------------
        // Ctrl+<char> shortcuts are intercepted BEFORE the general Char arm.
        KeyCode::Char(c) if key.modifiers.intersects(KeyModifiers::CONTROL) => {
            // Ctrl+Shift+T: cycle provider (check SHIFT before per-char match).
            if c == 't' && key.modifiers.intersects(KeyModifiers::SHIFT) {
                return (
                    State::Idle(state),
                    vec![Effect::CycleProvider, Effect::Render],
                );
            }
            match c {
                'c' => {
                    // Ctrl+C: quit. If there's a selection, do nothing (copy
                    // is handled via other paths for now).
                    if state.input.selection.is_some() {
                        (State::Idle(state), vec![Effect::Render])
                    } else {
                        (State::Idle(state), vec![Effect::Quit])
                    }
                }
                'a' => {
                    use crate::state_machine::input::InputEdit;
                    state.input.select_all();
                    (State::Idle(state), vec![Effect::Render])
                }
                'u' => {
                    use crate::state_machine::input::InputEdit;
                    state.input.delete_line_by_head();
                    state.input.prediction = None;
                    (State::Idle(state), vec![Effect::Render])
                }
                'w' => {
                    use crate::state_machine::input::InputEdit;
                    state.input.delete_word();
                    state.input.prediction = None;
                    (State::Idle(state), vec![Effect::Render])
                }
                't' => {
                    // Ctrl+T: cycle model alias (without Shift, handled above).
                    (State::Idle(state), vec![Effect::CycleModel, Effect::Render])
                }
                'b' => {
                    // Ctrl+B: focus background agent bar.
                    (State::Idle(state), vec![Effect::FocusBgBar, Effect::Render])
                }
                'o' => {
                    // Ctrl+O: toggle inline diff.
                    (State::Idle(state), vec![Effect::ToggleDiff, Effect::Render])
                }
                // Other Ctrl+<char> combos pass through (P3 will dispatch more).
                _ => (State::Idle(state), Vec::new()),
            }
        }

        KeyCode::Char(c) => {
            state.input.insert_str(&c.to_string());
            // Typing invalidates the prediction.
            state.input.prediction = None;
            (State::Idle(state), vec![Effect::Render])
        }

        // -- Backspace -------------------------------------------------------
        KeyCode::Backspace => {
            let before_len = state.input.text().len();
            state.input.backspace();
            let after_len = state.input.text().len();
            if after_len < before_len {
                state.input.prediction = None;
                return (State::Idle(state), vec![Effect::Render]);
            }
            (State::Idle(state), Vec::new())
        }

        // -- Left / Right arrow: move cursor (char-level) -------------------
        KeyCode::Left => {
            state.input.move_cursor_left(false);
            (State::Idle(state), vec![Effect::Render])
        }

        KeyCode::Right => {
            state.input.move_cursor_right(false);
            (State::Idle(state), vec![Effect::Render])
        }

        // -- Home / End: line navigation -----------------------------------
        KeyCode::Home => {
            state
                .input
                .move_cursor_home(key.modifiers.intersects(KeyModifiers::SHIFT));
            (State::Idle(state), vec![Effect::Render])
        }

        KeyCode::End => {
            state
                .input
                .move_cursor_end(key.modifiers.intersects(KeyModifiers::SHIFT));
            (State::Idle(state), vec![Effect::Render])
        }

        // -- Delete: forward delete -----------------------------------------
        KeyCode::Delete => {
            use crate::state_machine::input::InputEdit;
            // Simulate forward-delete: move right, then backspace.
            state.input.move_cursor_right(false);
            state.input.backspace();
            state.input.prediction = None;
            (State::Idle(state), vec![Effect::Render])
        }

        // -- Up / Down: history navigation ----------------------------------
        KeyCode::Up => {
            if state.input.history.is_empty() {
                return (State::Idle(state), Vec::new());
            }
            let idx = match state.history_index {
                Some(i) if i > 0 => i - 1,
                Some(_) => 0,
                None => state.input.history.len().saturating_sub(1),
            };
            state.history_index = Some(idx);
            state.input.lines = vec![state.input.history[idx].clone()];
            state.input.cursor = CursorPos::new(0, state.input.lines[0].len());
            (State::Idle(state), vec![Effect::Render])
        }

        KeyCode::Down => {
            let idx = match state.history_index {
                Some(i) if i + 1 < state.input.history.len() => i + 1,
                Some(_) => {
                    // Past the newest entry: restore original (empty).
                    state.history_index = None;
                    state.input.lines = vec![String::new()];
                    state.input.cursor = CursorPos::default();
                    return (State::Idle(state), vec![Effect::Render]);
                }
                None => return (State::Idle(state), Vec::new()),
            };
            state.history_index = Some(idx);
            state.input.lines = vec![state.input.history[idx].clone()];
            state.input.cursor = CursorPos::new(0, state.input.lines[0].len());
            (State::Idle(state), vec![Effect::Render])
        }

        // -- Everything else -------------------------------------------------
        _ => (State::Idle(state), Vec::new()),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_machine::input::{CursorPos, InputState};

    fn make_state() -> IdleState {
        IdleState {
            input: InputState::default(),
            scroll_offset: 0,
            view: vec![],
            double_esc_timer: None,
            history_index: None,
        }
    }

    fn char_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn test_char_key_appends_to_buffer() {
        let state = make_state();
        let (next, _effects) = handle(state, Event::Key(char_key('a')));
        match next {
            State::Idle(idle) => {
                assert_eq!(idle.input.text(), "a");
                assert_eq!(idle.input.cursor, CursorPos::new(0, 1));
            }
            _ => panic!("expected Idle"),
        }
    }

    #[test]
    fn test_typing_clears_prediction() {
        let mut state = make_state();
        state.input.prediction = Some("ghost".into());
        let (next, _effects) = handle(state, Event::Key(char_key('x')));
        match next {
            State::Idle(idle) => assert!(idle.input.prediction.is_none()),
            _ => panic!("expected Idle"),
        }
    }

    #[test]
    fn test_tick_produces_spinner_poll_and_workflow_in_idle() {
        let state = make_state();
        let (_next, effects) = handle(state, Event::Tick);
        assert!(effects.iter().any(|e| matches!(e, Effect::AdvanceSpinner)));
        assert!(effects.iter().any(|e| matches!(e, Effect::PollAgent)));
        assert!(effects.iter().any(|e| matches!(e, Effect::PollWorkflow)));
        assert!(effects.iter().any(|e| matches!(e, Effect::Render)));
    }

    #[test]
    fn test_empty_enter_does_not_submit() {
        let state = make_state();
        let (next, effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        );
        assert!(matches!(next, State::Idle(_)));
        assert!(effects.is_empty());
    }

    #[test]
    fn test_enter_submits_and_transitions_to_streaming() {
        let mut state = make_state();
        state.input.insert_str("hello world");
        let (next, effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        );
        assert!(matches!(next, State::Streaming(_)));
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::SendToAcp { method, params } => {
                assert_eq!(method, "session/prompt");
                assert_eq!(params["text"], "hello world");
            }
            _ => panic!("expected SendToAcp effect"),
        }
    }

    #[test]
    fn test_double_esc_quits() {
        // Two Esc presses with no time gap -> quit on second press.
        let state = make_state();
        let (next, _e1) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        );
        // Unwrap the IdleState so we can call `handle` again.
        let idle_again = match next {
            State::Idle(s) => s,
            _ => panic!("expected Idle after first Esc"),
        };
        let (_state, e2) = handle(
            idle_again,
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        );
        assert!(e2.iter().any(|e| matches!(e, Effect::Quit)));
    }

    #[test]
    fn test_single_esc_does_not_quit() {
        let state = make_state();
        let (_next, effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        );
        assert!(!effects.iter().any(|e| matches!(e, Effect::Quit)));
    }

    #[test]
    fn test_backspace_deletes_one_char() {
        let mut state = make_state();
        state.input.insert_str("abc");
        let (next, _effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
        );
        match next {
            State::Idle(idle) => {
                assert_eq!(idle.input.text(), "ab");
                assert_eq!(idle.input.cursor, CursorPos::new(0, 2));
            }
            _ => panic!("expected Idle"),
        }
    }

    #[test]
    fn test_paste_emits_paste_text_effect() {
        let mut state = make_state();
        state.input.insert_str("hello");
        state.input.cursor = CursorPos::new(0, 2); // between 'e' and 'l'
        let (next, effects) = handle(state, Event::Paste("XX".into()));
        // State machine no longer inserts paste into its own buffer; routing
        // is delegated to main_loop via PasteText effect.
        match next {
            State::Idle(idle) => {
                // Input is unchanged — paste routing is handled by main_loop.
                assert_eq!(idle.input.text(), "hello");
                assert_eq!(idle.input.cursor, CursorPos::new(0, 2));
            }
            _ => panic!("expected Idle"),
        }
        // Verify PasteText effect is emitted with the correct text.
        let paste_eff = effects.iter().find_map(|e| match e {
            Effect::PasteText { text } => Some(text.as_str()),
            _ => None,
        });
        assert_eq!(paste_eff, Some("XX"), "PasteText effect should carry 'XX'");
        assert!(effects.iter().any(|e| matches!(e, Effect::Render)));
    }

    #[test]
    fn test_view_commit_updates_view() {
        use peri_acp_types::event_data::ViewCommit;
        use peri_acp_types::view_model::{UserBubbleData, ViewModel};
        let state = make_state();
        let (next, _effects) = handle(
            state,
            Event::AcpEvent(AcpEventData::ViewCommit(ViewCommit {
                view_models: vec![ViewModel::UserBubble(UserBubbleData {
                    text: "committed".into(),
                })],
            })),
        );
        match next {
            State::Idle(idle) => assert_eq!(idle.view.len(), 1),
            _ => panic!("expected Idle"),
        }
    }

    #[test]
    fn test_ctrl_char_is_not_buffer_input() {
        // Use Ctrl+P (unhandled) — buffer stays empty.
        let state = make_state();
        let (next, _effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
        );
        match next {
            State::Idle(idle) => {
                assert!(idle.input.text().is_empty());
            }
            _ => panic!("expected Idle"),
        }
    }

    #[test]
    fn test_backtab_cycles_permission_mode() {
        let state = make_state();
        let (_next, effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE)),
        );
        assert!(effects
            .iter()
            .any(|e| matches!(e, Effect::CyclePermissionMode)));
        assert!(effects.iter().any(|e| matches!(e, Effect::Render)));
    }

    #[test]
    fn test_ctrl_t_cycles_model() {
        let state = make_state();
        let (_next, effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL)),
        );
        assert!(effects.iter().any(|e| matches!(e, Effect::CycleModel)));
        assert!(effects.iter().any(|e| matches!(e, Effect::Render)));
    }

    #[test]
    fn test_ctrl_shift_t_cycles_provider() {
        let state = make_state();
        let (_next, effects) = handle(
            state,
            Event::Key(KeyEvent::new(
                KeyCode::Char('t'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            )),
        );
        assert!(effects.iter().any(|e| matches!(e, Effect::CycleProvider)));
        assert!(effects.iter().any(|e| matches!(e, Effect::Render)));
    }

    #[test]
    fn test_ctrl_b_focuses_bg_bar() {
        let state = make_state();
        let (_next, effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)),
        );
        assert!(effects.iter().any(|e| matches!(e, Effect::FocusBgBar)));
        assert!(effects.iter().any(|e| matches!(e, Effect::Render)));
    }

    #[test]
    fn test_ctrl_o_toggles_diff() {
        let state = make_state();
        let (_next, effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)),
        );
        assert!(effects.iter().any(|e| matches!(e, Effect::ToggleDiff)));
        assert!(effects.iter().any(|e| matches!(e, Effect::Render)));
    }

    #[test]
    fn test_resize_clears_text_selection() {
        let state = make_state();
        let (_next, effects) = handle(
            state,
            Event::Resize {
                width: 80,
                height: 24,
            },
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::ClearTextSelection)),
            "Resize in Idle should emit ClearTextSelection"
        );
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Render)),
            "Resize in Idle should emit Render"
        );
    }

    #[test]
    fn test_acp_disconnected_pushes_system_note() {
        let state = make_state();
        let (_next, effects) = handle(state, Event::AcpDisconnected);
        assert!(
            effects.iter().any(
                |e| matches!(e, Effect::PushSystemNote(msg) if msg.contains("ACP connection lost"))
            ),
            "AcpDisconnected in Idle should push a system note about lost connection"
        );
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Render)),
            "AcpDisconnected in Idle should emit Render"
        );
    }

    // ── Mouse tests ──────────────────────────────────────────────────────

    fn make_mouse(
        kind: MouseEventKind,
        row: u16,
        column: u16,
    ) -> ratatui::crossterm::event::MouseEvent {
        ratatui::crossterm::event::MouseEvent {
            kind,
            row,
            column,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn test_mouse_left_click_emits_textarea_click() {
        let state = make_state();
        let mouse = make_mouse(MouseEventKind::Down(MouseButton::Left), 10, 5);
        let (_next, effects) = handle(state, Event::Mouse(mouse));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::MouseTextareaClick { row: 10, column: 5 })),
            "Left click should emit MouseTextareaClick with coordinates"
        );
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Render)),
            "Left click should also emit Render"
        );
    }

    #[test]
    fn test_mouse_drag_emits_textarea_drag() {
        let state = make_state();
        let mouse = make_mouse(MouseEventKind::Drag(MouseButton::Left), 12, 8);
        let (_next, effects) = handle(state, Event::Mouse(mouse));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::MouseTextareaDrag { row: 12, column: 8 })),
            "Left drag should emit MouseTextareaDrag with coordinates"
        );
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Render)),
            "Left drag should also emit Render"
        );
    }

    #[test]
    fn test_mouse_up_emits_release() {
        let state = make_state();
        let mouse = make_mouse(MouseEventKind::Up(MouseButton::Left), 10, 5);
        let (_next, effects) = handle(state, Event::Mouse(mouse));
        assert!(
            effects.iter().any(|e| matches!(e, Effect::MouseRelease)),
            "Mouse up should emit MouseRelease"
        );
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Render)),
            "Mouse up should also emit Render"
        );
    }
}
