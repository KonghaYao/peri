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
//! [`ConnectionTo`] handle (stdio production path), so broker assembly is
//! transport-agnostic.

pub mod mpsc;
pub mod router;
pub mod stdio;
pub mod types;

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use types::{AcpError, IncomingMessage, RequestId};

/// Bidirectional ACP JSON-RPC 2.0 transport.
///
/// Implementations are responsible for serializing/deserializing messages
/// to/from the underlying transport (mpsc channels, stdio, WebSocket, etc.).
#[async_trait]
pub trait AcpTransport: Send + Sync {
    /// Send a request and wait for a response.
    ///
    /// Connected silence has no built-in timeout. Cancelling the caller releases
    /// its pending registration; terminal stdio/MPSC closure settles current and
    /// future requests with `AcpError(-32603, "Transport closed")`.
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError>;

    /// Send a notification (fire-and-forget, no response expected).
    async fn send_notification(&self, method: &str, params: Value) -> Result<(), AcpError>;

    /// Receive the next incoming message, or `None` if the transport is closed.
    async fn recv(&self) -> Option<IncomingMessage>;

    /// Send a response to a previously-received request.
    async fn send_response(
        &self,
        id: RequestId,
        result: Result<Value, AcpError>,
    ) -> Result<(), AcpError>;
}

/// Minimal send-only transport contract (server→client request round-trip).
///
/// This is the narrowest slice the interaction broker needs. It exists because
/// the ACP SDK `ConnectionTo<Client>` handle (stdio production path) is a
/// callback/push model and cannot implement the full pull-model
/// [`AcpTransport`] (`recv`/`send_response`); the broker therefore depends on
/// this trait so both mpsc and stdio paths share one broker.
#[async_trait]
pub trait RequestTransport: Send + Sync {
    /// Send a request and wait for a response.
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError>;
}

#[async_trait]
impl<T: AcpTransport + ?Sized> RequestTransport for T {
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        AcpTransport::send_request(self, method, params).await
    }
}

/// `Arc<dyn AcpTransport>` → `Arc<dyn RequestTransport>` 的显式桥。
///
/// `dyn AcpTransport: RequestTransport` 经 blanket impl 成立，但 unsize
/// coercion 依赖 supertrait 关系而非 trait 满足，故 mpsc 装配点
/// （`host/prompt.rs`）需经本结构体显式转换。
pub struct AcpRequestBridge(pub Arc<dyn AcpTransport>);

#[async_trait]
impl RequestTransport for AcpRequestBridge {
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        AcpTransport::send_request(self.0.as_ref(), method, params).await
    }
}

/// ACP SDK stdio 模式的 `ConnectionTo<Client>` 适配：直接复用 SDK 的
/// server→client request 通道（`send_request` + `block_task`，dispatch loop
/// 之外使用），与 mpsc 路径的 [`AcpTransport`] 实现共享同一 broker。
#[async_trait]
impl RequestTransport for agent_client_protocol::ConnectionTo<agent_client_protocol::Client> {
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        let message = agent_client_protocol::UntypedMessage::new(method, params)
            .map_err(|e| AcpError::new(-32600, format!("Invalid request: {e}")))?;
        let response = self
            .send_request(message)
            .block_task()
            .await
            .map_err(|e| AcpError::new(-32603, format!("Request failed: {e}")))?;
        Ok(response)
    }
}
