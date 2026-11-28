//! The single mapping point for ACP v2 error codes.
//!
//! The pinned v2 schema reserves `-32000` for **Authentication required**, so the
//! preview never emits it: capacity and backpressure use Peri's own server error
//! `-32020` with a stable `data.reason`, and unknown sessions use `-32002` with
//! `data.sessionId` (`SessionNotFoundErrorData`). Every v2 error is constructed
//! here so the mapping stays reviewable in one place.

use serde_json::{json, Value};

use crate::transport::types::AcpError;

/// Method not found or not advertised on this connection.
pub(super) const METHOD_NOT_FOUND: i64 = -32601;
/// Invalid params: bad content block, missing/relative cwd, bad cursor, bad key.
pub(super) const INVALID_PARAMS: i64 = -32602;
/// Internal error.
pub(super) const INTERNAL: i64 = -32603;
/// Invalid request (repeated `initialize` after the connection started).
pub(super) const INVALID_REQUEST: i64 = -32600;
/// Resource not found: a session-scoped request named an unknown session.
pub(super) const SESSION_NOT_FOUND: i64 = -32002;
/// Request cancelled (prompt withdrawn before insertion, close settlement).
pub(super) const CANCELLED: i64 = -32800;
/// Peri-defined server error for capacity/backpressure (pinned allows custom
/// server errors in `-32000..=-32099`).
pub(super) const CAPACITY: i64 = -32020;

pub(super) fn unknown_method(method: &str) -> AcpError {
    AcpError::new(
        METHOD_NOT_FOUND,
        format!("Method not supported by Peri ACP v2 Preview: {method}"),
    )
}

pub(super) fn invalid_params(message: impl Into<String>) -> AcpError {
    AcpError::new(INVALID_PARAMS, message)
}

/// `-32002` carries the offending session id so clients can drop local state.
pub(super) fn session_not_found(session_id: &str) -> AcpError {
    AcpError::new(
        SESSION_NOT_FOUND,
        "Session not found or not active on this connection",
    )
    .with_data(json!({ "sessionId": session_id }))
}

pub(super) fn cancelled() -> AcpError {
    AcpError::new(CANCELLED, "Prompt cancelled before insertion")
}

/// Prompt queue/receipt capacity reached.
pub(super) fn prompt_capacity(session_id: &str) -> AcpError {
    capacity(
        "prompt_capacity",
        session_id,
        "ACP prompt queue capacity reached",
    )
}

/// The preview cannot replay a session whose turn is still running (S4 lifts it).
pub(super) fn session_busy(session_id: &str) -> AcpError {
    capacity(
        "session_busy",
        session_id,
        "Replay is unavailable while this session is running a turn",
    )
}

fn capacity(reason: &'static str, session_id: &str, message: &'static str) -> AcpError {
    AcpError::new(CAPACITY, message).with_data(json!({ "reason": reason, "sessionId": session_id }))
}

pub(super) fn already_initialized() -> AcpError {
    AcpError::new(INVALID_REQUEST, "Connection already initialized")
}

pub(super) fn invalid_protocol_version() -> AcpError {
    AcpError::new(INVALID_PARAMS, "Invalid protocolVersion")
}

/// `terminal/cancel` and `tool_call/cancel`: this agent never advertises per-object
/// cancellation, so a cancel for one is a bad request rather than a silent no-op.
pub(super) fn unsupported_object_cancel(method: &str) -> AcpError {
    AcpError::new(
        INVALID_PARAMS,
        format!(
            "{method} is not supported: Peri ACP v2 Preview never advertises per-object cancel"
        ),
    )
}

/// Rewrite a host-passthrough error that a v2 client would misread.
///
/// `-32000` means Authentication required in the pinned schema; Peri's v1 paths
/// use it for backpressure, MCP control and history domain failures. Only that
/// code is remapped (to `-32020` with the original `reason` preserved when there
/// is one), everything else passes through unchanged.
pub(super) fn normalize(error: AcpError) -> AcpError {
    if error.code != -32000 {
        return error;
    }
    let data: Value = match error.data {
        Some(Value::Object(mut object)) => {
            object
                .entry("reason".to_owned())
                .or_insert_with(|| json!("server_error"));
            Value::Object(object)
        }
        _ => json!({ "reason": "server_error" }),
    };
    AcpError::new(CAPACITY, error.message).with_data(data)
}
