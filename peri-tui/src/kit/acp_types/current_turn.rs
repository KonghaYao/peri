//! Canonical state and lifecycle for one streaming turn.

mod archive;
mod notes;
mod preview;
mod projection;
mod streaming;
mod subagents;

#[expect(
    clippy::module_inception,
    reason = "The style guide keeps the facade declarative and names the implementation leaf after its primary type."
)]
mod current_turn;
pub use current_turn::*;
