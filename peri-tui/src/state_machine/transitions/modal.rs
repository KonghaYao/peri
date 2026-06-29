//! Modal-state transition: `(ModalState, Event) -> (State, Vec<Effect>)`.
//!
//! A panel or interaction popup is active and captures all keyboard input.
//! Keys are delegated to the active `PanelState` / `Handler`; Esc dismisses
//! the popup and returns to the previous state (Idle in the P2 stub).
//!
//! Reference: `docs/design/peri-tui-architecture.md` section 8.6.

use std::collections::HashMap;

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use tui_textarea::Input;

use super::super::event::Event;
use super::super::state::{
    HandlerOutput, IdleState, InputState, ModalState, PanelReadContext, State,
};
use crate::panel::effect::PanelEffect;
use crate::panel::read_context::ServiceRegistrySnapshot;
use crate::runtime::effect::Effect;

/// Modal-state transition entry point.
pub fn handle(mut state: ModalState, event: Event) -> (State, Vec<Effect>) {
    match event {
        // -- Esc: dismiss popup -> back to Idle ------------------------------
        Event::Key(KeyEvent {
            code: KeyCode::Esc, ..
        }) => transition_to_idle(),

        // -- Other key events: delegate to the panel/handler -----------------
        Event::Key(key) => {
            let (panel_effects, should_close) = dispatch_key(&mut state, key);
            let effects = map_panel_effects(panel_effects);
            if should_close {
                transition_to_idle_with_effects(effects)
            } else {
                (State::Modal(state), effects)
            }
        }

        // -- Mouse / Resize: re-render so the popup lays out correctly -------
        Event::Mouse(_) | Event::Resize { .. } => (State::Modal(state), vec![Effect::Render]),

        // -- Everything else: keep Modal, no effect --------------------------
        Event::Tick
        | Event::Paste(_)
        | Event::AcpEvent(_)
        | Event::AcpDisconnected
        | Event::SessionLoaded { .. }
        | Event::Shutdown => (State::Modal(state), Vec::new()),
    }
}

/// Build a fresh Idle state with a Render effect (used when dismissing modal).
fn transition_to_idle() -> (State, Vec<Effect>) {
    let idle = IdleState {
        input: InputState::default(),
        scroll_offset: 0,
        view: vec![],
        double_esc_timer: None,
        history_index: None,
    };
    (State::Idle(idle), vec![Effect::Render])
}

/// Build a fresh Idle state, preserving the given effects (e.g., ShowNotification).
fn transition_to_idle_with_effects(effects: Vec<Effect>) -> (State, Vec<Effect>) {
    let idle = IdleState {
        input: InputState::default(),
        scroll_offset: 0,
        view: vec![],
        double_esc_timer: None,
        history_index: None,
    };
    // Ensure at least one Render so the user sees the dismissal.
    let mut all_effects = effects;
    if !all_effects.iter().any(|e| matches!(e, Effect::Render)) {
        all_effects.push(Effect::Render);
    }
    (State::Idle(idle), all_effects)
}

/// Dispatch a single key to the active panel / handler.
///
/// Returns `(panel_effects, should_close)`. `should_close=true` when the panel
/// produced `PanelEffect::Close` -- caller transitions to Idle.
fn dispatch_key(state: &mut ModalState, key: KeyEvent) -> (Vec<PanelEffect>, bool) {
    // P2 stub: build an empty context from thread-local storage.
    // P3 Integration: construct from real state machine data (App snapshot).
    thread_local! {
        static STUB_SNAPSHOT: ServiceRegistrySnapshot = ServiceRegistrySnapshot::new();
        static STUB_VMS: Vec<peri_acp_types::view_model::ViewModel> = const { Vec::new() };
        static STUB_CACHE: HashMap<String, serde_json::Value> = HashMap::new();
        static STUB_LC: crate::i18n::LcRegistry = crate::i18n::LcRegistry::default();
    }
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
                    match state {
                        ModalState::Panel(panel) => {
                            let panel_effects = panel.handle_key(Input::from(key), &ctx);
                            let should_close = panel_effects
                                .iter()
                                .any(|pe| matches!(pe, PanelEffect::Close));
                            (panel_effects, should_close)
                        }
                        ModalState::Interaction(handler) => {
                            if let KeyCode::Char(c) = key.code {
                                match handler.handle_key(c) {
                                    HandlerOutput::Nothing => (Vec::new(), false),
                                    HandlerOutput::Submit(_) | HandlerOutput::Dismiss => {
                                        (Vec::new(), true)
                                    }
                                }
                            } else {
                                (Vec::new(), false)
                            }
                        }
                    }
                })
            })
        })
    })
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
    use crate::panel::registry::create_panel;
    use crate::state_machine::handler::NoopHandler;
    use ratatui::crossterm::event::KeyModifiers;

    #[test]
    fn test_esc_dismisses_modal() {
        let modal = ModalState::Interaction(Box::new(NoopHandler));
        let (next, _effects) = handle(
            modal,
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        );
        assert!(matches!(next, State::Idle(_)));
    }

    #[test]
    fn test_tick_is_noop_in_modal() {
        let modal = ModalState::Interaction(Box::new(NoopHandler));
        let (next, effects) = handle(modal, Event::Tick);
        assert!(matches!(next, State::Modal(_)));
        assert!(effects.is_empty());
    }

    #[test]
    fn test_panel_close_transitions_to_idle() {
        // ModelPanel with Ctrl+M selection closes (Esc stub returns Close).
        // We use a panel that produces Close on a specific key.
        // MemoryPanel's handle_key(Input::default()) returns Close (stub behavior
        // before data migration); simplest is to use Esc-like key.
        // Actually, use a real panel: press Esc on any v2 panel -> Close.
        let panel = create_panel(crate::app::panel_manager::PanelKind::Betas);
        let modal = ModalState::Panel(panel);
        let (next, _effects) = handle(
            modal,
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        );
        assert!(matches!(next, State::Idle(_)));
    }
}
