//! Central construction and mapping for ACP v2 error codes.
//!
//! The v2 adapter and the preview-only host handlers build their errors from the
//! constructors and constants here, so no bare code literal lives in a request
//! path. The pinned v2 schema reserves `-32000` for **Authentication required**,
//! so the preview never emits it: capacity, backpressure and a busy session use
//! Peri's own server error `-32020` with a stable `data.reason`
//! (`prompt_capacity`, `session_busy`), unknown sessions use `-32002` with
//! `data.sessionId` (`SessionNotFoundErrorData`), and `state_update.error`
//! reports execution failure as `-32603`.
//!
//! `-32010` is the host's overloaded conflict code. The only v2 outbound path
//! allowed to keep it is the close-incomplete settlement of `session/close`
//! (and the shared close/delete gate behind it); every other host `-32010` is
//! normalized by [`host_failure`]. A setup refused because the session's
//! previous close has not finished is `-32603` with
//! `data.reason = "session_cleanup_incomplete"`: the request was valid and only
//! the explicit close can drain the session.

use serde_json::{json, Value};

use crate::transport::types::AcpError;

/// Method not found or not advertised on this connection.
pub(crate) const METHOD_NOT_FOUND: i64 = -32601;
/// Invalid params: bad content block, missing/relative cwd, bad cursor, bad key.
pub(crate) const INVALID_PARAMS: i64 = -32602;
/// Internal error.
pub(crate) const INTERNAL: i64 = -32603;
/// Invalid request (repeated `initialize` after the connection started).
pub(super) const INVALID_REQUEST: i64 = -32600;
/// Resource not found: a session-scoped request named an unknown session.
pub(crate) const SESSION_NOT_FOUND: i64 = -32002;
/// Request cancelled (prompt withdrawn before insertion, close settlement).
pub(super) const CANCELLED: i64 = -32800;
/// Peri-defined server error for capacity/backpressure and for a busy session
/// (pinned allows custom server errors in `-32000..=-32099`). `data.reason`
/// distinguishes the two families: `prompt_capacity` vs `session_busy`.
pub(super) const CAPACITY: i64 = -32020;
/// The host's `-32010` conflict code: the v1 session-switch/setup gates and the
/// close/cleanup passthrough. A v2 setup never forwards the host's busy
/// rejection under this code; it answers `session_busy` (`-32020`) instead.
pub(super) const SESSION_SWITCH: i64 = -32010;

/// The host's transient per-session refusals: the prompt lock is held by the
/// running turn, or the session is being closed. The message, not the code,
/// decides whether a host `-32010` is such a retryable conflict.
const HOST_TRANSIENT_REFUSALS: [&str; 2] = ["Session is executing", "Session is closing"];
/// The host's close/delete gate timeout. This is the only `-32010` the v2
/// outbound surface forwards.
const HOST_CLOSE_INCOMPLETE: &str = "Session close incomplete";

pub(super) fn unknown_method(method: &str) -> AcpError {
    AcpError::new(
        METHOD_NOT_FOUND,
        format!("Method not supported by Peri ACP v2 Preview: {method}"),
    )
}

pub(crate) fn invalid_params(message: impl Into<String>) -> AcpError {
    AcpError::new(INVALID_PARAMS, message)
}

/// A host-side failure the client cannot fix by changing its request.
pub(crate) fn internal(message: impl Into<String>) -> AcpError {
    AcpError::new(INTERNAL, message)
}

/// A preview-only handler answered for a session that is no longer active on the
/// host. The request named this session, so the reply is the pinned
/// [`session_not_found`] shape: `-32002` carries the id and clients can drop the
/// local state they still hold for it.
pub(crate) fn session_unavailable(session_id: &str, message: impl Into<String>) -> AcpError {
    session_not_found_with(session_id, message)
}

/// `-32002` carries the offending session id so clients can drop local state.
pub(super) fn session_not_found(session_id: &str) -> AcpError {
    session_not_found_with(
        session_id,
        "Session not found or not active on this connection",
    )
}

fn session_not_found_with(session_id: &str, message: impl Into<String>) -> AcpError {
    AcpError::new(SESSION_NOT_FOUND, message).with_data(json!({ "sessionId": session_id }))
}

pub(super) fn cancelled() -> AcpError {
    AcpError::new(CANCELLED, "Prompt cancelled before insertion")
}

/// Prompt queue/receipt capacity reached.
pub(super) fn prompt_capacity(session_id: &str) -> AcpError {
    server_error(
        "prompt_capacity",
        session_id,
        "ACP prompt queue capacity reached",
    )
}

/// The session's turn still executes, so the host refuses the setup.
///
/// Reported on the same `-32020` server error as capacity so a v2 client sees
/// one Peri code with an explicit reason, and can retry after the session
/// publishes `idle`.
pub(super) fn session_busy(session_id: &str) -> AcpError {
    server_error(
        "session_busy",
        session_id,
        "Session is busy; retry after the current turn",
    )
}

fn server_error(reason: &'static str, session_id: &str, message: &'static str) -> AcpError {
    AcpError::new(CAPACITY, message).with_data(json!({ "reason": reason, "sessionId": session_id }))
}

pub(super) fn already_initialized() -> AcpError {
    AcpError::new(INVALID_REQUEST, "Connection already initialized")
}

pub(super) fn invalid_protocol_version() -> AcpError {
    AcpError::new(INVALID_PARAMS, "Invalid protocolVersion")
}

/// The v1 connection still holds another session.
///
/// The preview wrapper reports multi-session support before the version is
/// negotiated, so the shared host no longer applies this gate; the adapter
/// enforces it for `Version::One` with the host's code, reason and message
/// (`host/connection_context.rs`).
pub(super) fn session_switch_required() -> AcpError {
    session_switch(
        crate::host::connection_context::SESSION_SWITCH_REQUIRED_REASON,
        "Close the current session with session/close before switching sessions",
    )
}

/// Another v1 setup is still in flight on this connection.
pub(super) fn session_setup_in_progress() -> AcpError {
    session_switch(
        crate::host::connection_context::SESSION_SETUP_IN_PROGRESS_REASON,
        "Another session setup is in progress",
    )
}

fn session_switch(reason: &'static str, message: &'static str) -> AcpError {
    AcpError::new(SESSION_SWITCH, message).with_data(json!({ "reason": reason }))
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

/// Where a host-passthrough error is about to be written to a v2 client.
///
/// The same host code means different things per request surface; naming the
/// context here keeps every mapping in this module instead of at the call sites.
#[derive(Debug, Clone, Copy)]
pub(super) enum HostContext<'a> {
    /// A `session/new` / `session/resume` reply; the id is `None` for `new`.
    Setup { session_id: Option<&'a str> },
    /// The preview-only `session/list` storage query.
    SessionList,
    /// A prompt's admission/workspace path.
    Prompt,
    /// `session/close` / `session/delete`; only the close-incomplete settlement
    /// keeps the host code.
    Close,
}

/// Whether a host `-32010` reports a session whose close has not finished.
///
/// The host's cold restore answers `data.reason = "session_cleanup_incomplete"`
/// together with the session id it keeps reserved; the restore dispatch's
/// "resume the close before loading this session" refusal only carries the
/// message prefix. Both mean the same thing to a client and neither is a
/// parameter it can correct.
pub(super) fn cleanup_incomplete(error: &AcpError) -> bool {
    error.code == SESSION_SWITCH
        && (error.message.starts_with(HOST_CLOSE_INCOMPLETE)
            || crate::host::server_loop::cleanup_incomplete_error_session_id(error).is_some())
}

/// The session id a cleanup-incomplete failure names, if the host supplied one.
pub(super) fn cleanup_incomplete_session_id(error: &AcpError) -> Option<&str> {
    crate::host::server_loop::cleanup_incomplete_error_session_id(error)
}

/// A cleanup-incomplete setup failure: internal, with the host's `data` kept and
/// the stable `reason`/`sessionId` filled in.
fn cleanup_incomplete_internal(error: AcpError, fallback_session: Option<&str>) -> AcpError {
    let mut data = match error.data {
        Some(Value::Object(object)) => object,
        _ => serde_json::Map::new(),
    };
    data.entry("reason".to_owned())
        .or_insert_with(|| json!("session_cleanup_incomplete"));
    if let Some(session_id) = fallback_session {
        data.entry("sessionId".to_owned())
            .or_insert_with(|| json!(session_id));
    }
    AcpError::new(INTERNAL, error.message).with_data(Value::Object(data))
}

/// Map a host-passthrough error that a v2 client would misread.
///
/// `-32000` means Authentication required in the pinned schema; Peri's v1 paths
/// use it for backpressure, MCP control and history domain failures. That code
/// is remapped to `-32020` with the original `reason` preserved when there is
/// one.
///
/// The host's `-32010` is only forwarded by the close-incomplete settlement,
/// which does not call this function. Every other context is translated:
///
/// - the host's transient per-session refusals ("Session is executing",
///   "Session is closing") become `session_busy` (`-32020` + `reason`) when the
///   caller can name the session;
/// - a setup refused because the session's close has not finished is internal
///   (`-32603`) with `reason`/`sessionId` kept: the client's request was valid
///   and the only way forward is the explicit close that drains the session;
/// - a setup whose client-supplied location cannot be used is Invalid params
///   (`-32602`) with the host's message intact;
/// - host resource/internal failures (list storage, prompt workspace checks,
///   a close that failed for another reason) are `-32603`, again keeping the
///   message;
/// - a close/delete gate timeout keeps `-32010`: it is the one retryable
///   close-incomplete settlement the v2 surface exposes.
pub(super) fn host_failure(error: AcpError, context: HostContext<'_>) -> AcpError {
    if error.code == SESSION_SWITCH {
        if HOST_TRANSIENT_REFUSALS
            .iter()
            .any(|prefix| error.message.starts_with(prefix))
        {
            return match context {
                HostContext::Setup {
                    session_id: Some(session_id),
                } => session_busy(session_id),
                _ => internal(error.message),
            };
        }
        return match context {
            HostContext::Setup { session_id } if cleanup_incomplete(&error) => {
                cleanup_incomplete_internal(error, session_id)
            }
            HostContext::Setup { .. } => invalid_params(error.message),
            HostContext::Close if error.message.starts_with(HOST_CLOSE_INCOMPLETE) => error,
            HostContext::Close | HostContext::SessionList | HostContext::Prompt => {
                internal(error.message)
            }
        };
    }
    normalize(error)
}

/// Rewrite the overloaded host `-32000`; other codes pass through unchanged.
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

/// Map a host error from a v2 `session/new` / `session/resume` setup.
///
/// A setup whose session's turn still holds the host's per-session prompt lock
/// gets the stable busy code instead of the host's overloaded `-32010`; any
/// other host `-32010` (unavailable cwd, relink/binding failures) is the
/// client's Invalid params. See [`host_failure`].
pub(super) fn setup_failure(error: AcpError, session_id: Option<&str>) -> AcpError {
    host_failure(error, HostContext::Setup { session_id })
}
