//! Interaction broker — bridges HITL and AskUser to ACP RPC.
//!
//! Implements [`UserInteractionBroker`](peri_acp_types::interaction::UserInteractionBroker) trait,
//! translating approval requests into `RequestPermission` RPC and user questions
//! into `elicitation/create` RPC via an [`AcpTransport`](crate::transport::AcpTransport).

pub mod transport_broker;
pub use transport_broker::*;
