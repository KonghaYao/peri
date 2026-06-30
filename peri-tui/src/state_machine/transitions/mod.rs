//! State-machine transition functions -- one module per top-level state.
//!
//! Each transition is a pure function:
//!
//! ```text
//! pub fn handle(state: XState, event: Event) -> (State, Vec<Effect>)
//! ```
//!
//! The top-level [`crate::state_machine::handle`] dispatches by `State`
//! variant to the matching transition module.
//!
//! Reference: `docs/design/peri-tui-architecture.md` section 8.6
//! (transition table).

pub mod idle;
pub mod modal;
pub mod streaming;
pub mod switching;

use crate::runtime::effect::Effect;
use crate::state_machine::state::{
    Handler, IdleState, ModalKind, ModalState, State, StreamingState,
};

/// Enter `State::Modal` from `State::Idle`, wrapping an agent-initiated
/// interaction handler (HITL / AskUser / Rewind / OAuth).
///
/// Extracts the Idle state's fields into `ModalState.saved_*` so closing
/// the popup restores the original view / input / scroll / history /
/// double-Esc tracker. `saved_current_turn` is `None` because Idle has
/// no active turn.
///
/// This is the v2 interaction entry path: instead of routing through an
/// `Effect::OpenInteraction(Box<dyn Handler>)` (which would require
/// removing `Clone + PartialEq` from `Effect`), we transition directly.
/// The handler is constructed in the transition arm and consumed by the
/// `ModalState`.
pub(crate) fn enter_modal_from_idle(
    state: IdleState,
    handler: Box<dyn Handler>,
) -> (State, Vec<Effect>) {
    let modal = ModalState {
        saved_view: state.view,
        saved_current_turn: None,
        saved_input: state.input,
        saved_scroll_offset: state.scroll_offset,
        saved_history_index: state.history_index,
        saved_double_esc_timer: state.double_esc_timer,
        kind: ModalKind::Interaction(handler),
    };
    (State::Modal(modal), vec![Effect::Render])
}

/// Enter `State::Modal` from `State::Streaming`, wrapping an agent-initiated
/// interaction handler.
///
/// Same as [`enter_modal_from_idle`] but preserves the in-progress
/// `CurrentTurn` via `saved_current_turn = Some(...)` so streaming output
/// accumulating while the popup is open is not lost. Streaming has no
/// double-Esc tracker / history navigation, so those are `None`.
pub(crate) fn enter_modal_from_streaming(
    state: StreamingState,
    handler: Box<dyn Handler>,
) -> (State, Vec<Effect>) {
    let modal = ModalState {
        saved_view: state.view,
        saved_current_turn: Some(state.current_turn),
        saved_input: state.input,
        saved_scroll_offset: state.scroll_offset,
        saved_history_index: None,
        saved_double_esc_timer: None,
        kind: ModalKind::Interaction(handler),
    };
    (State::Modal(modal), vec![Effect::Render])
}
