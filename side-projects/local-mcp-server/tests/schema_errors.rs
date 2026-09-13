//! 错误边界、错误码与冻结 DTO 形状的测试（WP-001）。
//!
//! 这里做两类事情：
//! 1. **把本地冻结常量钉在 rmcp 3.1.4 的真实 API 上**：错误码、协议版本、`_meta`
//!    必填键、`resultType`、HTTP header 名。任何一端漂移都会立刻失败，避免"字符串
//!    看起来对、实际与 SDK 不一致"。
//! 2. **冻结协议错误与工具业务错误的边界**：未知工具/请求形状 → JSON-RPC error；
//!    输入校验与执行失败 → `isError: true` 的 tool result。
//!
//! 容器期的宿主↔worker 信封用例（请求/响应配对、操作码名称）随 D-003 删除：
//! 单进程形态下没有 RPC 通道，配对语义没有对象。

use rmcp::model::{ErrorCode, ProtocolVersion, RequestMetaObject, ResultType};
use rmcp::transport::common::http_header::{
    HEADER_MCP_METHOD, HEADER_MCP_NAME, HEADER_MCP_PARAM_PREFIX, HEADER_MCP_PROTOCOL_VERSION,
};
use serde_json::json;

use local_mcp_server::error::{code, CapabilityError, ErrorPayload, ToolError};
use local_mcp_server::wire::{
    StructuredOutput, TaskSnapshot, TaskStatus, ToolResponse, MCP_LEGACY_PROTOCOL_VERSION,
    MCP_MODERN_PROTOCOL_VERSION, REQUIRED_REQUEST_META_KEYS, RESULT_TYPE_COMPLETE,
    RESULT_TYPE_INPUT_REQUIRED, SUPPORTED_PROTOCOL_VERSIONS,
};

#[test]
fn test_jsonrpc_codes_match_rmcp_error_codes() {
    assert_eq!(code::PARSE_ERROR, ErrorCode::PARSE_ERROR.0);
    assert_eq!(code::INVALID_REQUEST, ErrorCode::INVALID_REQUEST.0);
    assert_eq!(code::METHOD_NOT_FOUND, ErrorCode::METHOD_NOT_FOUND.0);
    assert_eq!(code::INVALID_PARAMS, ErrorCode::INVALID_PARAMS.0);
    assert_eq!(code::INTERNAL_ERROR, ErrorCode::INTERNAL_ERROR.0);
    assert_eq!(code::HEADER_MISMATCH, ErrorCode::HEADER_MISMATCH.0);
    assert_eq!(
        code::MISSING_REQUIRED_CLIENT_CAPABILITY,
        ErrorCode::MISSING_REQUIRED_CLIENT_CAPABILITY.0
    );
    assert_eq!(
        code::UNSUPPORTED_PROTOCOL_VERSION,
        ErrorCode::UNSUPPORTED_PROTOCOL_VERSION.0
    );
    assert_eq!(
        code::LEGACY_RESOURCE_NOT_FOUND_FORBIDDEN,
        ErrorCode::RESOURCE_NOT_FOUND.0
    );
}

#[test]
fn test_legacy_resource_not_found_code_is_never_emitted() {
    // 2026-07-28 起 -32002 必须不得发出；这里把"不得使用"变成可执行断言。
    for tool_error in [
        ToolError::UnknownTool {
            name: "Nope".to_string(),
        },
        ToolError::InvalidRequest {
            message: "bad params".to_string(),
        },
        ToolError::Internal {
            message: "boom".to_string(),
        },
        ToolError::BackendUnavailable {
            message: "docker missing".to_string(),
        },
    ] {
        assert_ne!(
            tool_error.jsonrpc_code(),
            code::LEGACY_RESOURCE_NOT_FOUND_FORBIDDEN
        );
    }
}

#[test]
fn test_protocol_version_constants_match_rmcp() {
    assert_eq!(
        ProtocolVersion::V_2026_07_28.as_str(),
        MCP_MODERN_PROTOCOL_VERSION
    );
    assert_eq!(
        ProtocolVersion::V_2025_11_25.as_str(),
        MCP_LEGACY_PROTOCOL_VERSION
    );
    // rmcp 的 LATEST 仍是 legacy 版本：这正是我们必须显式声明 modern 的原因。
    assert_eq!(
        ProtocolVersion::LATEST.as_str(),
        MCP_LEGACY_PROTOCOL_VERSION
    );
    for version in SUPPORTED_PROTOCOL_VERSIONS {
        assert!(
            ProtocolVersion::KNOWN_VERSIONS
                .iter()
                .any(|known| known.as_str() == version),
            "rmcp 不认识我们声明的版本 {version}"
        );
    }
    // 标准 HTTP header 从 2026-07-28 起生效。
    assert_eq!(
        ProtocolVersion::STANDARD_HEADERS.as_str(),
        MCP_MODERN_PROTOCOL_VERSION
    );
}

#[test]
fn test_required_request_meta_keys_match_rmcp() {
    let mut ours: Vec<&str> = REQUIRED_REQUEST_META_KEYS.to_vec();
    ours.sort_unstable();
    let mut theirs: Vec<&str> = RequestMetaObject::DRAFT_REQUIRED_KEYS.to_vec();
    theirs.sort_unstable();
    assert_eq!(ours, theirs, "modern 必填 _meta 键必须与 rmcp 一致");
}

#[test]
fn test_result_type_constants_match_rmcp() {
    assert_eq!(ResultType::COMPLETE.as_str(), RESULT_TYPE_COMPLETE);
    assert_eq!(
        ResultType::INPUT_REQUIRED.as_str(),
        RESULT_TYPE_INPUT_REQUIRED
    );
}

#[test]
fn test_streamable_http_header_constants_match_rmcp() {
    assert_eq!(
        HEADER_MCP_PROTOCOL_VERSION,
        local_mcp_server::wire::HEADER_MCP_PROTOCOL_VERSION
    );
    assert_eq!(HEADER_MCP_METHOD, local_mcp_server::wire::HEADER_MCP_METHOD);
    assert_eq!(HEADER_MCP_NAME, local_mcp_server::wire::HEADER_MCP_NAME);
    assert_eq!(
        HEADER_MCP_PARAM_PREFIX,
        local_mcp_server::wire::HEADER_MCP_PARAM_PREFIX
    );
}

#[test]
fn test_unknown_tool_is_a_protocol_error_with_spec_message() {
    let error = ToolError::UnknownTool {
        name: "invalid_tool_name".to_string(),
    };
    let payload = error.to_payload();
    assert_eq!(payload.code, code::INVALID_PARAMS);
    assert_eq!(payload.message, "Unknown tool: invalid_tool_name");
    assert_eq!(payload, error.to_payload(), "载荷必须可重复构造");
}

#[test]
fn test_backend_unavailable_maps_to_internal_error() {
    let error = ToolError::BackendUnavailable {
        message: "docker daemon unreachable".to_string(),
    };
    let payload = error.to_payload();
    assert_eq!(payload.code, code::INTERNAL_ERROR);
    assert!(payload.message.starts_with("Backend unavailable: "));
}

#[test]
fn test_business_failure_is_a_tool_result_not_a_protocol_error() {
    let response = ToolResponse::tool_error(
        "Error: File not found at /workspace/missing.txt",
        StructuredOutput::error("Read"),
    );
    assert!(response.is_error);
    assert_eq!(response.structured["ok"], json!(false));
    assert_eq!(response.structured["truncated"], json!(false));
}

#[test]
fn test_success_result_serializes_frozen_structured_shape() {
    let response = ToolResponse::ok("Replaced text", StructuredOutput::ok("Edit"));
    let encoded = serde_json::to_value(&response).expect("可序列化");
    assert_eq!(encoded["is_error"], json!(false));
    let structured = &encoded["structured"];
    assert_eq!(structured["tool"], json!("Edit"));
    assert_eq!(structured["ok"], json!(true));
    assert_eq!(structured["truncated"], json!(false));
    // 可选字段在缺省时必须缺席，而不是 null。
    assert!(structured.get("persisted_path").is_none());
    assert!(structured.get("task_id").is_none());
    assert!(structured.get("exit_code").is_none());
}

#[test]
fn test_structured_output_extra_fields_are_flattened() {
    let structured = StructuredOutput::ok("Bash")
        .with_extra("background", json!(true))
        .with_extra("lines", json!(12));
    let encoded = serde_json::to_value(&structured).expect("可序列化");
    assert_eq!(encoded["background"], json!(true));
    assert_eq!(encoded["lines"], json!(12));
    assert_eq!(encoded["tool"], json!("Bash"));
}

#[test]
fn test_tool_response_roundtrip() {
    let original = ToolResponse::tool_error_with_json(
        "Error: old_string is not unique in /workspace/a.rs (found 2 occurrences).",
        json!({"tool": "Edit", "ok": false, "truncated": false}),
    );
    let encoded = serde_json::to_string(&original).expect("可序列化");
    let decoded: ToolResponse = serde_json::from_str(&encoded).expect("可反序列化");
    assert_eq!(decoded, original);
}

#[test]
fn test_task_snapshot_serializes_owner_binding_fields() {
    let snapshot = TaskSnapshot {
        task_id: "shell-01928374-0000-7000-8000-000000000000".to_string(),
        owner: "principal-a".to_string(),
        client_instance: "instance-1".to_string(),
        status: TaskStatus::Running,
        pid: Some(4321),
        pgid: Some(4321),
        stdout_log: Some("/workspace/.sandbox/logs/task.stdout".to_string()),
        stderr_log: None,
        exit_code: None,
        started_at: "2026-09-11T10:00:00Z".to_string(),
        ended_at: None,
    };
    let encoded = serde_json::to_value(&snapshot).expect("可序列化");
    assert_eq!(encoded["status"], json!("running"));
    assert_eq!(encoded["owner"], json!("principal-a"));
    assert_eq!(encoded["client_instance"], json!("instance-1"));
    assert!(encoded.get("ended_at").is_none());
    assert!(encoded.get("exit_code").is_none());
}

#[test]
fn test_capability_error_public_message_redacts_resolved_paths() {
    let error = CapabilityError::SymlinkEscape {
        requested: "link/secret.txt".to_string(),
        resolved: "/Users/real-host/private/secret.txt".into(),
    };
    let text = error.public_message();
    assert!(!text.contains("/Users/real-host"));
    assert!(text.contains("link/secret.txt"));
}

#[test]
fn test_error_payload_omits_data_when_absent() {
    let payload = ErrorPayload::new(code::INVALID_PARAMS, "Unknown tool: Nope");
    let encoded = serde_json::to_value(&payload).expect("可序列化");
    assert_eq!(encoded["code"], json!(-32602));
    assert!(encoded.get("data").is_none());
}
