//! Optional task owner metadata shared by hosts and capability packages.
//!
//! The owner is the direct initiating session id. It is supplied by the host on
//! each request; the model cannot set it. A missing owner means generic MCP
//! mode, where basic tools work without any Peri-specific metadata.

use rmcp::{model::RequestMetaObject, ErrorData as McpError};

pub const TASK_OWNER_META_KEY: &str = "peri/taskOwner";

/// Read the optional task owner from request metadata.
///
/// - absent or `null` -> `Ok(None)` (generic mode)
/// - non-empty string -> `Ok(Some(owner))`
/// - anything else -> `-32602`
pub fn task_owner_from_meta(meta: &RequestMetaObject) -> Result<Option<String>, McpError> {
    match meta.0 .0.get(TASK_OWNER_META_KEY) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(owner)) if !owner.is_empty() => Ok(Some(owner.clone())),
        Some(_) => Err(McpError::invalid_params(
            "task owner must be a non-empty string",
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
}
