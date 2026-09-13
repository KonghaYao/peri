//! `mcp::error_map` 的单元测试（WP-005）。
//!
//! 断言的是**边界本身**：错误码只能来自冻结表、`-32002` 永不出现、业务错误走
//! tool result、`serverInfo` 不可被工具覆盖。

use rmcp::model::{ErrorCode, Implementation};
use serde_json::{json, Map, Value};

use super::{call_tool_result, mcp_error, meta_with_server_info, rpc_error};
use crate::error::{code, ToolError};
use crate::wire::{ToolResponse, META_KEY_SERVER_INFO};

fn server_info() -> Implementation {
    Implementation::new("local-mcp-server", "0.1.0")
}

#[test]
fn test_tool_error_maps_to_frozen_jsonrpc_codes() {
    let unknown = ToolError::UnknownTool {
        name: "invalid_tool_name".to_string(),
    };
    let invalid = ToolError::InvalidRequest {
        message: "arguments must be an object".to_string(),
    };
    let internal = ToolError::Internal {
        message: "execution interrupted".to_string(),
    };
    let backend = ToolError::BackendUnavailable {
        message: "capability root is gone".to_string(),
    };

    assert_eq!(mcp_error(&unknown).code, ErrorCode(code::INVALID_PARAMS));
    assert_eq!(
        mcp_error(&unknown).message,
        "Unknown tool: invalid_tool_name"
    );
    assert_eq!(mcp_error(&invalid).code, ErrorCode(code::INVALID_PARAMS));
    assert_eq!(mcp_error(&internal).code, ErrorCode(code::INTERNAL_ERROR));
    assert_eq!(mcp_error(&backend).code, ErrorCode(code::INTERNAL_ERROR));
    assert!(
        mcp_error(&backend)
            .message
            .starts_with("Backend unavailable:"),
        "后端不可用必须明确标注 fail closed 的原因"
    );
}

#[test]
fn test_emitted_codes_never_include_legacy_resource_not_found() {
    let codes = [
        mcp_error(&ToolError::UnknownTool {
            name: "x".to_string(),
        })
        .code,
        mcp_error(&ToolError::InvalidRequest {
            message: "x".to_string(),
        })
        .code,
        mcp_error(&ToolError::Internal {
            message: "x".to_string(),
        })
        .code,
        mcp_error(&ToolError::BackendUnavailable {
            message: "x".to_string(),
        })
        .code,
        rpc_error(code::INVALID_PARAMS, "resource not found").code,
    ];
    let allowed = [
        code::INVALID_REQUEST,
        code::METHOD_NOT_FOUND,
        code::INVALID_PARAMS,
        code::INTERNAL_ERROR,
        code::PARSE_ERROR,
        code::HEADER_MISMATCH,
        code::MISSING_REQUIRED_CLIENT_CAPABILITY,
        code::UNSUPPORTED_PROTOCOL_VERSION,
    ];
    for emitted in codes {
        assert!(
            allowed.contains(&emitted.0),
            "发出了冻结表之外的错误码 {}",
            emitted.0
        );
        assert_ne!(
            emitted.0,
            code::LEGACY_RESOURCE_NOT_FOUND_FORBIDDEN,
            "-32002 在本协议版本必须不得发出"
        );
    }
}

#[test]
fn test_success_result_carries_text_structured_content_and_server_info() {
    let response = ToolResponse::ok(
        "     1\thello",
        crate::wire::StructuredOutput::ok("Read").with_extra("lines", json!(1)),
    );
    let result = call_tool_result(&response, &server_info());

    assert_eq!(result.is_error, Some(false));
    let text = result.content[0]
        .as_text()
        .expect("成功结果必须带文本内容")
        .text
        .clone();
    assert_eq!(text, "     1\thello");
    let structured = result
        .structured_content
        .clone()
        .expect("structuredContent");
    assert_eq!(structured["tool"], "Read");
    assert_eq!(structured["ok"], true);
    assert_eq!(structured["lines"], 1);
    let meta = result.meta.expect("结果必须带 _meta");
    assert_eq!(meta.0[META_KEY_SERVER_INFO]["name"], "local-mcp-server");
    assert_eq!(meta.0[META_KEY_SERVER_INFO]["version"], "0.1.0");
}

#[test]
fn test_business_error_stays_a_tool_result_not_a_protocol_error() {
    let response = ToolResponse::tool_error(
        "Error: File not found at a.txt",
        crate::wire::StructuredOutput::error("Read"),
    );
    let result = call_tool_result(&response, &server_info());

    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.content[0].as_text().expect("错误文本必须可见").text,
        "Error: File not found at a.txt"
    );
    assert_eq!(
        result.structured_content.expect("structuredContent")["ok"],
        false
    );
}

#[test]
fn test_structured_only_result_does_not_invent_placeholder_text() {
    let response = ToolResponse::ok("", crate::wire::StructuredOutput::ok("Glob"));
    let result = call_tool_result(&response, &server_info());

    assert!(result.content.is_empty(), "空文本不得补造占位文本");
    assert!(result.structured_content.is_some(), "结构化内容必须保留");
}

#[test]
fn test_tool_metadata_cannot_override_server_identity() {
    let mut extra = Map::new();
    extra.insert(
        META_KEY_SERVER_INFO.to_string(),
        json!({"name": "forged", "version": "9.9.9"}),
    );
    extra.insert("sandbox/tool-note".to_string(), Value::from("kept"));

    let meta = meta_with_server_info(Some(&extra), &server_info());
    assert_eq!(meta.0[META_KEY_SERVER_INFO]["name"], "local-mcp-server");
    assert_eq!(meta.0["sandbox/tool-note"], "kept");
}
