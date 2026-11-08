//! Host-supplied canonical invocation identity in MCP request `_meta`.
//!
//! The host writes this before a model-initiated tool call; capability servers
//! may use it to correlate side state with the transcript entry that caused it.
//! A missing object means the call did not come from a canonical dispatch.

use rmcp::model::RequestMetaObject;

pub const INVOCATION_META_KEY: &str = "peri.invocation";

/// Model tool-call id for the current invocation, when the host supplied one.
pub fn tool_call_id_from_meta(meta: &RequestMetaObject) -> Option<String> {
    meta.0
         .0
        .get(INVOCATION_META_KEY)?
        .get("toolCallId")?
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(value: serde_json::Value) -> RequestMetaObject {
        let mut meta = RequestMetaObject::new();
        meta.0 .0.insert(INVOCATION_META_KEY.into(), value);
        meta
    }

    #[test]
    fn missing_invocation_is_none() {
        assert_eq!(tool_call_id_from_meta(&RequestMetaObject::new()), None);
    }

    #[test]
    fn tool_call_id_is_read_from_invocation_object() {
        assert_eq!(
            tool_call_id_from_meta(&meta(serde_json::json!({"toolCallId": "call-1"}))),
            Some("call-1".to_owned())
        );
    }

    #[test]
    fn malformed_or_empty_tool_call_id_is_none() {
        for value in [
            serde_json::json!({}),
            serde_json::json!({"toolCallId": ""}),
            serde_json::json!({"toolCallId": 42}),
            serde_json::json!("call-1"),
        ] {
            assert_eq!(tool_call_id_from_meta(&meta(value)), None);
        }
    }
}
