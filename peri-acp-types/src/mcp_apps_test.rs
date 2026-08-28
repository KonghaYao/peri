use super::*;
use serde_json::json;

#[test]
fn inbound_app_envelope_rejects_mcp_protocol_version() {
    let value = json!({
        "envelopeVersion": "1",
        "appsProtocolVersion": MCP_APPS_PROTOCOL_VERSION,
        "mcpProtocolVersion": "2025-11-25",
        "serverId": "server",
        "appSessionId": "app",
        "resourceUri": "ui://app",
        "payload": {"jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{}}
    });
    assert!(serde_json::from_value::<McpAppRequest>(value).is_err());
}

#[test]
fn raw_result_roundtrip_preserves_unknown_fields() {
    let value = json!({
        "content": [{"type":"resource", "resource":{"uri":"ui://app", "text":"<html/>"}}],
        "structuredContent": {"answer": 42},
        "_meta": {"vendor": {"opaque": true}},
        "isError": true,
        "futureField": {"preserved": true}
    });
    let decoded: RawCallToolResult = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), value);
}

#[test]
fn json_rpc_response_requires_exactly_one_terminal_shape() {
    assert!(serde_json::from_value::<JsonRpcResponse>(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {},
        "error": {"code": -1, "message": "bad"}
    }))
    .is_err());
    assert!(serde_json::from_value::<JsonRpcResponse>(json!({
        "jsonrpc": "2.0",
        "id": 1
    }))
    .is_err());
}

#[test]
fn call_tool_params_require_name_and_object_arguments() {
    assert!(serde_json::from_value::<JsonRpcRequest>(json!({
        "jsonrpc": "2.0",
        "id": "request",
        "method": "tools/call",
        "params": {"arguments": {}}
    }))
    .is_err());
    assert!(serde_json::from_value::<JsonRpcRequest>(json!({
        "jsonrpc": "2.0",
        "id": "request",
        "method": "tools/call",
        "params": {"name": "tool", "arguments": []}
    }))
    .is_err());
}

#[test]
fn app_resource_requires_exact_uri_mime_and_one_body() {
    let resource = RawResource {
        uri: "ui://app".into(),
        mime_type: MCP_APPS_HTML_MIME.into(),
        text: Some("<html/>".into()),
        blob: None,
        meta: BTreeMap::from([("ui".into(), json!({"csp": {"connectDomains": []}}))]),
        extra: BTreeMap::new(),
    };
    assert!(resource.is_valid_app_resource("ui://app"));
    assert!(!resource.is_valid_app_resource("ui://other"));
}
