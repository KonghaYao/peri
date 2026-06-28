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
