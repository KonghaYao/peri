//! `session/list` pagination for the ACP v2 stdio preview.
//!
//! The pinned cursor is opaque to the client; this module owns its only
//! encoding: `base64url(json({"u": updatedAt, "i": threadId}))`, exactly the
//! store's keyset position. Storage supports `(updated_at, thread_id)` keyset
//! paging but not a `cwd` predicate inside it, so a `cwd` filter is applied
//! after each raw page and pages are fetched until the requested number of
//! matching sessions is reached or the store is exhausted. `nextCursor` is the
//! cursor of the last raw page consumed, never of the last match, so an entry
//! filtered out of one response is never skipped.
//!
//! The query is a preview-only internal request (`_peri/stdio-v2/session-list`):
//! the v1 `session/list` handler keeps its existing shape and pagination
//! extension untouched.

use std::future::Future;
use std::path::{Path, PathBuf};

use agent_client_protocol_schema::v1::{ListSessionsResponse, SessionId, SessionInfo};
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use peri_acp_types::session_resources::SessionResourceResult;
use peri_acp_types::workspace::{
    ScopedThreadEntry, ScopedThreadPage, ScopedThreadQuery, ThreadListCursor, ThreadScope,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::host::AcpServerConfig;
use crate::transport::types::{AcpError, RequestId};

use super::state::{Connection, Reply, Routed};
use super::{errors, SESSION_LIST};

/// Page size used when the client sends no preview `limit`.
pub(super) const DEFAULT_LIMIT: u32 = 100;
/// Largest page size the preview serves.
pub(super) const MAX_LIMIT: u32 = 500;

#[derive(Serialize, Deserialize)]
struct CursorWire {
    u: String,
    i: String,
}

/// Encode one store position as the opaque v2 cursor.
pub(super) fn encode_cursor(cursor: &ThreadListCursor) -> String {
    let wire = CursorWire {
        u: cursor.updated_at.to_rfc3339(),
        i: cursor.thread_id.clone(),
    };
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(&wire).expect("a cursor is serializable"))
}

/// Decode a client-supplied cursor. Anything that is not exactly one of our
/// encoded positions is Invalid params, never a substituted "first page".
pub(super) fn decode_cursor(text: &str) -> Result<ThreadListCursor, AcpError> {
    let invalid = || errors::invalid_params("Invalid session list cursor");
    let bytes = URL_SAFE_NO_PAD
        .decode(text)
        .or_else(|_| URL_SAFE.decode(text))
        .map_err(|_| invalid())?;
    let wire: CursorWire = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    let updated_at = DateTime::parse_from_rfc3339(&wire.u)
        .map_err(|_| invalid())?
        .with_timezone(&Utc);
    if wire.i.is_empty() {
        return Err(invalid());
    }
    Ok(ThreadListCursor {
        updated_at,
        thread_id: wire.i,
    })
}

/// Admit one `session/list` request and forward the preview-only internal query.
pub(super) fn request(connection: &mut Connection, id: RequestId, params: &Value) -> Routed {
    let cwd = match params.get("cwd").filter(|value| !value.is_null()) {
        None => None,
        Some(value) => match value.as_str() {
            Some(cwd) if Path::new(cwd).is_absolute() => Some(cwd.to_owned()),
            _ => {
                return Routed::Answered(Err(errors::invalid_params(
                    "ListSessionsRequest.cwd must be an absolute path",
                )))
            }
        },
    };
    let cursor = match params.get("cursor").filter(|value| !value.is_null()) {
        None => None,
        Some(Value::String(text)) => match decode_cursor(text) {
            Ok(cursor) => Some(cursor),
            Err(error) => return Routed::Answered(Err(error)),
        },
        Some(_) => {
            return Routed::Answered(Err(errors::invalid_params("Invalid session list cursor")))
        }
    };
    // The pinned request has no page-size field; the preview accepts `limit`
    // as a documented extension so probes can exercise every page boundary.
    let limit = match params.get("limit").filter(|value| !value.is_null()) {
        None => DEFAULT_LIMIT,
        Some(value) => match value.as_u64() {
            Some(limit) => limit.clamp(1, u64::from(MAX_LIMIT)) as u32,
            None => {
                return Routed::Answered(Err(errors::invalid_params(
                    "session list limit must be a positive integer",
                )))
            }
        },
    };
    let internal_id = RequestId::String(uuid::Uuid::now_v7().to_string());
    connection
        .replies
        .insert(internal_id.clone(), Reply::SessionList { client: id });
    let mut internal = json!({ "limit": limit });
    if let Some(cwd) = cwd {
        internal["cwd"] = json!(cwd);
    }
    if let Some(cursor) = cursor {
        internal["cursor"] = json!({
            "updatedAt": cursor.updated_at.to_rfc3339(),
            "threadId": cursor.thread_id,
        });
    }
    Routed::Forward {
        id: internal_id,
        method: SESSION_LIST.into(),
        params: internal,
    }
}

/// One filtered page, already in the v2 wire shape.
#[derive(Debug)]
pub(super) struct Page {
    pub(super) sessions: Vec<SessionInfo>,
    pub(super) next_cursor: Option<String>,
}

/// The host side of `_peri/stdio-v2/session-list`.
pub(in crate::host) async fn handle(
    cfg: &AcpServerConfig,
    params: &Value,
) -> Result<Value, AcpError> {
    let cwd = params.get("cwd").and_then(Value::as_str);
    let limit = params
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(u64::from(DEFAULT_LIMIT))
        .clamp(1, u64::from(MAX_LIMIT)) as u32;
    let cursor = internal_cursor(params.get("cursor"))?;
    let resources = std::sync::Arc::clone(&cfg.session_resources);
    let page = page_through(cwd, cursor, limit, move |query| {
        let resources = std::sync::Arc::clone(&resources);
        async move { resources.list_sessions(&query).await }
    })
    .await?;
    let response = ListSessionsResponse::new(page.sessions).next_cursor(page.next_cursor);
    serde_json::to_value(response)
        .map_err(|error| AcpError::new(errors::INTERNAL, error.to_string()))
}

fn internal_cursor(value: Option<&Value>) -> Result<Option<ThreadListCursor>, AcpError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let invalid = || errors::invalid_params("Invalid session list cursor");
    let updated_at = value
        .get("updatedAt")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let thread_id = value
        .get("threadId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(invalid)?;
    let updated_at = DateTime::parse_from_rfc3339(updated_at)
        .map_err(|_| invalid())?
        .with_timezone(&Utc);
    Ok(Some(ThreadListCursor {
        updated_at,
        thread_id: thread_id.to_owned(),
    }))
}

/// Page storage until `limit` matching sessions are collected or it runs out.
async fn page_through<F, Fut>(
    cwd: Option<&str>,
    mut cursor: Option<ThreadListCursor>,
    limit: u32,
    mut fetch: F,
) -> Result<Page, AcpError>
where
    F: FnMut(ScopedThreadQuery) -> Fut,
    Fut: Future<Output = SessionResourceResult<ScopedThreadPage>>,
{
    let mut sessions = Vec::new();
    loop {
        let page = fetch(ScopedThreadQuery {
            scope: ThreadScope::All,
            cursor: cursor.clone(),
            limit,
        })
        .await
        .map_err(crate::host::workspace::resource_error)?;
        let next = page.next_cursor.clone();
        sessions.extend(
            page.entries
                .iter()
                .filter(|entry| cwd.is_none_or(|dir| entry.thread.cwd == dir))
                .map(session_info),
        );
        match next {
            Some(next) if sessions.len() < limit as usize => cursor = Some(next),
            Some(next) => {
                return Ok(Page {
                    sessions,
                    next_cursor: Some(encode_cursor(&next)),
                })
            }
            None => {
                return Ok(Page {
                    sessions,
                    next_cursor: None,
                })
            }
        }
    }
}

fn session_info(entry: &ScopedThreadEntry) -> SessionInfo {
    SessionInfo::new(
        SessionId::new(entry.thread.id.as_str()),
        PathBuf::from(&entry.thread.cwd),
    )
    .title(entry.thread.title.clone())
    .updated_at(entry.thread.updated_at.to_rfc3339())
}

#[cfg(test)]
#[path = "list_test.rs"]
mod tests;
