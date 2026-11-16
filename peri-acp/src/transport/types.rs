//! Core transport types for ACP JSON-RPC 2.0 communication.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC request/response identifier.
///
/// Mirrors the ACP spec: can be a string or number.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    String(String),
    Number(i64),
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RequestId::String(s) => write!(f, "{s}"),
            RequestId::Number(n) => write!(f, "{n}"),
        }
    }
}

/// An incoming JSON-RPC 2.0 message from the transport.
#[derive(Debug)]
pub enum IncomingMessage {
    /// A request that expects a response.
    Request {
        id: RequestId,
        method: String,
        params: Value,
    },
    /// A notification that does not expect a response.
    Notification { method: String, params: Value },
    /// A response to a previous request.
    Response {
        id: RequestId,
        result: Result<Value, AcpError>,
    },
}

/// ACP transport-level error, compatible with JSON-RPC 2.0 error objects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl AcpError {
    #[must_use]
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

impl fmt::Display for AcpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ACP error [{}]: {}", self.code, self.message)
    }
}

impl std::error::Error for AcpError {}
