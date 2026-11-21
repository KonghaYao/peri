use axum::extract::rejection::{BytesRejection, JsonRejection, QueryRejection};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use serde_json::Value;

use crate::domain::{ErrorCode, ErrorKind, SwarmError};

#[derive(Serialize)]
struct ErrorBody<'a> {
    code: &'a str,
    message: &'a str,
    details: &'a Value,
}

#[derive(Serialize)]
struct ErrorEnvelope<'a> {
    error: ErrorBody<'a>,
}

#[must_use]
pub fn status_for(kind: ErrorKind) -> StatusCode {
    match kind {
        ErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        ErrorKind::Unauthorized => StatusCode::UNAUTHORIZED,
        ErrorKind::NotFound => StatusCode::NOT_FOUND,
        ErrorKind::Conflict => StatusCode::CONFLICT,
        ErrorKind::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        ErrorKind::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ErrorKind::Unprocessable => StatusCode::UNPROCESSABLE_ENTITY,
        ErrorKind::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        ErrorKind::BadGateway => StatusCode::BAD_GATEWAY,
    }
}

impl IntoResponse for SwarmError {
    fn into_response(self) -> Response {
        let body = ErrorEnvelope {
            error: ErrorBody {
                code: self.code().as_str(),
                message: self.message(),
                details: self.details(),
            },
        };
        (status_for(self.kind()), Json(body)).into_response()
    }
}

/// Preserve axum's rejection status while keeping the documented error body.
#[must_use]
pub fn from_json_rejection(rejection: JsonRejection) -> SwarmError {
    let status = rejection.status();
    let message = rejection.body_text();
    if status == StatusCode::PAYLOAD_TOO_LARGE {
        return SwarmError::new(
            ErrorKind::PayloadTooLarge,
            ErrorCode::RequestTooLarge,
            "request body exceeds 8 MiB",
        );
    }
    if status == StatusCode::UNSUPPORTED_MEDIA_TYPE {
        return SwarmError::new(
            ErrorKind::UnsupportedMediaType,
            ErrorCode::InvalidContentType,
            "content-type must be application/json",
        );
    }
    if message.contains("unknown field") {
        return SwarmError::new(ErrorKind::BadRequest, ErrorCode::UnknownField, message);
    }
    if message.contains("unknown variant") {
        return SwarmError::new(ErrorKind::BadRequest, ErrorCode::InvalidInput, message);
    }
    SwarmError::new(ErrorKind::BadRequest, ErrorCode::InvalidRequest, message)
}

/// Buffer rejection for handlers that read the raw body to decide whether one
/// was sent at all (`sessions.update`).
#[must_use]
pub fn from_bytes_rejection(rejection: BytesRejection) -> SwarmError {
    if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return SwarmError::new(
            ErrorKind::PayloadTooLarge,
            ErrorCode::RequestTooLarge,
            "request body exceeds 8 MiB",
        );
    }
    SwarmError::new(
        ErrorKind::BadRequest,
        ErrorCode::InvalidRequest,
        rejection.to_string(),
    )
}

#[must_use]
pub fn from_query_rejection(rejection: QueryRejection) -> SwarmError {
    SwarmError::new(
        ErrorKind::BadRequest,
        ErrorCode::InvalidQuery,
        rejection.body_text(),
    )
}
