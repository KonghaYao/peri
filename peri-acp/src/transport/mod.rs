//! Transport abstraction for ACP JSON-RPC 2.0 bidirectional communication.
//!
//! The [`AcpTransport`] trait provides a unified interface for sending requests,
//! notifications, and responses, and for receiving incoming messages. Two
//! implementations are provided:
//!
//! - [`MpscTransport`](mpsc) — in-memory channel pair for TUI ↔ ACP Server
//! - [`StdioTransport`](stdio) — stdio-based transport for external IDE clients
//!
//! The [`RequestTransport`] trait is the minimal send-only slice used by the
//! interaction broker ([`AcpTransportBroker`](crate::broker)); it is
//! implemented blanket for all [`AcpTransport`]s and directly for the ACP SDK
//! `ConnectionTo` handle (stdio production path), so broker assembly is
//! transport-agnostic.

pub mod mpsc;
pub mod request_router;
pub use request_router as router;
pub mod stdio_transport;
pub use stdio_transport as stdio;
pub mod types;
pub mod wire_bridge;

mod acp_transport;

use acp_transport::wait_cancellable;
pub use acp_transport::{AcpRequestBridge, AcpTransport, RequestTransport};
