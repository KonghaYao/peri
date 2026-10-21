use super::*;
#[test]
fn control_dto_is_strict_and_roundtrips() {
    let request = serde_json::json!({"sessionId":"s1","serverName":"docs","action":"register","config":{"type":"http","url":"https://example.com/mcp"}});
    let parsed: McpControlRequest = serde_json::from_value(request).unwrap();
    parsed.validate().unwrap();
    assert_eq!(parsed.session_id(), "s1");
    assert!(serde_json::from_value::<McpControlRequest>(
        serde_json::json!({"sessionId":"s1","serverName":"x","action":"stop","config":{}})
    )
    .is_err());
    assert!(serde_json::from_value::<McpControlRequest>(serde_json::json!({"sessionId":"s1","serverName":"x","action":"register","config":{"type":"http","url":"https://example.com","systemMcp":true}})).is_err());
    assert!(
        serde_json::from_value::<McpControlRequest>(serde_json::to_value(parsed).unwrap()).is_ok()
    );
}

#[test]
fn historical_mcp_notification_reads_and_serializes_only_current_name() {
    let source: crate::session::MessageSource =
        serde_json::from_str("\"dynamic_mcp_notification\"").unwrap();
    assert_eq!(source, crate::session::MessageSource::McpNotification);
    assert_eq!(
        serde_json::to_value(source).unwrap(),
        serde_json::json!("mcp_notification")
    );
}

#[test]
fn invalid_transport_configuration_is_rejected_and_opaque_stop_names_are_allowed() {
    for config in [
        serde_json::json!({"type":"http","url":"file:///private/config"}),
        serde_json::json!({"type":"http","url":"https://name:secret@example.com/mcp"}),
        serde_json::json!({"type":"stdio","command":" "}),
        serde_json::json!({"type":"stdio","command":"tool","env":{"BAD=NAME":"value"}}),
    ] {
        let request: McpControlRequest = serde_json::from_value(serde_json::json!({
            "sessionId":"s1", "serverName":"docs", "action":"register", "config":config,
        }))
        .unwrap();
        assert!(request.validate().is_err());
    }
    let request: McpControlRequest = serde_json::from_value(serde_json::json!({
        "sessionId":"s1", "serverName":"plugin:pack:docs", "action":"stop",
    }))
    .unwrap();
    request.validate().unwrap();
}
