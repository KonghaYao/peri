//! Thin TUI-side wrapper around [`peri_acp::transport::mpsc::MpscClientTransport`].
//!
//! Translates raw [`peri_acp::transport::types::IncomingMessage`]s into [`AcpNotification`]s for the TUI event
//! loop to consume. The notification pump runs as a background tokio task.

mod child_history;
mod history_page;
mod interaction;
mod pump;
mod recovery_view;
mod requests;
mod session;
mod session_load_reservation;
mod steer;
mod workspace;

#[cfg(test)]
#[path = "client_test.rs"]
mod tests;

#[cfg(test)]
#[path = "client_reverse_test.rs"]
mod reverse_tests;

#[cfg(test)]
#[path = "client/child_origin_test.rs"]
mod child_origin_tests;

#[cfg(test)]
#[path = "client/history_v2_test.rs"]
mod history_v2_tests;

#[cfg(test)]
#[path = "client/history_v2_session_test.rs"]
mod history_v2_session_tests;

#[cfg(test)]
use peri_acp::event::AcpEvent;

#[cfg(test)]
use super::interaction_lifecycle::InteractionLifecycle;
#[cfg(test)]
use super::interaction_lifecycle::InteractionUiOutcome;
#[cfg(test)]
use tokio::sync::mpsc;

mod acp_tui_client;
pub use acp_tui_client::*;

pub(crate) use super::interaction_lifecycle::AdmissionRoute;
pub(crate) use session::AdmissionGuard;
pub(crate) use session_load_reservation::SessionLoadReservation;
