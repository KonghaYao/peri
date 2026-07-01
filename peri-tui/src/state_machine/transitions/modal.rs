//! Modal-state transition: `(ModalState, Event) -> (State, Vec<Effect>)`.
//!
//! A panel or interaction popup is active and captures all keyboard input.
//! Keys are delegated to the active `PanelState` / `Handler`; Esc dismisses
//! the popup and returns to the previous state (Idle in the P2 stub).
//!
//! Reference: `docs/design/peri-tui-architecture.md` section 8.6.

#[cfg(test)]
use std::collections::HashMap;

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use tui_textarea::Input;

use super::super::event::{AcpEventData, Event};
use super::super::state::{HandlerOutput, ModalKind, ModalState, PanelReadContext, State};
#[cfg(test)]
use super::super::state::{IdleState, InputState};
use crate::panel::effect::PanelEffect;
#[cfg(test)]
use crate::panel::read_context::ServiceRegistrySnapshot;
use crate::runtime::effect::Effect;

/// Result of dispatching a key/mouse/paste event in Modal state.
struct DispatchResult {
    /// Panel effects (for Panel variant).
    panel_effects: Vec<PanelEffect>,
    /// Whether the modal should close.
    should_close: bool,
    /// Handler submit payload (for Interaction variant, when Submit is returned).
    handler_submit: Option<String>,
}

/// Modal-state transition entry point (test-only).
///
/// Production code uses [`handle_with_context`] instead, providing a real
/// [`PanelReadContext`] built from `&App`. This test-only entry uses TLS
/// stub snapshots and is reachable only when tests call `state_machine::handle`
/// with a `State::Modal(..)` (production main_loop routes Modal directly to
/// `handle_with_context`).
#[cfg(test)]
pub fn handle(mut state: ModalState, event: Event) -> (State, Vec<Effect>) {
    match event {
        // -- Esc: dismiss popup via ClosePanel effect ------------------------
        // Emits ClosePanel so main_loop restores saved_idle (preserving
        // message history, scroll, input). Direct transition_to_idle() would
        // create a fresh empty state, losing all context.
        Event::Key(KeyEvent {
            code: KeyCode::Esc, ..
        }) => (
            State::Modal(state),
            vec![Effect::ClosePanel, Effect::Render],
        ),

        // -- Other key events: delegate to the panel/handler -----------------
        Event::Key(key) => {
            let result = dispatch_key(&mut state, key);
            let mut effects = map_panel_effects(result.panel_effects);
            if let Some(payload) = result.handler_submit {
                // Handler submitted -- send to ACP, then close panel.
                effects.push(Effect::SendToAcp {
                    method: "interaction/submit".to_string(),
                    params: serde_json::json!({ "payload": payload }),
                });
                effects.push(Effect::ClosePanel);
                effects.push(Effect::Render);
                (State::Modal(state), effects)
            } else if result.should_close {
                // ClosePanel effect so main_loop restores saved_idle.
                let mut all_effects = effects;
                all_effects.push(Effect::ClosePanel);
                all_effects.push(Effect::Render);
                (State::Modal(state), all_effects)
            } else {
                (State::Modal(state), effects)
            }
        }

        // -- Mouse: dispatch scroll/click/hover to the panel (TLS context) ----
        Event::Mouse(mouse) => {
            let result = dispatch_mouse_tls(&mut state, mouse);
            let mut effects = map_panel_effects(result.panel_effects);
            effects.push(Effect::Render);
            if result.should_close {
                effects.push(Effect::ClosePanel);
            }
            (State::Modal(state), effects)
        }

        // -- Paste: dispatch text to the panel (TLS context) ------------------
        Event::Paste(text) => {
            let result = dispatch_paste_tls(&mut state, &text);
            let mut effects = map_panel_effects(result.panel_effects);
            effects.push(Effect::Render);
            if result.should_close {
                effects.push(Effect::ClosePanel);
            }
            (State::Modal(state), effects)
        }

        // -- Resize: re-render so the popup lays out correctly ---------------
        Event::Resize { .. } => (State::Modal(state), vec![Effect::Render]),

        // -- Tick: keep background processes alive while Modal is open --------
        // PollAgent keeps ACP event consumption flowing; AdvanceSpinner keeps
        // the loading animation running; Render ensures the panel redraws.
        Event::Tick => (
            State::Modal(state),
            vec![
                Effect::AdvanceSpinner,
                Effect::PollAgent,
                Effect::PollWorkflow,
                Effect::Render,
            ],
        ),

        // -- AcpEvent: apply to saved_* (don't drop!) ------------------------
        // The agent keeps producing output while the popup is open. Apply
        // each event to saved_view / saved_current_turn so closing the
        // popup restores the latest state, not a stale snapshot.
        Event::AcpEvent(acp_event) => {
            let effects = apply_acp_event_to_saved(&mut state, &acp_event);
            (State::Modal(state), effects.unwrap_or_default())
        }

        // -- Everything else: keep Modal, no effect --------------------------
        Event::AcpDisconnected | Event::SessionLoaded { .. } => (State::Modal(state), Vec::new()),

        // -- Shutdown: propagate to main_loop so app can quit ----------------
        Event::Shutdown => (State::Modal(state), vec![Effect::Quit]),

        // -- Phase 2.4: push system note into saved_view (v2 source) -------
        Event::PushSystemNote(text) => {
            state
                .saved_view
                .push(peri_acp_types::view_model::ViewModel::SystemNote(
                    peri_acp_types::view_model::SystemNoteData {
                        text,
                        level: peri_acp_types::view_model::NoteLevel::Info,
                    },
                ));
            (State::Modal(state), Vec::new())
        }

        // -- Cron #24 P1 #2: push user bubble into saved_view (v2 source) --
        // Modal 期间到达的 AskUser 答案路由到 saved_view（ClosePanel 后恢复时可见）。
        Event::PushUserBubble(text) => {
            state
                .saved_view
                .push(peri_acp_types::view_model::ViewModel::UserBubble(
                    peri_acp_types::view_model::UserBubbleData { text },
                ));
            (State::Modal(state), Vec::new())
        }
    }
}

/// Build a fresh Idle state with a Render effect (used when dismissing modal).
#[cfg(test)]
fn transition_to_idle() -> (State, Vec<Effect>) {
    let idle = IdleState {
        input: InputState::default(),
        scroll_offset: 0,
        view: vec![],
        history_index: None,
    };
    (State::Idle(idle), vec![Effect::Render])
}

/// Build a fresh Idle state, preserving the given effects (e.g., ShowNotification).
#[cfg(test)]
fn transition_to_idle_with_effects(effects: Vec<Effect>) -> (State, Vec<Effect>) {
    let idle = IdleState {
        input: InputState::default(),
        scroll_offset: 0,
        view: vec![],
        history_index: None,
    };
    // Ensure at least one Render so the user sees the dismissal.
    let mut all_effects = effects;
    if !all_effects.iter().any(|e| matches!(e, Effect::Render)) {
        all_effects.push(Effect::Render);
    }
    (State::Idle(idle), all_effects)
}

/// Dispatch a single key to the active panel / handler, using TLS stubs.
///
/// Returns a [`DispatchResult`]. `should_close=true` when the panel produced
/// `PanelEffect::Close` or the handler returned `Submit`/`Dismiss`.
/// `handler_submit` carries the payload when an Interaction handler returns `Submit`.
///
/// **Production code uses [`dispatch_key_with_ctx`] instead**, providing
/// a real [`PanelReadContext`] constructed from App data. This function exists
/// only for test compatibility (tests don't have access to App).
#[cfg(test)]
fn dispatch_key(state: &mut ModalState, key: KeyEvent) -> DispatchResult {
    STUB_SNAPSHOT.with(|snapshot| {
        STUB_VMS.with(|vms| {
            STUB_CACHE.with(|cache| {
                STUB_LC.with(|lc| {
                    let ctx = PanelReadContext {
                        services: snapshot,
                        view_models: vms,
                        scroll_offset: 0,
                        area: ratatui::layout::Rect::new(0, 0, 0, 0),
                        lc,
                        acp_query_cache: cache,
                    };
                    dispatch_key_with_ctx(state, key, &ctx)
                })
            })
        })
    })
}

/// Dispatch a mouse event using TLS stubs (test-only path).
#[cfg(test)]
fn dispatch_mouse_tls(
    state: &mut ModalState,
    mouse: ratatui::crossterm::event::MouseEvent,
) -> DispatchResult {
    STUB_SNAPSHOT.with(|snapshot| {
        STUB_VMS.with(|vms| {
            STUB_CACHE.with(|cache| {
                STUB_LC.with(|lc| {
                    let ctx = PanelReadContext {
                        services: snapshot,
                        view_models: vms,
                        scroll_offset: 0,
                        area: ratatui::layout::Rect::new(0, 0, 0, 0),
                        lc,
                        acp_query_cache: cache,
                    };
                    dispatch_mouse(state, mouse, &ctx)
                })
            })
        })
    })
}

/// Dispatch a paste event using TLS stubs (test-only path).
#[cfg(test)]
fn dispatch_paste_tls(state: &mut ModalState, text: &str) -> DispatchResult {
    STUB_SNAPSHOT.with(|snapshot| {
        STUB_VMS.with(|vms| {
            STUB_CACHE.with(|cache| {
                STUB_LC.with(|lc| {
                    let ctx = PanelReadContext {
                        services: snapshot,
                        view_models: vms,
                        scroll_offset: 0,
                        area: ratatui::layout::Rect::new(0, 0, 0, 0),
                        lc,
                        acp_query_cache: cache,
                    };
                    dispatch_paste(state, text, &ctx)
                })
            })
        })
    })
}

// TLS stubs shared by the test-only dispatch wrappers.
#[cfg(test)]
thread_local! {
    static STUB_SNAPSHOT: ServiceRegistrySnapshot = ServiceRegistrySnapshot::new();
    static STUB_VMS: Vec<peri_acp_types::view_model::ViewModel> = const { Vec::new() };
    static STUB_CACHE: HashMap<String, serde_json::Value> = HashMap::new();
    static STUB_LC: crate::i18n::LcRegistry = crate::i18n::LcRegistry::default();
}

/// Dispatch a single key to the active panel / handler with a real context.
///
/// **Production path** -- `ctx` is constructed by main_loop from `&App` data.
/// Returns `(panel_effects, should_close)`.
/// Dispatch a mouse event to the active panel / handler with a real context.
///
/// For `ScrollUp`/`ScrollDown`, calls [`PanelState::handle_scroll`] with
/// ±3 lines (matching the legacy convention). All other mouse events
/// (`Moved`, `Down`, `Drag`, etc.) are forwarded to [`PanelState::handle_mouse`].
fn dispatch_mouse(
    state: &mut ModalState,
    mouse: ratatui::crossterm::event::MouseEvent,
    ctx: &PanelReadContext,
) -> DispatchResult {
    use ratatui::crossterm::event::MouseEventKind;
    match &mut state.kind {
        ModalKind::Panel(panel) => {
            let effects = match mouse.kind {
                MouseEventKind::ScrollUp => panel.handle_scroll(-3, ctx),
                MouseEventKind::ScrollDown => panel.handle_scroll(3, ctx),
                _ => panel.handle_mouse(mouse, ctx.area, ctx),
            };
            let should_close = effects.iter().any(|pe| matches!(pe, PanelEffect::Close));
            DispatchResult {
                panel_effects: effects,
                should_close,
                handler_submit: None,
            }
        }
        ModalKind::Interaction(_) => DispatchResult {
            panel_effects: Vec::new(),
            should_close: false,
            handler_submit: None,
        },
    }
}

/// Dispatch a paste event to the active panel / handler with a real context.
fn dispatch_paste(state: &mut ModalState, text: &str, ctx: &PanelReadContext) -> DispatchResult {
    match &mut state.kind {
        ModalKind::Panel(panel) => {
            let effects = panel.handle_paste(text, ctx);
            let should_close = effects.iter().any(|pe| matches!(pe, PanelEffect::Close));
            DispatchResult {
                panel_effects: effects,
                should_close,
                handler_submit: None,
            }
        }
        ModalKind::Interaction(_) => DispatchResult {
            panel_effects: Vec::new(),
            should_close: false,
            handler_submit: None,
        },
    }
}

fn dispatch_key_with_ctx(
    state: &mut ModalState,
    key: KeyEvent,
    ctx: &PanelReadContext,
) -> DispatchResult {
    match &mut state.kind {
        ModalKind::Panel(panel) => {
            let panel_effects = panel.handle_key(Input::from(key), ctx);
            let should_close = panel_effects
                .iter()
                .any(|pe| matches!(pe, PanelEffect::Close));
            DispatchResult {
                panel_effects,
                should_close,
                handler_submit: None,
            }
        }
        ModalKind::Interaction(handler) => {
            // Handler trait (Phase 1.2) takes a full KeyEvent so it can
            // match on KeyCode::Esc / Tab / Enter / Char directly — no
            // lossy char projection needed.
            match handler.handle_key(key) {
                HandlerOutput::Nothing => DispatchResult {
                    panel_effects: Vec::new(),
                    should_close: false,
                    handler_submit: None,
                },
                HandlerOutput::Submit(payload) => DispatchResult {
                    panel_effects: Vec::new(),
                    should_close: true,
                    handler_submit: Some(payload),
                },
                HandlerOutput::Dismiss => DispatchResult {
                    panel_effects: Vec::new(),
                    should_close: true,
                    handler_submit: None,
                },
            }
        }
    }
}

/// Modal handler that accepts an external [`PanelReadContext`].
///
/// **Production path** -- called by main_loop when state is `State::Modal(...)`.
/// Uses the provided `ctx` (built from `&App`) instead of TLS stubs.
pub fn handle_with_context(
    mut state: ModalState,
    event: Event,
    ctx: &PanelReadContext,
) -> (State, Vec<Effect>) {
    match event {
        // -- Esc: dismiss popup via ClosePanel effect ------------------------
        // Emits ClosePanel so main_loop restores saved_idle (preserving
        // message history, scroll, input). Direct transition_to_idle() would
        // create a fresh empty state, losing all context.
        Event::Key(KeyEvent {
            code: KeyCode::Esc, ..
        }) => (
            State::Modal(state),
            vec![Effect::ClosePanel, Effect::Render],
        ),

        // -- Other key events: delegate to the panel/handler -----------------
        Event::Key(key) => {
            let result = dispatch_key_with_ctx(&mut state, key, ctx);
            let mut effects = map_panel_effects(result.panel_effects);
            if let Some(payload) = result.handler_submit {
                // Handler submitted -- send to ACP, then close panel.
                effects.push(Effect::SendToAcp {
                    method: "interaction/submit".to_string(),
                    params: serde_json::json!({ "payload": payload }),
                });
                effects.push(Effect::ClosePanel);
                effects.push(Effect::Render);
                (State::Modal(state), effects)
            } else if result.should_close {
                // ClosePanel effect so main_loop restores saved_idle.
                let mut all_effects = effects;
                all_effects.push(Effect::ClosePanel);
                all_effects.push(Effect::Render);
                (State::Modal(state), all_effects)
            } else {
                (State::Modal(state), effects)
            }
        }

        // -- Mouse: dispatch scroll/click/hover to the panel -----------------
        Event::Mouse(mouse) => {
            let result = dispatch_mouse(&mut state, mouse, ctx);
            let mut effects = map_panel_effects(result.panel_effects);
            effects.push(Effect::Render);
            if result.should_close {
                effects.push(Effect::ClosePanel);
            }
            (State::Modal(state), effects)
        }

        // -- Paste: dispatch text to the panel -------------------------------
        Event::Paste(text) => {
            let result = dispatch_paste(&mut state, &text, ctx);
            let mut effects = map_panel_effects(result.panel_effects);
            effects.push(Effect::Render);
            if result.should_close {
                effects.push(Effect::ClosePanel);
            }
            (State::Modal(state), effects)
        }

        // -- Resize: re-render so the popup lays out correctly ---------------
        Event::Resize { .. } => (State::Modal(state), vec![Effect::Render]),

        // -- Tick: keep background processes alive while Modal is open --------
        Event::Tick => (
            State::Modal(state),
            vec![
                Effect::AdvanceSpinner,
                Effect::PollAgent,
                Effect::PollWorkflow,
                Effect::Render,
            ],
        ),

        // -- AcpEvent: apply to saved_* (don't drop!) ------------------------
        // The agent keeps producing output while the popup is open. Apply
        // each event to saved_view / saved_current_turn so closing the
        // popup restores the latest state, not a stale snapshot.
        Event::AcpEvent(acp_event) => {
            let effects = apply_acp_event_to_saved(&mut state, &acp_event);
            (State::Modal(state), effects.unwrap_or_default())
        }

        // -- Everything else: keep Modal, no effect --------------------------
        Event::AcpDisconnected | Event::SessionLoaded { .. } => (State::Modal(state), Vec::new()),

        // -- Shutdown: propagate to main_loop so app can quit ----------------
        Event::Shutdown => (State::Modal(state), vec![Effect::Quit]),

        // -- Phase 2.4: push system note into saved_view (v2 source) -------
        Event::PushSystemNote(text) => {
            state
                .saved_view
                .push(peri_acp_types::view_model::ViewModel::SystemNote(
                    peri_acp_types::view_model::SystemNoteData {
                        text,
                        level: peri_acp_types::view_model::NoteLevel::Info,
                    },
                ));
            (State::Modal(state), Vec::new())
        }

        // -- Cron #24 P1 #2: push user bubble into saved_view (v2 source) --
        // Modal 期间到达的 AskUser 答案路由到 saved_view（ClosePanel 后恢复时可见）。
        Event::PushUserBubble(text) => {
            state
                .saved_view
                .push(peri_acp_types::view_model::ViewModel::UserBubble(
                    peri_acp_types::view_model::UserBubbleData { text },
                ));
            (State::Modal(state), Vec::new())
        }
    }
}

/// Apply an `AcpEventData` to the ModalState's saved_* fields.
///
/// While a modal popup is open, ACP events that mutate the underlying
/// agent state (view snapshot, streaming chunks, turn boundaries) must
/// still be applied to `saved_view` / `saved_current_turn` so that when
/// the popup closes, the restored state reflects progress made during
/// the modal session. Without this, closing the popup would rewind the
/// UI to whatever state was captured when the modal opened.
///
/// Returns `Some(effects)` if the event was handled (always includes
/// `Effect::Render`), or `None` if the event is a silent no-op (status
/// events, interaction requests, unknown events).
fn apply_acp_event_to_saved(state: &mut ModalState, event: &AcpEventData) -> Option<Vec<Effect>> {
    use super::super::current_turn::ToolCardAccumulator;
    match event {
        AcpEventData::ViewCommit(vc) => {
            // Replace saved_view with the new snapshot (替换语义, per P2-C).
            //
            // Phase 2.5 — preserve TUI-only SystemNote (see streaming.rs
            // ViewCommit handler for rationale).
            state.saved_view = super::super::view_store::merge_preserving_local_notes(
                &state.saved_view,
                vc.view_models.clone(),
            );
            state.saved_current_turn = None;
            Some(vec![Effect::Render])
        }

        AcpEventData::TextChunk(tc) => {
            if let Some(turn) = state.saved_current_turn.as_mut() {
                turn.append_text(&tc.text);
                Some(vec![Effect::Render])
            } else {
                None
            }
        }

        AcpEventData::ReasoningChunk(rc) => {
            if let Some(turn) = state.saved_current_turn.as_mut() {
                turn.append_reasoning(&rc.text);
                Some(vec![Effect::Render])
            } else {
                None
            }
        }

        AcpEventData::ToolStarted(ts) => {
            if let Some(turn) = state.saved_current_turn.as_mut() {
                turn.start_tool(ToolCardAccumulator::new(
                    ts.tool_id.clone(),
                    ts.tool_name.clone(),
                    ts.input_summary.clone(),
                ));
                Some(vec![Effect::Render])
            } else {
                None
            }
        }

        AcpEventData::ToolEnded(te) => {
            if let Some(turn) = state.saved_current_turn.as_mut() {
                turn.end_tool(&te.tool_id, te.output_summary.clone(), te.is_error);
                Some(vec![Effect::Render])
            } else {
                None
            }
        }

        AcpEventData::TurnDone | AcpEventData::TurnInterrupted(_) => {
            // Turn ended while modal was open. Before clearing
            // saved_current_turn, flush its incremental view_models
            // (text / reasoning / tool cards accumulated during the modal
            // session) into saved_view — mirroring the streaming.rs
            // TurnInterrupted handler (line 98-99). Without this flush,
            // ClosePanel would restore saved_view that never received the
            // interrupted/done turn's tool cards or partial text, silently
            // losing any streaming progress made behind the popup.
            //
            // Note: a preceding ViewCommit would already have replaced
            // saved_view wholesale, so this extend is only load-bearing
            // when ViewCommit is delayed, lost, or never sent (interrupt).
            if let Some(turn) = state.saved_current_turn.as_mut() {
                let streaming_vms = turn.view_models().to_vec();
                state.saved_view.extend(streaming_vms);
                turn.deactivate();
            }
            state.saved_current_turn = None;
            Some(vec![Effect::Render])
        }

        // Status events / interaction requests / unknown — silently drop.
        // Status bar will display these separately; interaction requests
        // would replace the current modal (Phase 1.4 territory).
        _ => None,
    }
}

/// Map `PanelEffect`s to top-level `Effect`s.
///
/// - `SendToAcp` → forwarded to ACP transport via ApplyContext.
/// - `Copy` → clipboard write via ApplyContext.
/// - `ShowNotification` → main_loop injects into message area.
/// - `UpdateConfig` → main_loop persists to PeriConfig + syncs ACP Server.
/// - `SwitchSession` → main_loop asks App to switch sessions.
/// - `Close` → handled by caller (state transition), not emitted as Effect.
fn map_panel_effects(panel_effects: Vec<PanelEffect>) -> Vec<Effect> {
    panel_effects
        .into_iter()
        .filter_map(|pe| match pe {
            PanelEffect::SendToAcp { event, data } => Some(Effect::SendToAcp {
                method: event,
                params: data,
            }),
            PanelEffect::Copy(text) => Some(Effect::CopyToClipboard(text)),
            PanelEffect::ShowNotification(text) => Some(Effect::ShowNotification(text)),
            PanelEffect::UpdateConfig { key, value } => Some(Effect::UpdateConfig { key, value }),
            PanelEffect::SwitchSession(session_id) => Some(Effect::SwitchSession(session_id)),
            PanelEffect::OpenEditor { path } => Some(Effect::MemoryPanelOpenEditor { path }),
            PanelEffect::Close => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::panel_types::PanelKind;
    use crate::panel::registry::PanelStateStub;
    use crate::state_machine::handler::NoopHandler;
    use crate::state_machine::input::InputState;

    /// Build a ModalState for an interaction handler, with empty saved state.
    fn make_interaction_modal(
        handler: Box<dyn crate::state_machine::state::Handler>,
    ) -> ModalState {
        ModalState {
            saved_view: Vec::new(),
            saved_current_turn: None,
            saved_input: InputState::default(),
            saved_scroll_offset: 0,
            saved_history_index: None,
            kind: ModalKind::Interaction(handler),
        }
    }

    /// Build a ModalState for a panel, with empty saved state.
    fn make_panel_modal(panel: Box<dyn PanelState>) -> ModalState {
        ModalState {
            saved_view: Vec::new(),
            saved_current_turn: None,
            saved_input: InputState::default(),
            saved_scroll_offset: 0,
            saved_history_index: None,
            kind: ModalKind::Panel(panel),
        }
    }
    use crate::state_machine::PanelState;
    use ratatui::crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};

    /// 所有 14 个 PanelKind，用于全量回归测试。
    const ALL_PANEL_KINDS: [PanelKind; 14] = [
        PanelKind::Model,
        PanelKind::Login,
        PanelKind::Agent,
        PanelKind::Hooks,
        PanelKind::Config,
        PanelKind::ThreadBrowser,
        PanelKind::Mcp,
        PanelKind::Plugin,
        PanelKind::Cron,
        PanelKind::Status,
        PanelKind::Memory,
        PanelKind::Tasks,
        PanelKind::Betas,
        PanelKind::Workflow,
    ];

    #[test]
    fn test_esc_emits_close_panel() {
        // Esc in Modal now emits ClosePanel effect instead of directly
        // transitioning to Idle. The main_loop handles the transition
        // via saved_idle restoration, which preserves message history.
        let modal = make_interaction_modal(Box::new(NoopHandler));
        let (next, effects) = handle(
            modal,
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        );
        // State stays Modal; main_loop processes ClosePanel to restore saved_idle.
        assert!(matches!(next, State::Modal(_)));
        assert!(
            effects.iter().any(|e| matches!(e, Effect::ClosePanel)),
            "Esc should emit ClosePanel effect"
        );
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Render)),
            "Esc should emit Render effect"
        );
    }

    #[test]
    fn test_tick_keeps_background_processes_alive_in_modal() {
        // Tick 在 Modal 期间必须保持 PollAgent + AdvanceSpinner + Render，
        // 否则 ACP 事件消费停止、loading 动画冻结、面板不再重绘。
        let modal = make_interaction_modal(Box::new(NoopHandler));
        let (next, effects) = handle(modal, Event::Tick);
        assert!(matches!(next, State::Modal(_)));
        assert!(effects.iter().any(|e| matches!(e, Effect::PollAgent)));
        assert!(effects.iter().any(|e| matches!(e, Effect::AdvanceSpinner)));
        assert!(effects.iter().any(|e| matches!(e, Effect::Render)));
    }

    #[test]
    fn test_view_commit_updates_saved_view_in_modal() {
        // ViewCommit 在 Modal 期间必须更新 saved_view，否则关闭 popup
        // 时会回退到打开前的旧快照，丢失 agent 在 Modal 期间产出的消息。
        use peri_acp_types::event_data::ViewCommit;
        use peri_acp_types::view_model::{UserBubbleData, ViewModel};
        let mut modal = make_interaction_modal(Box::new(NoopHandler));
        modal.saved_view = vec![ViewModel::UserBubble(UserBubbleData {
            text: "old snapshot".into(),
        })];
        let new_view = vec![ViewModel::UserBubble(UserBubbleData {
            text: "new snapshot during modal".into(),
        })];
        let (next, effects) = handle(
            modal,
            Event::AcpEvent(AcpEventData::ViewCommit(ViewCommit {
                view_models: new_view.clone(),
            })),
        );
        match next {
            State::Modal(m) => {
                assert_eq!(m.saved_view.len(), 1);
                match &m.saved_view[0] {
                    ViewModel::UserBubble(data) => {
                        assert_eq!(data.text, "new snapshot during modal");
                    }
                    _ => panic!("expected UserBubble"),
                }
                assert!(m.saved_current_turn.is_none(), "ViewCommit clears turn");
            }
            _ => panic!("expected Modal"),
        }
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Render)),
            "ViewCommit in Modal should emit Render"
        );
    }

    #[test]
    fn test_status_events_silently_dropped_in_modal() {
        // TokenUsage 等状态事件在 Modal 期间不应更新 saved_view，
        // 由 status bar 独立显示。
        use peri_acp_types::event_data::TokenUsage;
        let modal = make_interaction_modal(Box::new(NoopHandler));
        let (next, effects) = handle(
            modal,
            Event::AcpEvent(AcpEventData::TokenUsage(TokenUsage {
                input: 100,
                output: 50,
            })),
        );
        assert!(matches!(next, State::Modal(_)));
        assert!(
            effects.is_empty(),
            "Status events in Modal should produce no effects"
        );
    }

    // ── Cron #24 P1 #2: PushUserBubble 推送到 saved_view ─────────────────

    #[test]
    fn test_push_userbubble_adds_to_saved_view_in_modal() {
        // Cron #24 P1 #2: Modal 期间到达的 AskUser 答案必须追加到
        // saved_view（ClosePanel 后恢复时可见），不 emit Render（modal
        // 已通过其它路径重绘）。
        use peri_acp_types::view_model::ViewModel;
        let modal = make_interaction_modal(Box::new(NoopHandler));
        let (next, effects) = handle(modal, Event::PushUserBubble("yes".into()));
        match next {
            State::Modal(m) => {
                assert_eq!(m.saved_view.len(), 1);
                match &m.saved_view[0] {
                    ViewModel::UserBubble(d) => {
                        assert_eq!(d.text, "yes");
                    }
                    other => panic!("expected UserBubble, got {other:?}"),
                }
            }
            other => panic!("expected Modal, got {other:?}"),
        }
        assert!(
            !effects.iter().any(|e| matches!(e, Effect::Render)),
            "PushUserBubble in Modal should not emit Render (mirrors PushSystemNote)"
        );
    }

    #[test]
    fn test_push_userbubble_preserves_prior_saved_view_in_modal() {
        // 已有 saved_view 时，PushUserBubble 应在末尾追加而非替换。
        use peri_acp_types::view_model::{UserBubbleData, ViewModel};
        let mut modal = make_interaction_modal(Box::new(NoopHandler));
        modal.saved_view = vec![ViewModel::UserBubble(UserBubbleData {
            text: "prior".into(),
        })];
        let (next, _) = handle(modal, Event::PushUserBubble("new".into()));
        match next {
            State::Modal(m) => {
                assert_eq!(m.saved_view.len(), 2);
                assert!(matches!(m.saved_view[0], ViewModel::UserBubble(_)));
                assert!(matches!(m.saved_view[1], ViewModel::UserBubble(_)));
            }
            other => panic!("expected Modal, got {other:?}"),
        }
    }

    #[test]
    fn test_text_chunk_without_saved_turn_is_dropped() {
        // 没有 saved_current_turn（Modal 从 Idle 打开）时，TextChunk 应静默丢弃。
        use peri_acp_types::event_data::TextChunk;
        let modal = make_interaction_modal(Box::new(NoopHandler));
        let (next, effects) = handle(
            modal,
            Event::AcpEvent(AcpEventData::TextChunk(TextChunk {
                text: "ignored".into(),
                agent_id: None,
            })),
        );
        assert!(matches!(next, State::Modal(_)));
        assert!(
            effects.is_empty(),
            "TextChunk without saved_current_turn should be dropped"
        );
    }

    #[test]
    fn test_text_chunk_appends_to_saved_turn_in_modal() {
        // Modal 从 Streaming 进入（saved_current_turn = Some）时，TextChunk
        // 必须追加到 saved_current_turn，而不是丢弃。这保证用户打开 Model
        // 面板查看配置期间，agent 流式输出仍然累积，ClosePanel 后能恢复。
        use crate::state_machine::current_turn::CurrentTurn;
        use peri_acp_types::event_data::TextChunk;
        let mut turn = CurrentTurn::new();
        turn.append_text("streaming-so-far");
        let mut modal = make_interaction_modal(Box::new(NoopHandler));
        modal.saved_current_turn = Some(turn);
        let (next, effects) = handle(
            modal,
            Event::AcpEvent(AcpEventData::TextChunk(TextChunk {
                text: " +more".into(),
                agent_id: None,
            })),
        );
        match next {
            State::Modal(m) => {
                let turn = m.saved_current_turn.expect("saved_current_turn preserved");
                assert_eq!(turn.text, "streaming-so-far +more");
            }
            _ => panic!("expected Modal"),
        }
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Render)),
            "TextChunk with saved_current_turn should emit Render"
        );
    }

    #[test]
    fn test_turn_done_clears_saved_turn_in_modal() {
        // Modal 期间 agent 完成（TurnDone）：saved_current_turn 应被清空，
        // 这样 ClosePanel 时 main_loop 检测到 saved_current_turn = None，
        // 恢复到 Idle 而非 Streaming（与 agent 已完成的事实一致）。
        use crate::state_machine::current_turn::CurrentTurn;
        let mut turn = CurrentTurn::new();
        turn.append_text("finishing");
        let mut modal = make_interaction_modal(Box::new(NoopHandler));
        modal.saved_current_turn = Some(turn);
        let (next, _effects) = handle(modal, Event::AcpEvent(AcpEventData::TurnDone));
        match next {
            State::Modal(m) => {
                assert!(
                    m.saved_current_turn.is_none(),
                    "TurnDone in Modal must clear saved_current_turn"
                );
            }
            _ => panic!("expected Modal"),
        }
    }

    #[test]
    fn test_turn_done_in_modal_flushes_text_to_saved_view() {
        // Modal 期间 agent 完成（TurnDone）：saved_current_turn 上累积的
        // 文本/工具卡片必须 flush 到 saved_view，否则 ClosePanel 恢复后
        // 用户看不到 popup 后面发生的流式输出。回归测试：修复前 saved_view
        // 不变，修复后必须包含 AssistantBubble（"finishing"）。
        use crate::state_machine::current_turn::CurrentTurn;
        let mut turn = CurrentTurn::new();
        turn.append_text("finishing");
        let mut modal = make_interaction_modal(Box::new(NoopHandler));
        modal.saved_view = vec![];
        modal.saved_current_turn = Some(turn);
        let (next, effects) = handle(modal, Event::AcpEvent(AcpEventData::TurnDone));
        match next {
            State::Modal(m) => {
                assert!(m.saved_current_turn.is_none());
                assert!(
                    !m.saved_view.is_empty(),
                    "TurnDone must flush saved_current_turn VMs into saved_view"
                );
                let has_finishing_text = m.saved_view.iter().any(|vm| match vm {
                    peri_acp_types::view_model::ViewModel::AssistantBubble(d) => {
                        d.text.contains("finishing")
                    }
                    _ => false,
                });
                assert!(
                    has_finishing_text,
                    "saved_view should contain the AssistantBubble with finishing text"
                );
                assert!(
                    effects.iter().any(|e| matches!(e, Effect::Render)),
                    "TurnDone in Modal must emit Render so popup background updates"
                );
            }
            _ => panic!("expected Modal"),
        }
    }

    #[test]
    fn test_turn_interrupted_in_modal_flushes_tool_cards_to_saved_view() {
        // TurnInterrupted in Modal 同样需要 flush。工具卡片场景特别重要：
        // streaming.rs TurnInterrupted 已经把工具卡片持久化到 state.view，
        // Modal 路径必须保持一致，否则 popup 关闭后已执行的工具调用消失。
        use crate::state_machine::current_turn::{CurrentTurn, ToolCardAccumulator};
        let mut turn = CurrentTurn::new();
        turn.append_text("partial");
        turn.start_tool(ToolCardAccumulator::new(
            "tool-1".to_string(),
            "Read".to_string(),
            "{path:\"f.txt\"}".to_string(),
        ));
        turn.end_tool("tool-1", "ok".to_string(), false);
        let mut modal = make_interaction_modal(Box::new(NoopHandler));
        modal.saved_view = vec![];
        modal.saved_current_turn = Some(turn);
        let (next, effects) = handle(
            modal,
            Event::AcpEvent(AcpEventData::TurnInterrupted(
                peri_acp_types::event_data::TurnInterrupted {
                    reason: "user-cancel".to_string(),
                },
            )),
        );
        match next {
            State::Modal(m) => {
                assert!(m.saved_current_turn.is_none());
                assert!(
                    !m.saved_view.is_empty(),
                    "TurnInterrupted must flush saved_current_turn VMs into saved_view"
                );
                let has_tool = m
                    .saved_view
                    .iter()
                    .any(|vm| matches!(vm, peri_acp_types::view_model::ViewModel::ToolCard(_)));
                assert!(
                    has_tool,
                    "saved_view should contain the ToolCard from interrupted turn"
                );
                assert!(
                    effects.iter().any(|e| matches!(e, Effect::Render)),
                    "TurnInterrupted in Modal must emit Render"
                );
            }
            _ => panic!("expected Modal"),
        }
    }

    #[test]
    fn test_turn_done_in_modal_without_saved_current_turn_is_noop() {
        // Modal 从 Idle 打开时 saved_current_turn = None。TurnDone 到达时
        // 不应 panic，不应修改 saved_view，但仍 emit Render（与有数据时一致）。
        let mut modal = make_interaction_modal(Box::new(NoopHandler));
        modal.saved_view = vec![];
        modal.saved_current_turn = None;
        let (next, effects) = handle(modal, Event::AcpEvent(AcpEventData::TurnDone));
        match next {
            State::Modal(m) => {
                assert!(m.saved_current_turn.is_none());
                assert!(
                    m.saved_view.is_empty(),
                    "saved_view must remain empty when no saved_current_turn"
                );
                assert!(
                    effects.iter().any(|e| matches!(e, Effect::Render)),
                    "TurnDone in Modal should still emit Render even without turn data"
                );
            }
            _ => panic!("expected Modal"),
        }
    }

    #[test]
    fn test_panel_close_emits_close_panel_effect() {
        // Betas panel + Esc -> ClosePanel effect (not direct Idle transition).
        // main_loop restores saved_idle to preserve message history.
        let panel = Box::new(PanelStateStub::new(PanelKind::Betas)) as Box<dyn PanelState>;
        let modal = make_panel_modal(panel);
        let (next, effects) = handle(
            modal,
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        );
        assert!(matches!(next, State::Modal(_)));
        assert!(effects.iter().any(|e| matches!(e, Effect::ClosePanel)));
        assert!(effects.iter().any(|e| matches!(e, Effect::Render)));
    }

    // -----------------------------------------------------------------------
    // 全量面板回归测试（14 个 PanelKind × 关键事件）
    // -----------------------------------------------------------------------

    #[test]
    fn test_all_14_panels_esc_emits_close_panel() {
        // 每个 v2 面板按 Esc 必须 emit ClosePanel 效果（而非直接 Idle 转换），
        // 这样 main_loop 才能通过 saved_idle 恢复消息历史。
        // 这是面板关闭契约的基础保证 —— 用户不会丢失消息数据。
        for kind in ALL_PANEL_KINDS {
            let panel = Box::new(PanelStateStub::new(kind)) as Box<dyn PanelState>;
            let modal = make_panel_modal(panel);
            let (next, effects) = handle(
                modal,
                Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            );
            assert!(
                matches!(next, State::Modal(_)),
                "Esc on {kind:?} should keep Modal state, got {next:?}"
            );
            assert!(
                effects.iter().any(|e| matches!(e, Effect::ClosePanel)),
                "Esc on {kind:?} should emit ClosePanel effect"
            );
            assert!(
                effects.iter().any(|e| matches!(e, Effect::Render)),
                "Esc on {kind:?} should emit Render effect"
            );
        }
    }

    #[test]
    fn test_all_14_panels_tick_keeps_background_alive() {
        // Tick 在面板打开期间必须保持后台进程活跃（PollAgent/AdvanceSpinner/Render）。
        for kind in ALL_PANEL_KINDS {
            let panel = Box::new(PanelStateStub::new(kind)) as Box<dyn PanelState>;
            let modal = make_panel_modal(panel);
            let (next, effects) = handle(modal, Event::Tick);
            assert!(
                matches!(next, State::Modal(_)),
                "Tick on {kind:?} should keep Modal state, got {next:?}"
            );
            assert!(
                effects.iter().any(|e| matches!(e, Effect::PollAgent)),
                "Tick on {kind:?} should emit PollAgent"
            );
            assert!(
                effects.iter().any(|e| matches!(e, Effect::AdvanceSpinner)),
                "Tick on {kind:?} should emit AdvanceSpinner"
            );
            assert!(
                effects.iter().any(|e| matches!(e, Effect::Render)),
                "Tick on {kind:?} should emit Render"
            );
        }
    }

    #[test]
    fn test_all_14_panels_resize_triggers_render() {
        // Resize 必须触发重绘以重新计算面板布局。
        for kind in ALL_PANEL_KINDS {
            let panel = Box::new(PanelStateStub::new(kind)) as Box<dyn PanelState>;
            let modal = make_panel_modal(panel);
            let (next, effects) = handle(
                modal,
                Event::Resize {
                    width: 100,
                    height: 40,
                },
            );
            assert!(
                matches!(next, State::Modal(_)),
                "Resize on {kind:?} should keep Modal state"
            );
            assert!(
                effects.iter().any(|e| matches!(e, Effect::Render)),
                "Resize on {kind:?} should emit Render effect"
            );
        }
    }

    #[test]
    fn test_all_14_panels_mouse_triggers_render() {
        // Mouse 事件应该触发重绘（高亮、悬停状态可能改变）。
        let mouse_event = MouseEvent {
            kind: MouseEventKind::Moved,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        for kind in ALL_PANEL_KINDS {
            let panel = Box::new(PanelStateStub::new(kind)) as Box<dyn PanelState>;
            let modal = make_panel_modal(panel);
            let (next, effects) = handle(modal, Event::Mouse(mouse_event));
            assert!(
                matches!(next, State::Modal(_)),
                "Mouse on {kind:?} should keep Modal state"
            );
            assert!(
                effects.iter().any(|e| matches!(e, Effect::Render)),
                "Mouse on {kind:?} should emit Render effect"
            );
        }
    }

    #[test]
    fn test_all_14_panels_acp_disconnected_is_noop() {
        // ACP 断开时，面板状态保持不变 —— 断开由 main_loop 上层处理。
        for kind in ALL_PANEL_KINDS {
            let panel = Box::new(PanelStateStub::new(kind)) as Box<dyn PanelState>;
            let modal = make_panel_modal(panel);
            let (next, effects) = handle(modal, Event::AcpDisconnected);
            assert!(
                matches!(next, State::Modal(_)),
                "AcpDisconnected on {kind:?} should keep Modal state"
            );
            assert!(
                effects.is_empty(),
                "AcpDisconnected on {kind:?} should emit no effects"
            );
        }
    }

    #[test]
    fn test_all_14_panels_shutdown_emits_quit() {
        // Shutdown 在 Modal 状态下必须 emit Quit，确保用户 Ctrl+C 时可以退出。
        for kind in ALL_PANEL_KINDS {
            let panel = Box::new(PanelStateStub::new(kind)) as Box<dyn PanelState>;
            let modal = make_panel_modal(panel);
            let (next, effects) = handle(modal, Event::Shutdown);
            assert!(
                matches!(next, State::Modal(_)),
                "Shutdown on {kind:?} should keep Modal state"
            );
            assert!(
                effects.iter().any(|e| matches!(e, Effect::Quit)),
                "Shutdown on {kind:?} should emit Quit effect"
            );
        }
    }

    // -----------------------------------------------------------------------
    // PanelEffect → Effect 映射测试（核心数据流）
    // -----------------------------------------------------------------------

    #[test]
    fn test_map_panel_effects_close_filtered_out() {
        // Close 不应该作为 Effect 传播 —— 它通过状态转换（→ Idle）处理。
        let panel_effects = vec![PanelEffect::Close];
        let mapped = map_panel_effects(panel_effects);
        assert!(mapped.is_empty(), "Close should be filtered out");
    }

    #[test]
    fn test_map_panel_effects_send_to_acp() {
        let panel_effects = vec![PanelEffect::SendToAcp {
            event: "set_model".to_string(),
            data: serde_json::json!({"alias": "sonnet"}),
        }];
        let mapped = map_panel_effects(panel_effects);
        assert_eq!(mapped.len(), 1);
        match &mapped[0] {
            Effect::SendToAcp { method, params } => {
                assert_eq!(method, "set_model");
                assert_eq!(params["alias"], "sonnet");
            }
            other => panic!("expected SendToAcp, got {other:?}"),
        }
    }

    #[test]
    fn test_map_panel_effects_copy() {
        let panel_effects = vec![PanelEffect::Copy("hello".to_string())];
        let mapped = map_panel_effects(panel_effects);
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0], Effect::CopyToClipboard("hello".to_string()));
    }

    #[test]
    fn test_map_panel_effects_show_notification() {
        let panel_effects = vec![PanelEffect::ShowNotification("saved".to_string())];
        let mapped = map_panel_effects(panel_effects);
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0], Effect::ShowNotification("saved".to_string()));
    }

    #[test]
    fn test_map_panel_effects_update_config() {
        let panel_effects = vec![PanelEffect::UpdateConfig {
            key: "model".to_string(),
            value: "opus".to_string(),
        }];
        let mapped = map_panel_effects(panel_effects);
        assert_eq!(mapped.len(), 1);
        match &mapped[0] {
            Effect::UpdateConfig { key, value } => {
                assert_eq!(key, "model");
                assert_eq!(value, "opus");
            }
            other => panic!("expected UpdateConfig, got {other:?}"),
        }
    }

    #[test]
    fn test_map_panel_effects_switch_session() {
        let panel_effects = vec![PanelEffect::SwitchSession("sess_123".to_string())];
        let mapped = map_panel_effects(panel_effects);
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0], Effect::SwitchSession("sess_123".to_string()));
    }

    #[test]
    fn test_map_panel_effects_mixed_batch() {
        // 模拟 ModelPanel::apply_effects 的真实批量输出。
        let panel_effects = vec![
            PanelEffect::UpdateConfig {
                key: "model".to_string(),
                value: "sonnet".to_string(),
            },
            PanelEffect::SendToAcp {
                event: "set_model".to_string(),
                data: serde_json::json!({}),
            },
            PanelEffect::ShowNotification("switched".to_string()),
            PanelEffect::Close,
        ];
        let mapped = map_panel_effects(panel_effects);
        // Close 被过滤，剩 3 个。
        assert_eq!(mapped.len(), 3);
        assert!(matches!(mapped[0], Effect::UpdateConfig { .. }));
        assert!(matches!(mapped[1], Effect::SendToAcp { .. }));
        assert!(matches!(mapped[2], Effect::ShowNotification(_)));
    }

    #[test]
    fn test_map_panel_effects_empty() {
        let mapped = map_panel_effects(Vec::new());
        assert!(mapped.is_empty());
    }

    // -----------------------------------------------------------------------
    // Handler (Interaction) 测试
    // -----------------------------------------------------------------------

    #[test]
    fn test_handler_submit_closes_modal() {
        // NoopHandler 对任何字符都返回 Nothing，所以应该保持 Modal。
        let modal = make_interaction_modal(Box::new(NoopHandler));
        let (next, _effects) = handle(
            modal,
            Event::Key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
        );
        // NoopHandler 返回 Nothing，所以保持 Modal。
        assert!(matches!(next, State::Modal(_)));
    }

    #[test]
    fn test_handler_esc_emits_close_panel() {
        let modal = make_interaction_modal(Box::new(NoopHandler));
        let (next, effects) = handle(
            modal,
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        );
        assert!(matches!(next, State::Modal(_)));
        assert!(
            effects.iter().any(|e| matches!(e, Effect::ClosePanel)),
            "Esc on interaction should emit ClosePanel"
        );
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Render)),
            "Esc on interaction should emit Render"
        );
    }

    // -----------------------------------------------------------------------
    // transition_to_idle_with_effects 单元测试
    // -----------------------------------------------------------------------

    #[test]
    fn test_transition_to_idle_with_effects_adds_render_if_missing() {
        let effects = vec![Effect::CopyToClipboard("x".to_string())];
        let (next, all_effects) = transition_to_idle_with_effects(effects);
        assert!(matches!(next, State::Idle(_)));
        // 自动补一个 Render。
        assert!(
            all_effects.iter().any(|e| matches!(e, Effect::Render)),
            "should auto-add Render if missing"
        );
        assert_eq!(all_effects.len(), 2);
    }

    #[test]
    fn test_transition_to_idle_with_effects_preserves_existing_render() {
        let effects = vec![Effect::Render];
        let (next, all_effects) = transition_to_idle_with_effects(effects);
        assert!(matches!(next, State::Idle(_)));
        // 已有 Render，不再补。
        assert_eq!(all_effects.len(), 1);
    }

    #[test]
    fn test_transition_to_idle_basic() {
        let (next, effects) = transition_to_idle();
        assert!(matches!(next, State::Idle(_)));
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], Effect::Render));
    }
}
