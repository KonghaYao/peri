//! Streaming-state transition: `(StreamingState, Event) -> (State, Vec<Effect>)`.
//!
//! The agent is actively producing output. Text/tool/reasoning chunks extend
//! `current_turn`; `view-commit` replaces the base view and resets the turn;
//! `turn-done` transitions back to Idle.
//!
//! Reference: `docs/design/peri-tui-architecture.md` section 8.6.

use peri_acp_types::event_data::{TextChunk, ToolEnded, ToolStarted};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::current_turn::ToolCardAccumulator;
use super::super::event::{AcpEventData, Event};
use super::super::handlers::{AskUserHandler, HitlHandler, OauthHandler, RewindHandler};
use super::super::state::{IdleState, State, StreamingState};
use super::enter_modal_from_streaming;
use crate::app::panel_types::PanelKind;
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
        // Construct the matching Handler and enter Modal. The handler is
        // wrapped in `ModalKind::Interaction`. `saved_current_turn = Some(..)`
        // preserves the in-progress streaming output so it is not lost while
        // the popup is open — ClosePanel will restore Streaming afterwards.
        Event::AcpEvent(AcpEventData::HitlPending(p)) => {
            enter_modal_from_streaming(state, Box::new(HitlHandler::new(p)))
        }
        Event::AcpEvent(AcpEventData::AskUser(f)) => {
            enter_modal_from_streaming(state, Box::new(AskUserHandler::new(f)))
        }
        Event::AcpEvent(AcpEventData::RewindPreview(rp)) => {
            enter_modal_from_streaming(state, Box::new(RewindHandler::new(rp)))
        }
        Event::AcpEvent(AcpEventData::OauthNeeded(o)) => {
            enter_modal_from_streaming(state, Box::new(OauthHandler::new(o)))
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

        // -- Key events: Ctrl-shortcuts mirror Idle (panels/global toggles
        //    remain accessible while the agent streams). Plain chars and
        //    navigation keys fall through to the keyboard fallback (textarea
        //    remains the single source of truth for input editing).
        Event::Key(key) => handle_key(state, key),

        // -- Paste: routed by main_loop. Streaming re-renders to show the
        //    pasted text in the input box.
        Event::Paste(_) => (State::Streaming(state), vec![Effect::Render]),

        // -- System events --------------------------------------------------
        Event::AcpDisconnected | Event::SessionLoaded { .. } => {
            (State::Streaming(state), vec![Effect::Render])
        }

        Event::Shutdown => (State::Streaming(state), vec![Effect::Quit]),
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

/// Key dispatch for the Streaming state.
///
/// Mirrors the Idle Ctrl-shortcut set (panels + global toggles) so users can
/// open the Model panel, cycle provider/model, focus the bg bar, toggle diff,
/// or cycle permission mode while the agent is producing output. The SM
/// emits the same `Effect`s as Idle; `main_loop` decides state transitions
/// (e.g. `Effect::OpenPanel` from Streaming enters Modal with
/// `saved_current_turn = Some(...)` so streaming progress is preserved).
///
/// Plain chars, navigation keys, and Backspace/Delete are NOT handled here —
/// they flow through `is_sm_handled_shortcut` (returns false for them) to the
/// keyboard fallback, which mutates the textarea widget. The 2b sync then
/// reflects textarea state back into `StreamingState.input`.
fn handle_key(state: StreamingState, key: KeyEvent) -> (State, Vec<Effect>) {
    // Ctrl+Char: global shortcuts (same set as Idle).
    if key.modifiers.intersects(KeyModifiers::CONTROL) {
        if let KeyCode::Char(c) = key.code {
            // Ctrl+Shift+T: cycle provider (check SHIFT before per-char match).
            if c == 't' && key.modifiers.intersects(KeyModifiers::SHIFT) {
                return (
                    State::Streaming(state),
                    vec![Effect::CycleProvider, Effect::Render],
                );
            }
            match c {
                // Ctrl+T: cycle model alias.
                't' => (
                    State::Streaming(state),
                    vec![Effect::CycleModel, Effect::Render],
                ),
                // Ctrl+B: focus background agent bar.
                'b' => (
                    State::Streaming(state),
                    vec![Effect::FocusBgBar, Effect::Render],
                ),
                // Ctrl+O: toggle inline diff.
                'o' => (
                    State::Streaming(state),
                    vec![Effect::ToggleDiff, Effect::Render],
                ),
                // Ctrl+P: open Model panel.
                'p' => (
                    State::Streaming(state),
                    vec![Effect::OpenPanel(PanelKind::Model), Effect::Render],
                ),
                // Other Ctrl+<char>: render-only (typing handled by keyboard
                // fallback when is_sm_handled_shortcut returns false).
                _ => (State::Streaming(state), vec![Effect::Render]),
            }
        } else {
            // Ctrl+non-Char (e.g. Ctrl+Arrow): render-only.
            (State::Streaming(state), vec![Effect::Render])
        }
    } else {
        match key.code {
            // BackTab: cycle permission mode.
            KeyCode::BackTab => (
                State::Streaming(state),
                vec![Effect::CyclePermissionMode, Effect::Render],
            ),
            // All other keys (plain chars, navigation, Backspace, Enter, Esc):
            // keyboard fallback owns textarea editing; SM re-renders to make
            // typed text visible.
            _ => (State::Streaming(state), vec![Effect::Render]),
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

    // ── New tests for streaming key/system events ──────────────────────────

    #[test]
    fn test_key_event_emits_render_in_streaming() {
        // 用户在 agent 运行期间可以打字；streaming 状态应触发重绘以显示输入。
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let state = make_state();
        let (next, effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
        );
        assert!(matches!(next, State::Streaming(_)));
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Render)),
            "Key in Streaming should emit Render"
        );
    }

    #[test]
    fn test_shutdown_emits_quit_in_streaming() {
        // 用户 Ctrl+C 时即使正在流式输出也应能退出。
        let state = make_state();
        let (next, effects) = handle(state, Event::Shutdown);
        assert!(matches!(next, State::Streaming(_)));
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Quit)),
            "Shutdown in Streaming should emit Quit"
        );
    }

    #[test]
    fn test_acp_disconnected_emits_render_in_streaming() {
        let state = make_state();
        let (next, effects) = handle(state, Event::AcpDisconnected);
        assert!(matches!(next, State::Streaming(_)));
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Render)),
            "AcpDisconnected in Streaming should emit Render"
        );
    }

    // ── Ctrl-shortcut parity with Idle (panels/toggles reachable) ────────

    #[test]
    fn test_ctrl_p_opens_model_panel_in_streaming() {
        // 用户在 agent 流式输出期间也应能打开 Model 面板查看当前模型。
        let state = make_state();
        let (next, effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
        );
        assert!(matches!(next, State::Streaming(_)));
        assert!(effects
            .iter()
            .any(|e| matches!(e, Effect::OpenPanel(PanelKind::Model))));
    }

    #[test]
    fn test_ctrl_t_cycles_model_in_streaming() {
        let state = make_state();
        let (next, effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL)),
        );
        assert!(matches!(next, State::Streaming(_)));
        assert!(effects.iter().any(|e| matches!(e, Effect::CycleModel)));
    }

    #[test]
    fn test_ctrl_shift_t_cycles_provider_in_streaming() {
        let state = make_state();
        let (next, effects) = handle(
            state,
            Event::Key(KeyEvent::new(
                KeyCode::Char('t'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            )),
        );
        assert!(matches!(next, State::Streaming(_)));
        assert!(effects.iter().any(|e| matches!(e, Effect::CycleProvider)));
    }

    #[test]
    fn test_ctrl_b_focuses_bg_bar_in_streaming() {
        let state = make_state();
        let (next, effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)),
        );
        assert!(matches!(next, State::Streaming(_)));
        assert!(effects.iter().any(|e| matches!(e, Effect::FocusBgBar)));
    }

    #[test]
    fn test_ctrl_o_toggles_diff_in_streaming() {
        let state = make_state();
        let (next, effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)),
        );
        assert!(matches!(next, State::Streaming(_)));
        assert!(effects.iter().any(|e| matches!(e, Effect::ToggleDiff)));
    }

    #[test]
    fn test_backtab_cycles_permission_mode_in_streaming() {
        let state = make_state();
        let (next, effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE)),
        );
        assert!(matches!(next, State::Streaming(_)));
        assert!(effects
            .iter()
            .any(|e| matches!(e, Effect::CyclePermissionMode)));
    }

    #[test]
    fn test_plain_char_in_streaming_only_renders() {
        // 普通 Char 键由 keyboard fallback 处理（textarea 编辑）；
        // SM 仅触发重绘，不修改 InputState。
        let mut state = make_state();
        state.input.insert_str("existing");
        let (next, effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
        );
        match next {
            State::Streaming(s) => {
                // SM 不修改 InputState —— textarea 是输入的唯一权威。
                assert_eq!(s.input.text(), "existing");
            }
            _ => panic!("expected Streaming"),
        }
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Render)),
            "Plain char in Streaming should emit Render"
        );
        // 不应该有副作用消费方误以为 SM 已处理输入。
        assert!(effects.iter().all(|e| matches!(e, Effect::Render)));
    }

    // ── Phase 1.3: Agent-initiated interaction requests enter Modal ──
    // 这些测试验证 Streaming 期间到达的 HITL/AskUser/Rewind/OAuth 会
    // 进入 Modal(Interaction)，并且 saved_current_turn 保留了流式进度。

    fn assert_interaction_modal_from_streaming(next: State, expected_text: &str) {
        match next {
            State::Modal(m) => {
                // Streaming source: saved_current_turn must preserve text.
                let turn = m
                    .saved_current_turn
                    .expect("Streaming→Modal must preserve saved_current_turn");
                assert_eq!(
                    turn.text, expected_text,
                    "saved_current_turn must contain streaming progress"
                );
                assert!(
                    matches!(
                        m.kind,
                        crate::state_machine::state::ModalKind::Interaction(_)
                    ),
                    "Modal kind must be Interaction"
                );
            }
            other => panic!("expected Modal after interaction request, got {:?}", other),
        }
    }

    #[test]
    fn test_hitl_pending_in_streaming_enters_modal_with_saved_turn() {
        // Streaming 期间收到 HitlPending 必须进入 Modal 且保留 current_turn。
        let mut state = make_state();
        state.current_turn.append_text("streaming-so-far");
        let (next, effects) = handle(
            state,
            Event::AcpEvent(AcpEventData::HitlPending(
                peri_acp_types::event_data::HitlPending {
                    tool_name: "Edit".into(),
                    tool_input: serde_json::json!({}),
                    batch: None,
                },
            )),
        );
        assert_interaction_modal_from_streaming(next, "streaming-so-far");
        assert!(effects.iter().any(|e| matches!(e, Effect::Render)));
    }

    #[test]
    fn test_ask_user_in_streaming_enters_modal_with_saved_turn() {
        use peri_acp_types::event_data::{AskUser, Question};
        let mut state = make_state();
        state.current_turn.append_text("partial");
        let (next, _effects) = handle(
            state,
            Event::AcpEvent(AcpEventData::AskUser(AskUser {
                questions: vec![Question {
                    id: "q1".into(),
                    header: "Pick".into(),
                    question: "Which?".into(),
                    options: vec![],
                    multi_select: false,
                }],
            })),
        );
        assert_interaction_modal_from_streaming(next, "partial");
    }

    #[test]
    fn test_rewind_preview_in_streaming_enters_modal_with_saved_turn() {
        use peri_acp_types::event_data::RewindPreview;
        let mut state = make_state();
        state.current_turn.append_text("rp-stream");
        let (next, _effects) = handle(
            state,
            Event::AcpEvent(AcpEventData::RewindPreview(RewindPreview {
                files: vec![],
                messages: vec![],
            })),
        );
        assert_interaction_modal_from_streaming(next, "rp-stream");
    }

    #[test]
    fn test_oauth_needed_in_streaming_enters_modal_with_saved_turn() {
        use peri_acp_types::event_data::OauthNeeded;
        let mut state = make_state();
        state.current_turn.append_text("oauth-stream");
        let (next, _effects) = handle(
            state,
            Event::AcpEvent(AcpEventData::OauthNeeded(OauthNeeded {
                server_name: "github-mcp".into(),
                auth_url: "https://github.com/login".into(),
            })),
        );
        assert_interaction_modal_from_streaming(next, "oauth-stream");
    }

    #[test]
    fn test_streaming_interaction_modal_preserves_input_and_scroll() {
        // 进入 Modal 时 Streaming 的 input / view / scroll 都要保留，
        // 这样关闭弹窗（ClosePanel）后能恢复 Streaming 而不丢失上下文。
        use peri_acp_types::event_data::HitlPending;
        let mut state = make_state();
        state.input.insert_str("typing during stream");
        state.scroll_offset = 4;
        state.current_turn.append_text("stream-output");
        let (next, _effects) = handle(
            state,
            Event::AcpEvent(AcpEventData::HitlPending(HitlPending {
                tool_name: "Edit".into(),
                tool_input: serde_json::json!({}),
                batch: None,
            })),
        );
        match next {
            State::Modal(m) => {
                assert_eq!(m.saved_input.text(), "typing during stream");
                assert_eq!(m.saved_scroll_offset, 4);
                assert!(m.saved_current_turn.is_some());
            }
            _ => panic!("expected Modal"),
        }
    }
}
