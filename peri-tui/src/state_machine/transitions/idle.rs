//! Idle-state transition: `(IdleState, Event) -> (State, Vec<Effect>)`.
//!
//! Idle holds the input box and waits for user input. Key events edit the
//! buffer; Enter submits and transitions to Streaming; Esc drives the
//! double-Esc quit tracker. Tick produces no effect (power-save). AcpEvent
//! payloads that require interaction (HitlPending / AskUser / ...) will
//! transition to Modal in P3; for P2 they are accepted as no-ops.
//!
//! Reference: `docs/design/peri-tui-architecture.md` section 8.6.
//!
//! # Input editing — textarea is the single source of truth
//!
//! The SM **does not** handle text-editing keys (Backspace/Delete/Home/End/
//! Left/Right/Ctrl+A/U/W/普通 Char). These are owned exclusively by the
//! keyboard fallback path (`event::keyboard::normal_keys`), which mutates
//! the `tui_textarea::TextArea` widget directly. The main loop's 2b sync
//! (TextArea → InputState, conditional on `effect_did_mutate_textarea`) then reflects
//! the widget state back into `InputState` for read-only use by SM branches
//! like Enter/Up/Down.
//!
//! Rationale: handling these keys in the SM caused two bugs:
//! 1. **Double-execution** — both SM and keyboard fallback processed the
//!    same key, diverging `InputState` and `TextArea`.
//! 2. **2b sync revert** — even with double-execution filtered, the
//!    unconditional 2b sync overwrote SM's `InputState` edit with the
//!    stale `TextArea` value, so Backspace silently failed.
//!
//! Prediction clearing (a side-effect previously attached to Backspace/
//! Ctrl+U/Ctrl+W) is now done in the 2b sync layer by comparing text
//! length before/after.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};

use super::super::current_turn::CurrentTurn;
use super::super::event::{AcpEventData, Event};
use super::super::handlers::{AskUserHandler, HitlHandler, OauthHandler, RewindHandler};
use super::super::input::CursorPos;
use super::super::state::{IdleState, State, StreamingState};
use super::enter_modal_from_idle;
use crate::app::panel_types::PanelKind;
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

        // -- Tick: poll agent + drain notes + render. --
        Event::Tick => (
            State::Idle(state),
            vec![
                Effect::PollAgent,
                Effect::DrainPendingNotes,
                Effect::Render,
            ],
        ),

        // -- Mouse / Resize --------------------------------------------------
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollDown => (State::Idle(state), vec![Effect::Scroll { delta: 3 }]),
            MouseEventKind::ScrollUp => (State::Idle(state), vec![Effect::Scroll { delta: -3 }]),
            MouseEventKind::Down(MouseButton::Left) => (
                State::Idle(state),
                vec![Effect::Render],
            ),
            MouseEventKind::Drag(MouseButton::Left) => (
                State::Idle(state),
                vec![Effect::Render],
            ),
            MouseEventKind::Up(MouseButton::Left) => (
                State::Idle(state),
                vec![Effect::Render],
            ),
            _ => (State::Idle(state), vec![Effect::Render]),
        },
        Event::Resize { .. } => (
            State::Idle(state),
            vec![Effect::Render],
        ),

        // -- ACP events ------------------------------------------------------
        Event::AcpEvent(AcpEventData::ViewCommit(vc)) => {
            // Phase 2.5 — preserve TUI-only SystemNote (see streaming.rs
            // ViewCommit handler for rationale).
            state.view =
                super::super::view_store::merge_preserving_local_notes(&state.view, vc.view_models);
            (State::Idle(state), vec![Effect::Render])
        }

        Event::AcpEvent(AcpEventData::Prediction(_p)) => {
            // Cron #45: removed `state.input.prediction = Some(p.text)` —
            // production render reads `app.session_mgr.current().ui.prediction`
            // (v1 field set by `acp_bridge.rs:63`), not the SM InputState.
            // The v2 field was misleading dead code. SM now just emits Render
            // (redundant with v1 fallback, but safe). Proper v2 ownership of
            // prediction requires rewiring render — deferred.
            (State::Idle(state), vec![Effect::Render])
        }

        Event::AcpEvent(AcpEventData::FileSuggestions(_fs)) => {
            (State::Idle(state), vec![Effect::Render])
        }

        // -- §4.1 Streaming events: transition Idle → Streaming --------------
        // Arrives when agent starts producing output (out-of-band or after
        // SubmitMessage). Transition to Streaming so incremental data can
        // accumulate in current_turn.
        Event::AcpEvent(AcpEventData::TextChunk(tc)) => {
            let mut streaming = state.into_streaming();
            streaming.current_turn.append_text(&tc.text);
            (State::Streaming(streaming), vec![Effect::Render])
        }
        Event::AcpEvent(AcpEventData::ReasoningChunk(rc)) => {
            let mut streaming = state.into_streaming();
            streaming.current_turn.append_reasoning(&rc.text);
            (State::Streaming(streaming), vec![Effect::Render])
        }
        Event::AcpEvent(AcpEventData::ToolStarted(ts)) => {
            let mut streaming = state.into_streaming();
            streaming.current_turn.start_tool(
                super::super::current_turn::ToolCardAccumulator::new(
                    ts.tool_id,
                    ts.tool_name,
                    ts.input_summary,
                ),
            );
            (State::Streaming(streaming), vec![Effect::Render])
        }
        Event::AcpEvent(AcpEventData::ToolEnded(te)) => {
            let mut streaming = state.into_streaming();
            streaming
                .current_turn
                .end_tool(&te.tool_id, te.output_summary, te.is_error);
            (State::Streaming(streaming), vec![Effect::Render])
        }

        // -- §4.2 Boundary events in Idle (race / delayed) --------------------
        Event::AcpEvent(AcpEventData::TurnDone)
        | Event::AcpEvent(AcpEventData::TurnInterrupted(_)) => {
            // Agent finished but we're already Idle — no-op.
            (State::Idle(state), Vec::new())
        }

        // -- §4.3 Status: no state mutation, but emit Render ----------------
        //
        // 这些事件由 status bar 消费（CPU/MEM/context usage 等）。当前 v1
        // handle_acp_event 总是 emit Render 兜底，但 Phase 2.6 退役 v1 路径
        // 后，SM 必须自己 emit Render 否则 status bar 不会更新。
        //
        // Cron #27: 显式 emit Effect::Render（重复 Render 由 main_loop 去重）。
        Event::AcpEvent(AcpEventData::TokenUsage(_))
        | Event::AcpEvent(AcpEventData::ToolCount(_))
        | Event::AcpEvent(AcpEventData::Progress(_))
        | Event::AcpEvent(AcpEventData::BudgetWarning(_))
        | Event::AcpEvent(AcpEventData::SystemNotification(_))
        | Event::AcpEvent(AcpEventData::SubagentStarted(_))
        | Event::AcpEvent(AcpEventData::SubagentStopped(_))
        | Event::AcpEvent(AcpEventData::Unknown { .. }) => {
            (State::Idle(state), vec![Effect::Render])
        }

        // Interaction requests: construct the matching Handler and enter
        // Modal. The handler is wrapped in `ModalKind::Interaction` and
        // the IdleState's fields become `ModalState.saved_*` so closing
        // the popup restores the original view / input / scroll / history.
        Event::AcpEvent(AcpEventData::HitlPending(p)) => {
            enter_modal_from_idle(state, Box::new(HitlHandler::new(p)))
        }
        Event::AcpEvent(AcpEventData::AskUser(f)) => {
            enter_modal_from_idle(state, Box::new(AskUserHandler::new(f)))
        }
        Event::AcpEvent(AcpEventData::RewindPreview(rp)) => {
            enter_modal_from_idle(state, Box::new(RewindHandler::new(rp)))
        }
        Event::AcpEvent(AcpEventData::OauthNeeded(o)) => {
            enter_modal_from_idle(state, Box::new(OauthHandler::new(o)))
        }

        // -- System signals ---------------------------------------------------
        Event::AcpDisconnected => (
            State::Idle(state),
            vec![
                Effect::ShowNotification(
                    "ACP connection lost. Agent responses may not arrive.".to_string(),
                ),
                Effect::Render,
            ],
        ),
        Event::SessionLoaded { .. } => (State::Idle(state), Vec::new()),
        Event::Shutdown => (State::Idle(state), vec![Effect::Quit]),

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
            (State::Idle(state), vec![Effect::Render])
        }

        // -- Cron #24 P1 #2: push user bubble into state.view (v2 source) ----
        // AskUser 答案由 ask_user_confirm 通过 pending_v2_user_bubbles 队列路由，
        // main_loop 取出后通过此事件送入 SM，确保答案在生产渲染路径中可见。
        Event::PushUserBubble(text) => {
            state
                .view
                .push(peri_acp_types::view_model::ViewModel::UserBubble(
                    peri_acp_types::view_model::UserBubbleData { text },
                ));
            (State::Idle(state), vec![Effect::Render])
        }
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
            // Slash commands (/history, /model, etc.) are routed through the
            // keyboard fallback for CommandRegistry::dispatch. The SM stays
            // in Idle with no effects; is_sm_handled_shortcut returns false
            // for slash commands so the fallback takes over.
            if text.starts_with('/') {
                return (State::Idle(state), Vec::new());
            }
            state.input.clear_buffer();

            if text.trim().is_empty() {
                // Empty submit -- no-op, stay Idle.
                return (State::Idle(state), Vec::new());
            }

            // Save into history (newest at the back).
            state.input.history.push(text.clone());
            state.history_index = None;

            // Phase 2.6 step 7d: Push UserBubble to state.view so v2 readers
            // (interrupt paths migrated in step 7c, production render) can
            // locate the user's message immediately after submit — without
            // waiting for the first ACP ViewCommit echo. Cron #19 workflow
            // wjdz1xyqm confirmed: SM Enter did NOT push UserBubble, leaving
            // a window where state.view_models() had no UserBubble until the
            // ACP server's view_mapper emitted one in the next ViewCommit.
            // The ACP ViewCommit later replaces state.view wholesale, so the
            // canonical UserBubble (with attachment formatting) overwrites
            // this raw-text placeholder safely.
            state
                .view
                .push(peri_acp_types::view_model::ViewModel::UserBubble(
                    peri_acp_types::view_model::UserBubbleData { text: text.clone() },
                ));

            // SubmitMessage routes through main_loop → app.submit_message(),
            // which sends to ACP AND clears the TextArea. Using raw SendToAcp
            // would bypass TextArea cleanup, causing submitted text to reappear.
            let effects = vec![Effect::SubmitMessage { text }];

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

        // -- Esc: no-op -------------------------------------------------------
        //
        // Cron #45: removed `Effect::Quit` emission from this handler.
        // Previously the SM advanced a `DoubleEscTracker` (500ms threshold)
        // and emitted `Effect::Quit` on fast double-Esc. But
        // `is_sm_handled_shortcut` returns `false` for Esc — the v1 keyboard
        // fallback at `event/keyboard/normal_keys.rs:55-68` ALSO runs and
        // advances its own `rewind_pending_since` (2s threshold) tracker.
        //
        // Dual execution: a fast double-Esc (<500ms) triggered BOTH paths —
        // the SM emitted `Effect::Quit` while the v1 fallback opened the
        // rewind prompt. Quit won the effect race, but the rewind prompt
        // briefly flashed on screen before exit. Users who learned the
        // rewind gesture (slower double-tap) would accidentally quit on a
        // fast double-tap.
        //
        // Fix: SM no longer emits Quit. Esc gestures are owned by v1
        // exclusively:
        //   - First Esc: sets `rewind_pending_since` timestamp
        //   - Second Esc within 2s: opens rewind selector
        //   - Otherwise: no-op
        // Quit remains available via Ctrl+C (3-tier: clear input → interrupt
        // → double-tap quit) and the `/exit` slash command.
        KeyCode::Esc => (State::Idle(state), Vec::new()),

        // -- BackTab: cycle permission mode (handled by keyboard fallback) ---
        KeyCode::BackTab => (
            State::Idle(state),
            vec![Effect::Render],
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
                    // Ctrl+C: keyboard fallback (normal_keys::handle_ctrl_c)
                    // owns the full 3-level priority logic (clear input →
                    // interrupt agent → double-tap quit).  The SM just
                    // acknowledges the key with a re-render.
                    (State::Idle(state), vec![Effect::Render])
                }
                // Ctrl+A/U/W + Backspace/Delete/Home/End/Left/Right are
                // intentionally NOT handled by the SM — keyboard fallback
                // owns textarea editing (textarea is the single source of
                // truth for input). See module-level note above. SM handling
                // these would double-execute against keyboard fallback and
                // get reverted by the 2b sync (TextArea → InputState).
                't' => {
                    // Ctrl+T: cycle model alias (without Shift, handled above).
                    (State::Idle(state), vec![Effect::CycleModel, Effect::Render])
                }
                'b' => {
                    // Ctrl+B: focus background agent bar (handled by keyboard fallback).
                    (State::Idle(state), vec![Effect::Render])
                }
                'o' => {
                    // Ctrl+O: toggle inline diff (handled by keyboard fallback).
                    (State::Idle(state), vec![Effect::Render])
                }
                'p' => {
                    // Ctrl+P: open Model panel (P5 proof of concept).
                    (
                        State::Idle(state),
                        vec![Effect::OpenPanel(PanelKind::Model), Effect::Render],
                    )
                }
                // Other Ctrl+<char> combos pass through (P3 will dispatch more).
                _ => (State::Idle(state), Vec::new()),
            }
        }

        // -- Backspace/Delete/Home/End/Left/Right: NOT handled by SM -------
        // These editing keys are owned by the keyboard fallback (textarea
        // widget is the single source of truth). See module-level note.
        // SM handling would cause double-execution and get reverted by 2b
        // sync (TextArea → InputState) — visible as Backspace silently
        // failing.

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
            history_index: None,
        }
    }

    fn char_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn test_char_key_is_ignored_by_sm() {
        // Plain Char keys are handled by the keyboard fallback, not the SM.
        // The SM must stay in Idle without modifying InputState, so the
        // keyboard fallback can handle the key and step 2b can sync the
        // TextArea changes back to InputState.
        let state = make_state();
        let (next, effects) = handle(state, Event::Key(char_key('a')));
        match next {
            State::Idle(idle) => {
                // SM should NOT have inserted 'a' — keyboard fallback owns typing.
                assert_eq!(idle.input.text(), "");
                assert_eq!(idle.input.cursor, CursorPos::default());
            }
            _ => panic!("expected Idle"),
        }
        // SM produces no effects for plain Char — keyboard fallback provides Render.
        assert!(effects.is_empty());
    }

    #[test]
    fn test_tick_produces_poll_in_idle() {
        let state = make_state();
        let (_next, effects) = handle(state, Event::Tick);
        assert!(effects.iter().any(|e| matches!(e, Effect::PollAgent)));
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
            Effect::SubmitMessage { text } => {
                assert_eq!(text, "hello world");
            }
            _ => panic!("expected SubmitMessage effect"),
        }
    }

    // -- Phase 2.6 step 7d: UserBubble 推送到 state.view ---------------------

    #[test]
    fn test_enter_pushes_userbubble_to_state_view() {
        // Phase 2.6 step 7d: SM Enter 必须把 UserBubble 推到 state.view，
        // 让中断路径和生产渲染在第一次 ACP ViewCommit 之前就能定位到
        // 用户消息。此前是 cron #19 文档记录的潜在 bug。
        let mut state = make_state();
        state.input.insert_str("hello world");
        let (next, _effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        );
        match next {
            State::Streaming(s) => {
                // view 中应包含恰好一个 UserBubble，文本与提交的输入一致。
                assert_eq!(
                    s.view.len(),
                    1,
                    "StreamingState.view should contain exactly the pushed UserBubble"
                );
                match &s.view[0] {
                    peri_acp_types::view_model::ViewModel::UserBubble(d) => {
                        assert_eq!(d.text, "hello world");
                    }
                    other => panic!("expected UserBubble, got {other:?}"),
                }
            }
            other => panic!("expected Streaming, got {other:?}"),
        }
    }

    #[test]
    fn test_enter_does_not_push_userbubble_for_slash_commands() {
        // Slash 命令（如 /history, /model）在 idle.rs:238-240 短路返回 Idle，
        // 不进入 Enter 提交分支。UserBubble 不应被推送。
        let mut state = make_state();
        state.input.insert_str("/help");
        let (next, _effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        );
        match next {
            State::Idle(idle) => {
                assert!(
                    idle.view.is_empty(),
                    "Slash command must not push UserBubble to view"
                );
            }
            other => panic!("expected Idle for slash command, got {other:?}"),
        }
    }

    #[test]
    fn test_empty_enter_does_not_push_userbubble() {
        // 空白输入（trim 后为空）应在 idle.rs:243-246 提前返回 Idle，
        // 不推送 UserBubble。
        let mut state = make_state();
        state.input.insert_str("   "); // 仅空白
        let (next, _effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        );
        match next {
            State::Idle(idle) => {
                assert!(
                    idle.view.is_empty(),
                    "Empty submit must not push UserBubble to view"
                );
            }
            other => panic!("expected Idle for empty submit, got {other:?}"),
        }
    }

    #[test]
    fn test_enter_preserves_prior_view_when_pushing_userbubble() {
        // 已有历史 view（例如上一轮对话）时，Enter 应在末尾追加 UserBubble，
        // 而不是替换整个 view。
        use peri_acp_types::view_model::{AssistantBubbleData, ViewModel};
        let mut state = make_state();
        state.view = vec![ViewModel::AssistantBubble(AssistantBubbleData {
            text: "prior reply".into(),
            reasoning: None,
            tool_card_ids: vec![],
        })];
        state.input.insert_str("next question");
        let (next, _effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        );
        match next {
            State::Streaming(s) => {
                assert_eq!(
                    s.view.len(),
                    2,
                    "Prior view must be preserved, UserBubble appended"
                );
                assert!(matches!(s.view[0], ViewModel::AssistantBubble(_)));
                assert!(matches!(s.view[1], ViewModel::UserBubble(_)));
            }
            other => panic!("expected Streaming, got {other:?}"),
        }
    }

    #[test]
    fn test_double_esc_does_not_quit_from_sm() {
        // Cron #45: removed `Effect::Quit` emission from SM Esc handler.
        // Previously the SM advanced `DoubleEscTracker` (500ms threshold)
        // and emitted `Effect::Quit` on fast double-Esc. But
        // `is_sm_handled_shortcut` returns false for Esc — v1 keyboard
        // fallback ALSO runs and advances `rewind_pending_since` (2s
        // threshold). Fast double-Esc triggered BOTH: SM Quit + v1 rewind
        // prompt. Quit won the effect race but rewind prompt briefly
        // flashed. Now SM Esc is no-op for quit; v1 owns rewind gesture
        // exclusively. Quit is via Ctrl+C / `/exit`.
        let state = make_state();
        let (next, _e1) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        );
        let idle_again = match next {
            State::Idle(s) => s,
            _ => panic!("expected Idle after first Esc"),
        };
        let (_state, e2) = handle(
            idle_again,
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        );
        assert!(
            !e2.iter().any(|e| matches!(e, Effect::Quit)),
            "Cron #45: SM Esc handler must NOT emit Quit (conflicts with v1 rewind gesture)"
        );
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
    fn test_backspace_is_noop_in_sm() {
        // SM 不处理 Backspace —— textarea 是 Idle 输入的唯一编辑权威。
        // Backspace 由 keyboard fallback 处理（修改 textarea widget），
        // 之后 main_loop 的 2b 同步把结果反映回 InputState。
        // 参见模块顶部"Input editing"注释。
        let mut state = make_state();
        state.input.insert_str("abc");
        let (next, effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
        );
        match next {
            State::Idle(idle) => {
                // SM 不修改 InputState，文本应保持原样。
                assert_eq!(idle.input.text(), "abc");
            }
            _ => panic!("expected Idle"),
        }
        // SM 也不 emit effects —— keyboard fallback 负责 Render。
        assert!(effects.is_empty());
    }

    #[test]
    fn test_ctrl_a_u_w_are_noop_in_sm() {
        // Ctrl+A/U/W 同样由 keyboard fallback 独占（select_all /
        // delete_line_by_head / delete_word 走 textarea widget）。
        for c in ['a', 'u', 'w'] {
            let mut state = make_state();
            state.input.insert_str("hello world");
            let (next, effects) = handle(
                state,
                Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)),
            );
            match next {
                State::Idle(idle) => {
                    // SM 不修改 InputState。
                    assert_eq!(idle.input.text(), "hello world", "Ctrl+{c} should be no-op");
                }
                _ => panic!("expected Idle"),
            }
            assert!(effects.is_empty(), "Ctrl+{c} should emit no effects");
        }
    }

    #[test]
    fn test_navigation_keys_are_noop_in_sm() {
        // Left/Right/Home/End/Delete 同样由 keyboard fallback 独占。
        for code in [
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::Delete,
        ] {
            let mut state = make_state();
            state.input.insert_str("abc");
            let (next, effects) =
                handle(state, Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
            match next {
                State::Idle(idle) => {
                    assert_eq!(idle.input.text(), "abc", "{code:?} should be no-op");
                }
                _ => panic!("expected Idle"),
            }
            assert!(effects.is_empty(), "{code:?} should emit no effects");
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
    fn test_backtab_emits_render() {
        let state = make_state();
        let (_next, effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE)),
        );
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
    fn test_ctrl_b_emits_render() {
        let state = make_state();
        let (_next, effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)),
        );
        assert!(effects.iter().any(|e| matches!(e, Effect::Render)));
    }

    #[test]
    fn test_ctrl_o_emits_render() {
        let state = make_state();
        let (_next, effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)),
        );
        assert!(effects.iter().any(|e| matches!(e, Effect::Render)));
    }

    #[test]
    fn test_ctrl_p_opens_model_panel() {
        let state = make_state();
        let (_next, effects) = handle(
            state,
            Event::Key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
        );
        assert!(effects
            .iter()
            .any(|e| matches!(e, Effect::OpenPanel(PanelKind::Model))));
        assert!(effects.iter().any(|e| matches!(e, Effect::Render)));
    }

    #[test]
    fn test_resize_emits_render() {
        let state = make_state();
        let (_next, effects) = handle(
            state,
            Event::Resize {
                width: 80,
                height: 24,
            },
        );
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Render)),
            "Resize in Idle should emit Render"
        );
    }

    #[test]
    fn test_acp_disconnected_emits_show_notification() {
        let state = make_state();
        let (_next, effects) = handle(state, Event::AcpDisconnected);
        assert!(
            effects.iter().any(
                |e| matches!(e, Effect::ShowNotification(msg) if msg.contains("ACP connection lost"))
            ),
            "AcpDisconnected in Idle should emit a notification about lost connection"
        );
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Render)),
            "AcpDisconnected in Idle should emit Render"
        );
    }

    // ── Cron #24 P1 #2: PushUserBubble 推送到 state.view ─────────────────

    #[test]
    fn test_push_userbubble_adds_to_state_view_in_idle() {
        // Cron #24 P1 #2: AskUser 答案由 ask_user_confirm 通过队列路由，
        // main_loop 取出后通过 Event::PushUserBubble 送入 SM。Idle 状态下
        // 必须把 UserBubble 追加到 state.view 并 emit Render，否则答案
        // 在生产渲染路径中不可见（v1 view_messages 路径已退役）。
        let state = make_state();
        let (next, effects) = handle(state, Event::PushUserBubble("yes".into()));
        match next {
            State::Idle(idle) => {
                assert_eq!(idle.view.len(), 1);
                match &idle.view[0] {
                    peri_acp_types::view_model::ViewModel::UserBubble(d) => {
                        assert_eq!(d.text, "yes");
                    }
                    other => panic!("expected UserBubble, got {other:?}"),
                }
            }
            other => panic!("expected Idle, got {other:?}"),
        }
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Render)),
            "PushUserBubble in Idle should emit Render"
        );
    }

    #[test]
    fn test_push_userbubble_preserves_prior_view_in_idle() {
        // 已有历史 view 时，PushUserBubble 应在末尾追加而非替换。
        use peri_acp_types::view_model::{AssistantBubbleData, ViewModel};
        let mut state = make_state();
        state.view = vec![ViewModel::AssistantBubble(AssistantBubbleData {
            text: "prior reply".into(),
            reasoning: None,
            tool_card_ids: vec![],
        })];
        let (next, _) = handle(state, Event::PushUserBubble("answer".into()));
        match next {
            State::Idle(idle) => {
                assert_eq!(idle.view.len(), 2);
                assert!(matches!(idle.view[0], ViewModel::AssistantBubble(_)));
                assert!(matches!(idle.view[1], ViewModel::UserBubble(_)));
            }
            other => panic!("expected Idle, got {other:?}"),
        }
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
    fn test_mouse_left_click_emits_render() {
        let state = make_state();
        let mouse = make_mouse(MouseEventKind::Down(MouseButton::Left), 10, 5);
        let (_next, effects) = handle(state, Event::Mouse(mouse));
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Render)),
            "Left click should emit Render"
        );
    }

    #[test]
    fn test_mouse_drag_emits_render() {
        let state = make_state();
        let mouse = make_mouse(MouseEventKind::Drag(MouseButton::Left), 12, 8);
        let (_next, effects) = handle(state, Event::Mouse(mouse));
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Render)),
            "Left drag should emit Render"
        );
    }

    #[test]
    fn test_mouse_up_emits_render() {
        let state = make_state();
        let mouse = make_mouse(MouseEventKind::Up(MouseButton::Left), 10, 5);
        let (_next, effects) = handle(state, Event::Mouse(mouse));
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Render)),
            "Mouse up should emit Render"
        );
    }

    // ── Streaming events during Idle → transition to Streaming ──────────

    #[test]
    fn test_text_chunk_in_idle_transitions_to_streaming() {
        // 第一个流式事件到达时，Idle 应转换到 Streaming 并累积数据。
        use peri_acp_types::event_data::TextChunk;
        let state = make_state();
        let (next, effects) = handle(
            state,
            Event::AcpEvent(AcpEventData::TextChunk(TextChunk {
                text: "hello streaming".into(),
                agent_id: None,
            })),
        );
        match next {
            State::Streaming(s) => {
                assert_eq!(s.current_turn.text, "hello streaming");
                assert!(s.current_turn.active);
            }
            other => panic!(
                "expected Streaming after TextChunk in Idle, got {:?}",
                other
            ),
        }
        assert_eq!(effects, vec![Effect::Render]);
    }

    #[test]
    fn test_reasoning_chunk_in_idle_transitions_to_streaming() {
        use peri_acp_types::event_data::ReasoningChunk;
        let state = make_state();
        let (next, _effects) = handle(
            state,
            Event::AcpEvent(AcpEventData::ReasoningChunk(ReasoningChunk {
                text: "thinking...".into(),
                agent_id: None,
            })),
        );
        match next {
            State::Streaming(s) => {
                assert_eq!(s.current_turn.reasoning, "thinking...");
            }
            other => panic!(
                "expected Streaming after ReasoningChunk in Idle, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn test_tool_started_in_idle_transitions_to_streaming() {
        use peri_acp_types::event_data::ToolStarted;
        let state = make_state();
        let (next, _effects) = handle(
            state,
            Event::AcpEvent(AcpEventData::ToolStarted(ToolStarted {
                tool_id: "t1".into(),
                tool_name: "Read".into(),
                input_summary: "path: foo.rs".into(),
                agent_id: None,
            })),
        );
        match next {
            State::Streaming(s) => {
                assert!(!s.current_turn.tool_cards.is_empty());
            }
            other => panic!(
                "expected Streaming after ToolStarted in Idle, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn test_turn_done_in_idle_is_noop() {
        // TurnDone 在 Idle 是 race/delayed，保持 Idle。
        let state = make_state();
        let (next, effects) = handle(state, Event::AcpEvent(AcpEventData::TurnDone));
        assert!(matches!(next, State::Idle(_)));
        assert!(effects.is_empty());
    }

    #[test]
    fn test_status_events_in_idle_emit_render() {
        // Cron #27: status-bar events (TokenUsage/Progress/BudgetWarning/...)
        // 必须触发 Render，否则 status bar 不会更新。当前由 v1 handle_acp_event
        // 兜底，但 Phase 2.6 退役 v1 路径后 SM 是唯一来源——必须自己 emit Render。
        use peri_acp_types::event_data::TokenUsage;
        let state = make_state();
        let (next, effects) = handle(
            state,
            Event::AcpEvent(AcpEventData::TokenUsage(TokenUsage {
                input: 10,
                output: 5,
            })),
        );
        assert!(matches!(next, State::Idle(_)));
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Render)),
            "Status events (TokenUsage) must emit Render so status bar updates"
        );
    }

    // ── Phase 1.3: Agent-initiated interaction requests enter Modal ──

    /// Helper: assert the result is a Modal holding a specific Handler kind.
    /// We can't introspect Box<dyn Handler>, but we can verify the variant
    /// and that saved_* fields are populated correctly.
    fn assert_interaction_modal(next: State) {
        match next {
            State::Modal(m) => {
                // Idle source: no saved_current_turn.
                assert!(
                    m.saved_current_turn.is_none(),
                    "Idle→Modal must have no saved_current_turn"
                );
                // Kind is Interaction (not Panel).
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
    fn test_hitl_pending_in_idle_enters_modal() {
        // HitlPending 在 Idle 必须进入 Modal(Interaction(HitlHandler))。
        // Phase 1.3: 此前是 (State::Idle(state), Vec::new()) 静默丢弃。
        use peri_acp_types::event_data::HitlPending;
        let state = make_state();
        let (next, effects) = handle(
            state,
            Event::AcpEvent(AcpEventData::HitlPending(HitlPending {
                tool_name: "Edit".into(),
                tool_input: serde_json::json!({}),
                batch: None,
            })),
        );
        assert_interaction_modal(next);
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Render)),
            "HitlPending → Modal must emit Render"
        );
    }

    #[test]
    fn test_ask_user_in_idle_enters_modal() {
        // AskUser 在 Idle 必须进入 Modal(Interaction(AskUserHandler))。
        use peri_acp_types::event_data::{AskUser, Question};
        let state = make_state();
        let (next, effects) = handle(
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
        assert_interaction_modal(next);
        assert!(effects.iter().any(|e| matches!(e, Effect::Render)));
    }

    #[test]
    fn test_rewind_preview_in_idle_enters_modal() {
        // RewindPreview 在 Idle 必须进入 Modal(Interaction(RewindHandler))。
        use peri_acp_types::event_data::RewindPreview;
        let state = make_state();
        let (next, effects) = handle(
            state,
            Event::AcpEvent(AcpEventData::RewindPreview(RewindPreview {
                files: vec![],
                messages: vec![],
            })),
        );
        assert_interaction_modal(next);
        assert!(effects.iter().any(|e| matches!(e, Effect::Render)));
    }

    #[test]
    fn test_oauth_needed_in_idle_enters_modal() {
        // OauthNeeded 在 Idle 必须进入 Modal(Interaction(OauthHandler))。
        use peri_acp_types::event_data::OauthNeeded;
        let state = make_state();
        let (next, effects) = handle(
            state,
            Event::AcpEvent(AcpEventData::OauthNeeded(OauthNeeded {
                server_name: "github-mcp".into(),
                auth_url: "https://github.com/login".into(),
            })),
        );
        assert_interaction_modal(next);
        assert!(effects.iter().any(|e| matches!(e, Effect::Render)));
    }

    #[test]
    fn test_interaction_modal_preserves_idle_saved_fields() {
        // 进入 Modal 时必须保留 view / input / scroll / history / 双 Esc。
        // 否则关闭弹窗后会丢失用户上下文。
        use peri_acp_types::event_data::HitlPending;
        let mut state = make_state();
        state.input.insert_str("typing before popup");
        state.scroll_offset = 7;
        state.history_index = Some(2);
        state.view = vec![];

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
                assert_eq!(m.saved_input.text(), "typing before popup");
                assert_eq!(m.saved_scroll_offset, 7);
                assert_eq!(m.saved_history_index, Some(2));
            }
            _ => panic!("expected Modal"),
        }
    }
}
