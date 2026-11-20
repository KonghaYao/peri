//! Gateway-side [`crate::service::SwarmService`] backed by Rivet actors (S5).
//!
//! The gateway never owns session state: it maps `logicalId` to the session row
//! (`rootSessionId`) through [`SwarmStore`], routes typed actions through the
//! Rivet client, and re-emits the actor's event stream as the public SSE feed.
//! Requests are never served from process memory.

mod rivet_service;

pub use rivet_service::{RivetServiceConfig, RivetSwarmService};
