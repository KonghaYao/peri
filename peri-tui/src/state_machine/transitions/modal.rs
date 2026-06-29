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
use crate::panel::read_context::ServiceRegistrySnapshot;
use crate::runtime::effect::Effect;

/// Modal-state transition entry point.
pub fn handle(mut state: ModalState, event: Event) -> (State, Vec<Effect>) {
    match event {
        // -- Esc: dismiss popup -> back to Idle ------------------------------
        Event::Key(KeyEvent {
            code: KeyCode::Esc, ..
        }) => {
            let idle = IdleState {
                input: InputState::default(),
                scroll_offset: 0,
                view: vec![],
                double_esc_timer: None,
                history_index: None,
            };
            (State::Idle(idle), vec![Effect::Render])
        }

        // -- Other key events: delegate to the panel/handler -----------------
        Event::Key(key) => {
            let effects = dispatch_key(&mut state, key);
            // The handler keeps its own internal state -- we keep Modal as-is.
            // P3 will inspect `HandlerOutput::Submit/Dismiss` and transition.
            (State::Modal(state), effects)
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

/// Dispatch a single key to the active panel / handler and collect effects.
fn dispatch_key(state: &mut ModalState, key: KeyEvent) -> Vec<Effect> {
    // P2 stub: build an empty context from thread-local storage.
    // P3 will construct from real state machine data.
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
                            map_panel_effects(panel_effects)
                        }
                        ModalState::Interaction(handler) => {
                            if let KeyCode::Char(c) = key.code {
                                match handler.handle_key(c) {
                                    HandlerOutput::Nothing => Vec::new(),
                                    HandlerOutput::Submit(_) | HandlerOutput::Dismiss => Vec::new(),
                                }
                            } else {
                                Vec::new()
                            }
                        }
                    }
                })
            })
        })
    })
}

/// Map `PanelEffect`s to top-level `Effect`s.
fn map_panel_effects(panel_effects: Vec<super::super::state::PanelEffect>) -> Vec<Effect> {
    panel_effects
        .into_iter()
        .filter_map(|pe| match pe {
            super::super::state::PanelEffect::SendToAcp { event, data } => {
                Some(Effect::SendToAcp {
                    method: event,
                    params: data,
                })
            }
            super::super::state::PanelEffect::Copy(text) => Some(Effect::CopyToClipboard(text)),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
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
}
