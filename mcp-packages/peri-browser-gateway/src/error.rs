//! Typed gateway errors and their HTTP mapping.
//!
//! The mapping is deliberately coarse: unknown tokens and unauthorized tokens
//! both answer `404` (README §6.6), so the monitor port never reveals whether a
//! session exists. Internal details are logged, never returned.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    /// Unknown or unauthorized session token.
    #[error("unknown session token")]
    UnknownToken,
    /// Malformed request (bad path, missing upgrade headers).
    #[error("bad request: {0}")]
    BadRequest(String),
    /// Gateway-side failure; the detail stays in the log.
    #[error("internal gateway error: {0}")]
    Internal(String),
}

impl IntoResponse for GatewayError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            Self::UnknownToken => (StatusCode::NOT_FOUND, "not found"),
            Self::BadRequest(detail) => {
                tracing::debug!(detail, "rejecting request");
                (StatusCode::BAD_REQUEST, "bad request")
            }
            Self::Internal(detail) => {
                tracing::error!(detail, "internal gateway error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error")
            }
        };
        (
            status,
            [("content-type", "text/plain; charset=utf-8")],
            body,
        )
            .into_response()
    }
}

#[cfg(test)]
#[path = "error_test.rs"]
mod tests;
