//! TUI-private semantic ownership for reverse interactions.
//!
//! Wire request ids identify JSON-RPC frames. [`InteractionOwner`] identifies
//! the one UI operation that may settle a reverse request.  Session boundaries,
//! prompt terminals, bridge publication and user responses all compete for that
//! same owner in this module tree.

mod leases;
mod ordinary;
mod prompt;
mod reverse;
mod session_route;

#[cfg(test)]
#[path = "interaction_lifecycle_test.rs"]
mod tests;

#[cfg(test)]
use super::client::AcpNotification;

mod interaction_registry;
pub use interaction_registry::*;

pub use leases::{ClaimedBatchLease, ClaimedSettlementLease, PromptLease, TransitionLease};
