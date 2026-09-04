use std::path::PathBuf;

use peri_acp_types::thread::ThreadMeta;
use peri_tui::thread::{ReadOnlyThreadStoreError, open_thread_store_read_only};
use serde::Serialize;
use uuid::Uuid;

const SCHEMA_VERSION: u8 = 1;

pub(crate) struct MetaCommandOutcome {
    pub(crate) stdout: Option<String>,
    pub(crate) stderr: Option<String>,
    pub(crate) exit_code: u8,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionMetaDtoV1 {
    schema_version: u8,
    id: String,
    title: Option<String>,
    cwd: String,
    created_at: String,
    updated_at: String,
    message_count: usize,
    parent_thread_id: Option<String>,
    persisted_agent_status: String,
}

impl From<ThreadMeta> for SessionMetaDtoV1 {
    fn from(meta: ThreadMeta) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            id: meta.id,
            title: meta.title,
            cwd: meta.cwd,
            created_at: meta.created_at.to_rfc3339(),
            updated_at: meta.updated_at.to_rfc3339(),
            message_count: meta.message_count,
            parent_thread_id: meta.parent_thread_id,
            persisted_agent_status: meta.agent_status.as_str().to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum MetaErrorKind {
    InvalidArgument,
    InvalidSessionId,
    DatabaseNotFound,
    DatabaseUnreadable,
    SchemaIncompatible,
    SessionNotFound,
    CorruptSessionData,
    InternalError,
}

impl MetaErrorKind {
    fn name(self) -> &'static str {
        match self {
            Self::InvalidArgument => "invalid_argument",
            Self::InvalidSessionId => "invalid_session_id",
            Self::DatabaseNotFound => "database_not_found",
            Self::DatabaseUnreadable => "database_unreadable",
            Self::SchemaIncompatible => "schema_incompatible",
            Self::SessionNotFound => "session_not_found",
            Self::CorruptSessionData => "corrupt_session_data",
            Self::InternalError => "internal_error",
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::InvalidArgument => "invalid Meta command arguments",
            Self::InvalidSessionId => "session ID must be a valid UUID",
            Self::DatabaseNotFound => "thread database was not found",
            Self::DatabaseUnreadable => "thread database could not be opened for reading",
            Self::SchemaIncompatible => "thread database schema is incompatible",
            Self::SessionNotFound => "session was not found",
            Self::CorruptSessionData => "stored session metadata is corrupt",
            Self::InternalError => "an internal error occurred",
        }
    }

    fn exit_code(self) -> u8 {
        match self {
            Self::InternalError => 1,
            Self::InvalidArgument | Self::InvalidSessionId => 2,
            Self::DatabaseNotFound | Self::SessionNotFound => 3,
            Self::DatabaseUnreadable | Self::SchemaIncompatible | Self::CorruptSessionData => 4,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MetaErrorDtoV1 {
    schema_version: u8,
    error: MetaErrorBodyV1,
}

#[derive(Serialize)]
struct MetaErrorBodyV1 {
    kind: &'static str,
    message: &'static str,
}

pub(crate) fn invalid_argument_outcome(json: bool) -> MetaCommandOutcome {
    error_outcome(MetaErrorKind::InvalidArgument, json)
}

pub(crate) fn internal_error_outcome(json: bool) -> MetaCommandOutcome {
    error_outcome(MetaErrorKind::InternalError, json)
}

pub(crate) async fn run_meta_session(
    db_path: Option<PathBuf>,
    session_id: String,
    json: bool,
) -> MetaCommandOutcome {
    // Validate syntax before resolving or opening any database. Keep the original
    // spelling for lookup because ThreadId is persisted as text.
    if Uuid::parse_str(&session_id).is_err() {
        return error_outcome(MetaErrorKind::InvalidSessionId, json);
    }

    let store = match open_thread_store_read_only(db_path).await {
        Ok(store) => store,
        Err(error) => return error_outcome(map_storage_error(&error), json),
    };
    let meta = match store.load_meta(&session_id).await {
        Ok(meta) => meta,
        Err(error) => {
            let kind = error
                .downcast_ref::<ReadOnlyThreadStoreError>()
                .map(map_storage_error)
                .unwrap_or(MetaErrorKind::InternalError);
            return error_outcome(kind, json);
        }
    };

    success_outcome(SessionMetaDtoV1::from(meta), json)
}

fn map_storage_error(error: &ReadOnlyThreadStoreError) -> MetaErrorKind {
    match error {
        ReadOnlyThreadStoreError::DatabaseNotFound => MetaErrorKind::DatabaseNotFound,
        ReadOnlyThreadStoreError::DatabaseUnreadable => MetaErrorKind::DatabaseUnreadable,
        ReadOnlyThreadStoreError::SchemaIncompatible => MetaErrorKind::SchemaIncompatible,
        ReadOnlyThreadStoreError::SessionNotFound => MetaErrorKind::SessionNotFound,
        ReadOnlyThreadStoreError::CorruptSessionData => MetaErrorKind::CorruptSessionData,
        ReadOnlyThreadStoreError::Internal => MetaErrorKind::InternalError,
    }
}

fn success_outcome(dto: SessionMetaDtoV1, json: bool) -> MetaCommandOutcome {
    let output = if json {
        match serde_json::to_string(&dto) {
            Ok(value) => value,
            Err(_) => return error_outcome(MetaErrorKind::InternalError, true),
        }
    } else {
        render_human(&dto)
    };
    MetaCommandOutcome {
        stdout: Some(format!("{output}\n")),
        stderr: None,
        exit_code: 0,
    }
}

fn error_outcome(kind: MetaErrorKind, json: bool) -> MetaCommandOutcome {
    let output = if json {
        let dto = MetaErrorDtoV1 {
            schema_version: SCHEMA_VERSION,
            error: MetaErrorBodyV1 {
                kind: kind.name(),
                message: kind.message(),
            },
        };
        serde_json::to_string(&dto).unwrap_or_else(|_| {
            "{\"schemaVersion\":1,\"error\":{\"kind\":\"internal_error\",\"message\":\"an internal error occurred\"}}".to_owned()
        })
    } else {
        format!("{}: {}", kind.name(), kind.message())
    };
    MetaCommandOutcome {
        stdout: None,
        stderr: Some(format!("{output}\n")),
        exit_code: kind.exit_code(),
    }
}

fn render_human(dto: &SessionMetaDtoV1) -> String {
    format!(
        "Schema version: {}\nID: {}\nTitle: {}\nCWD: {}\nCreated at: {}\nUpdated at: {}\nMessage count: {}\nParent thread ID: {}\nPersisted agent status: {}",
        dto.schema_version,
        escape_human(&dto.id),
        render_nullable(dto.title.as_deref()),
        escape_human(&dto.cwd),
        escape_human(&dto.created_at),
        escape_human(&dto.updated_at),
        dto.message_count,
        render_nullable(dto.parent_thread_id.as_deref()),
        escape_human(&dto.persisted_agent_status),
    )
}

fn render_nullable(value: Option<&str>) -> String {
    value.map(escape_human).unwrap_or_else(|| "null".to_owned())
}

fn escape_human(value: &str) -> String {
    value.chars().flat_map(char::escape_debug).collect()
}

#[cfg(test)]
#[path = "cli_meta_test.rs"]
mod tests;
