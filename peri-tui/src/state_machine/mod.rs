//! v2 state machine -- pure function `(State, Event) -> (State, Vec<Effect>)`.
//!
//! This module is the heart of the v2 TUI architecture. It holds all mutable
//! state (ViewModel list, input box, scroll offset, current turn, panels) and
//! transforms it via pure functions that take an event and produce a new state
//! plus a list of side-effect instructions (`Effect`).
//!
//! The state machine never performs I/O -- no terminal access, no network calls,
//! no file reads, no clipboard writes. It is fully testable in isolation.
//!
//! Reference: `docs/design/peri-tui-architecture.md` section 8.

pub mod current_turn;
pub mod event;
pub mod handler;
pub mod handlers;
pub mod input;
pub mod state;
pub mod transitions;
pub mod view_store;

#[cfg(test)]
mod input_test;

// Re-export the primary types at the state_machine level.
pub use current_turn::CurrentTurn;
pub use event::{AcpEventData, Event};
pub use state::{
    DoubleEscTracker, Handler, HandlerOutput, IdleState, InputState, ModalState, PanelEffect,
    PanelReadContext, PanelState, State, StreamingState, SwitchingState,
};
pub use view_store::ViewStore;

/// Pure-function state machine entry point.
///
/// Dispatches by `State` variant to the matching transition module
/// (`transitions::idle`, `transitions::streaming`, ...). Each transition is
/// itself a pure function `(XState, Event) -> (State, Vec<Effect>)`.
///
/// The v2 main loop (`runtime::main_loop::run`) drives this as the primary
/// event handler (path 1a), with keyboard fallback (path 1b) for complex UI
/// interactions not yet ported to pure transitions.
pub fn handle(state: State, event: Event) -> (State, Vec<crate::runtime::effect::Effect>) {
    match state {
        State::Idle(s) => transitions::idle::handle(s, event),
        State::Streaming(s) => transitions::streaming::handle(s, event),
        State::Modal(s) => transitions::modal::handle(s, event),
        State::Switching(s) => transitions::switching::handle(s, event),
    }
}
