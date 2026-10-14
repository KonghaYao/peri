use super::*;
use serde_json::json;

#[test]
fn inbound_invoke_envelope_rejects_mcp_protocol_version() {
    let value = json!({
        "envelopeVersion": "1",
        "appsProtocolVersion": MCP_APPS_PROTOCOL_VERSION,
        "mcpProtocolVersion": "2025-11-25",
        "serverId": "cursor-canvas",
        "toolName": "show_canvas",
        "ownerSessionId": "session",
        "arguments": {"source": "export default function App(){return null}"}
    });
    assert!(serde_json::from_value::<McpAppInvokeRequest>(value).is_err());
}

#[test]
fn invoke_request_roundtrip_keeps_inner_mcp_arguments() {
    let value = json!({
        "envelopeVersion": "1",
        "appsProtocolVersion": MCP_APPS_PROTOCOL_VERSION,
        "serverId": "cursor-canvas",
        "toolName": "show_canvas",
        "ownerSessionId": "acp-session-1",
        "arguments": {"source": "export default function App(){return null}", "title": "Board"}
    });
    let decoded: McpAppInvokeRequest = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(decoded.server_id, "cursor-canvas");
    assert_eq!(decoded.tool_name, "show_canvas");
    assert_eq!(decoded.arguments["title"], "Board");
    let encoded = serde_json::to_value(&decoded).unwrap();
    assert_eq!(encoded["toolName"], "show_canvas");
    assert_eq!(encoded["ownerSessionId"], "acp-session-1");
}

#[test]
fn invoke_response_exposes_camel_case_tool_call_id() {
    let response = McpAppInvokeResponse {
        envelope_version: MCP_APPS_ENVELOPE_VERSION.into(),
        apps_protocol_version: MCP_APPS_PROTOCOL_VERSION.into(),
        mcp_protocol_version: "2025-03-26".into(),
        server_id: "cursor-canvas".into(),
        tool_call_id: "tool-new-1".into(),
    };
    let value = serde_json::to_value(response).unwrap();
    assert_eq!(value["toolCallId"], "tool-new-1");
    assert!(value.get("tool_call_id").is_none());
}

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

// ─── SessionInvocationPort 契约（session-owned canonical 入口） ────────────────

#[test]
fn invocation_targets_keep_app_and_host_identity_distinct() {
    let app = InvocationTarget::App {
        record_id: "app-session-1".into(),
        tool_name: "show_canvas".into(),
        arguments: serde_json::Map::new(),
    };
    let host = InvocationTarget::Host {
        server_id: "cursor-canvas".into(),
        tool_name: "show_canvas".into(),
        arguments: serde_json::Map::new(),
    };
    assert_ne!(app, host, "App record 身份与 Host server 身份不得混用");
}

#[test]
fn not_executed_distinguishes_missing_approval_from_user_rejection() {
    // 设计 §3.4：没有人在线**不是**用户拒绝——两者必须是不同的 typed 事实，
    // 否则消费侧会把「无人可答」当成「用户不同意」。
    let unavailable = NotExecutedCause::ApprovalUnavailable(UnansweredCause::NoOnlineEndpoint);
    let rejected = NotExecutedCause::UserRejected;
    assert_ne!(unavailable, rejected);
    assert_ne!(
        unavailable,
        NotExecutedCause::ApprovalUnavailable(UnansweredCause::NonInteractiveClient),
        "原因本身也必须保留（不得折叠成单一 Unavailable）"
    );
}

#[test]
fn outcome_record_carries_explicitly_skipped_post_hooks() {
    // 裁决 R2：无 turn 时被跳过的事后钩子必须写进 outcome 记录（结构化），
    // 而不是只在日志里出现一次。
    let record = SessionInvocationOutcomeRecord {
        invocation_id: crate::tool_invocation::InvocationId::new("inv-1"),
        // rerun 目标权威：记录必须随结果保留服务端目标身份（裁决 D4）。
        target: Some(crate::mcp_apps_facade::TargetIdentity {
            server_id: "server-1".to_string(),
            wire_tool_name: "show_canvas".to_string(),
            effective_tool_name: "mcp__server-1__show_canvas".to_string(),
        }),
        outcome: SessionInvocationOutcome::Completed(crate::tool_outcome::RawToolResult::new(
            json!({
                "content": [{"type": "text", "text": "ok"}]
            }),
        )),
        skipped_post_hooks: vec![
            SkippedPostHook::AfterToolNotAvailableWithoutTurn,
            SkippedPostHook::OnErrorNotAvailableWithoutTurn,
        ],
    };
    assert_eq!(
        record
            .target
            .as_ref()
            .map(|target| target.wire_tool_name.as_str()),
        Some("show_canvas"),
        "目标身份随结果保留（不回落客户端字段）"
    );
    // 记录可编码（有界存储按字节预算判定，见 `InvocationOutcomeStore`）。
    let encoded = serde_json::to_value(&record).expect("outcome 记录必须可序列化");
    assert_eq!(
        encoded["target"]["effectiveToolName"],
        "mcp__server-1__show_canvas"
    );
    assert!(matches!(
        record.outcome,
        SessionInvocationOutcome::Completed(_)
    ));
    assert_eq!(
        record.skipped_post_hooks[0].as_str(),
        "after_tool:not_available_without_turn"
    );
    assert_eq!(
        record.skipped_post_hooks[1].as_str(),
        "on_error:not_available_without_turn"
    );
}
