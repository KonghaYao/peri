//! Optional task owner metadata shared by hosts and capability packages.
//!
//! The owner is the direct initiating session id. It is supplied by the host on
//! each request; the model cannot set it. A missing owner means generic MCP
//! mode, where basic tools work without any Peri-specific metadata.

use rmcp::{model::RequestMetaObject, ErrorData as McpError};

pub const TASK_OWNER_META_KEY: &str = "peri/taskOwner";

/// Agent thread that directly issued the work; recorded beside the session owner.
///
/// Recovery queries live peripheral tasks by the thread that owns them, so the
/// two identities stay separate even when a deployment currently uses the same
/// value for both.
pub const THREAD_OWNER_META_KEY: &str = "peri/threadOwner";

/// Read the optional task owner from request metadata.
///
/// - absent or `null` -> `Ok(None)` (generic mode)
/// - non-empty string -> `Ok(Some(owner))`
/// - anything else -> `-32602`
pub fn task_owner_from_meta(meta: &RequestMetaObject) -> Result<Option<String>, McpError> {
    owner_from_meta(meta, TASK_OWNER_META_KEY, "task owner")
}

/// Read the optional thread owner from request metadata (same shapes as
/// [`task_owner_from_meta`]).
pub fn thread_owner_from_meta(meta: &RequestMetaObject) -> Result<Option<String>, McpError> {
    owner_from_meta(meta, THREAD_OWNER_META_KEY, "thread owner")
}

fn owner_from_meta(
    meta: &RequestMetaObject,
    key: &str,
    label: &str,
) -> Result<Option<String>, McpError> {
    match meta.0 .0.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(owner)) if !owner.is_empty() => Ok(Some(owner.clone())),
        Some(_) => Err(McpError::invalid_params(
            format!("{label} must be a non-empty string"),
            None,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(value: serde_json::Value) -> RequestMetaObject {
        let mut meta = RequestMetaObject::new();
        meta.0 .0.insert(TASK_OWNER_META_KEY.into(), value);
        meta
    }

    #[test]
    fn missing_owner_is_generic_mode() {
        assert_eq!(
            task_owner_from_meta(&RequestMetaObject::new()).unwrap(),
            None
        );
    }

    #[test]
    fn string_owner_is_returned() {
        assert_eq!(
            task_owner_from_meta(&meta("session-a".into())).unwrap(),
            Some("session-a".to_owned())
        );
    }

    #[test]
    fn invalid_owner_is_rejected() {
        assert!(task_owner_from_meta(&meta(serde_json::json!(42))).is_err());
        assert!(task_owner_from_meta(&meta("".into())).is_err());
    }

    #[test]
    fn thread_owner_is_read_independently_from_task_owner() {
        let mut fields = serde_json::Map::new();
        fields.insert(TASK_OWNER_META_KEY.into(), "session-a".into());
        fields.insert(THREAD_OWNER_META_KEY.into(), "thread-a".into());
        let meta = RequestMetaObject(rmcp::model::MetaObject(fields));
        assert_eq!(
            task_owner_from_meta(&meta).unwrap(),
            Some("session-a".to_owned())
        );
        assert_eq!(
            thread_owner_from_meta(&meta).unwrap(),
            Some("thread-a".to_owned())
        );
        assert_eq!(
            thread_owner_from_meta(&RequestMetaObject::new()).unwrap(),
            None
        );
    }
}
