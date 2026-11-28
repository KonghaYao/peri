use super::*;

#[test]
fn replay_resets_existing_message_before_appending_and_preserves_canonical_id() {
    let projected = project_update(
        json!({
            "sessionUpdate": "agent_message_chunk", "messageId": "canonical",
            "content": {"type": "text", "text": "hello"}, "_meta": {"private": true}
        }),
        true,
    )
    .unwrap();
    assert_eq!(
        projected[0],
        json!({"sessionUpdate":"agent_message","messageId":"canonical","content":[]})
    );
    assert_eq!(projected[1]["messageId"], projected[0]["messageId"]);
    assert!(projected[1].get("_meta").is_none());
}

#[test]
fn v2_negotiation_never_inherits_v1_client_tool_capabilities() {
    let params = initialize_params(&json!({
        "info":{"name":"client","version":"1"}, "clientCapabilities": {"terminal":true}
    }))
    .unwrap();
    assert!(params["clientCapabilities"].get("terminal").is_none());
    assert!(initialize_params(&json!({"info":{"name":"client"}})).is_err());
    assert_eq!(initialize_response()["protocolVersion"], 2);
}
