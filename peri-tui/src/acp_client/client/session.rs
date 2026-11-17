//! Session transitions and queue-side load reservations under the lifecycle gate.

mod admission;
mod close;
mod delete;
mod guards;
mod reservation;
mod startup;
mod transition;

#[cfg(test)]
#[path = "recovery_test.rs"]
mod recovery_tests;

#[cfg(test)]
use super::AcpTuiClient;

mod session_transition;

pub(crate) use admission::AdmissionGuard;
pub(in crate::acp_client::client) use close::PendingClose;
