//! WP-006：七工具在 HTTP 原始 wire 上的完整性（R-002 / R-006 / FC-MCP-01/02/03）。
//!
//! 这里验证的是**传输层**：同一条 HTTP 通道上，两种生命周期都能完成
//! `initialize`/`discover`、`tools/list`、七工具 `tools/call` 与错误族，且 schema
//! 逐字来自黄金夹具。工具语义本身由 WP-002/003 的单测与 WP-007 的集成用例负责
//! （见 handoff 的 Next Consumer）。

mod http_support;

use serde_json::{json, Value};

use http_support::*;
use local_mcp_server::wire::{SUPPORTED_PROTOCOL_VERSIONS, TOOL_NAMES};

/// 每个工具的最小合法参数（夹具只要求 `required` 字段存在）。
fn minimal_arguments(tool: &str) -> Value {
    match tool {
        "Read" => json!({ "file_path": "a.txt" }),
        "Write" => json!({ "file_path": "b.txt", "content": "hello" }),
        "Edit" => json!({ "file_path": "c.txt", "old_string": "a", "new_string": "b" }),
        "Glob" => json!({ "pattern": "**/*.rs" }),
        "Grep" => json!({ "pattern": "needle" }),
        "folder_operations" => json!({ "operation": "list", "folder_path": "/workspace" }),
        "Bash" => json!({ "command": "echo hi" }),
        other => panic!("未登记的工具: {other}"),
    }
}

/// `tools/list` 必须恰好七个工具，且 description/inputSchema 与黄金夹具逐字一致。
#[test]
fn modern_tools_list_matches_frozen_fixtures() {
    let server = TestServer::start();
    let mut client = server.connect();
    let response = client.send(&modern_tools_list(1));
    assert_eq!(response.status, 200);

    let result = rpc_result(&response);
    assert_eq!(
        result.get("resultType").and_then(Value::as_str),
        Some("complete"),
        "modern 结果必须带 resultType"
    );
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .expect("tools 数组");

    let fixtures = load_tool_fixtures();
    assert_eq!(tools.len(), TOOL_NAMES.len(), "tools/list 恰好七工具");
    assert_eq!(tools.len(), fixtures.len());

    for (index, fixture) in fixtures.iter().enumerate() {
        let tool = &tools[index];
        assert_eq!(
            tool.get("name").and_then(Value::as_str),
            Some(fixture.name.as_str()),
            "工具顺序必须与冻结顺序一致"
        );
        assert_eq!(
            tool.get("description").and_then(Value::as_str),
            Some(fixture.description.as_str()),
            "{} 的 description 必须与夹具逐字一致",
            fixture.name
        );
        assert_eq!(
            tool.get("inputSchema"),
            Some(&fixture.schema),
            "{} 的 inputSchema 必须与夹具逐字一致",
            fixture.name
        );
        // 别名不是 tools/list 条目。
        assert_ne!(tool.get("name").and_then(Value::as_str), Some("reading"));
        assert_ne!(tool.get("name").and_then(Value::as_str), Some("Shell"));
    }
}

/// modern 生命周期：七个工具全部成功，且文本与结构化事实一致。
#[test]
fn modern_all_seven_tools_succeed_over_raw_http() {
    let server = TestServer::start();
    let mut client = server.connect();

    for (index, tool) in TOOL_NAMES.iter().enumerate() {
        let id = index as u64 + 1;
        let response = client.send(&modern_tools_call(tool, minimal_arguments(tool), id));
        assert_eq!(response.status, 200, "{tool} 状态码");
        let result = rpc_result(&response);
        assert_eq!(
            result.get("isError").and_then(Value::as_bool),
            Some(false),
            "{tool} 不应是业务错误"
        );
        assert_eq!(
            result.get("resultType").and_then(Value::as_str),
            Some("complete")
        );
        let structured = structured_content(&response);
        assert_eq!(
            structured.get("tool").and_then(Value::as_str),
            Some(*tool),
            "{tool} 的 structuredContent.tool 必须与请求一致"
        );
        assert_eq!(structured.get("ok").and_then(Value::as_bool), Some(true));
        // P-21：文本与结构化内容描述同一事实。
        let text = result
            .get("content")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("text"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            text.contains(tool),
            "{tool} 的文本内容必须点明工具名: {text}"
        );
    }
}

/// legacy 生命周期：`initialize` → `initialized` → 七个工具全部成功。
#[test]
fn legacy_all_seven_tools_succeed_over_raw_http() {
    let server = TestServer::start();
    let mut client = server.connect();

    let initialize = client.send(&legacy_initialize(1));
    assert_eq!(initialize.status, 200);
    let session = initialize
        .header("mcp-session-id")
        .expect("会话 id")
        .to_string();
    assert_eq!(
        client
            .send(&legacy_initialized_notification(&session))
            .status,
        202
    );

    for (index, tool) in TOOL_NAMES.iter().enumerate() {
        let id = index as u64 + 2;
        let response = client.send(&legacy_tools_call(
            tool,
            minimal_arguments(tool),
            id,
            &session,
        ));
        assert_eq!(response.status, 200, "{tool} 状态码");
        let structured = structured_content(&response);
        assert_eq!(structured.get("tool").and_then(Value::as_str), Some(*tool));
        let identity = identity_of(&response);
        assert_eq!(identity.session_header.as_deref(), Some(session.as_str()));
    }
}

/// 至少三类独立错误：未知工具、缺必填参数（业务错误）、缺必填 `_meta`、header 不一致。
#[test]
fn http_error_families_are_distinct_and_traceable() {
    let server = TestServer::start();
    let mut client = server.connect();

    // 1) 未知工具 → 协议错误 -32602，文案与冻结映射一致。
    let unknown = client.send(&modern_tools_call("Nonexistent", json!({}), 1));
    assert_eq!(unknown.status, 400);
    assert_eq!(rpc_error_code(&unknown), -32602);
    assert_eq!(rpc_error_message(&unknown), "Unknown tool: Nonexistent");

    // 2) 缺必填参数 → 业务错误（isError=true），文案来自黄金夹具。
    let missing_argument = client.send(&modern_tools_call(
        "Edit",
        json!({ "file_path": "c.txt" }),
        2,
    ));
    assert_eq!(missing_argument.status, 200, "业务错误走 200 + isError");
    let result = rpc_result(&missing_argument);
    assert_eq!(result.get("isError").and_then(Value::as_bool), Some(true));
    let expected = load_tool_fixtures()
        .into_iter()
        .find(|fixture| fixture.name == "Edit")
        .and_then(|fixture| fixture.required_messages.get("old_string").cloned())
        .expect("夹具必须提供 old_string 的文案");
    assert_eq!(
        result
            .get("content")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("text"))
            .and_then(Value::as_str),
        Some(expected.as_str())
    );

    // 3) 缺必填 `_meta` → 协议错误 -32602。
    let missing_meta = client.send(
        &RawRequest::post(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "Read", "arguments": { "file_path": "a.txt" } }
        }))
        .header("MCP-Protocol-Version", "2026-07-28")
        .header("Mcp-Method", "tools/call")
        .header("Mcp-Name", "Read"),
    );
    assert_eq!(missing_meta.status, 400);
    assert_eq!(rpc_error_code(&missing_meta), -32602);

    // 4) header/body 不一致 → -32020。
    let mismatch = server.connect().send(
        &modern_tools_call("Read", json!({ "file_path": "a.txt" }), 4).header("Mcp-Name", "Grep"),
    );
    assert_eq!(mismatch.status, 400);
    assert_eq!(rpc_error_code(&mismatch), -32020);

    // 四个错误互不相同，且没有一个是 -32002（已废止码）。
    let codes = [
        rpc_error_code(&unknown),
        rpc_error_code(&missing_meta),
        rpc_error_code(&mismatch),
    ];
    assert!(!codes.contains(&-32002), "不得发出已废止的 -32002");
}

/// 别名只在 `tools/call` 名称解析生效（`reading` → Read，`Shell` → Bash）。
#[test]
fn aliases_resolve_on_the_wire_without_becoming_tools() {
    let server = TestServer::start();
    let mut client = server.connect();

    for (alias, canonical) in [("reading", "Read"), ("Shell", "Bash")] {
        let arguments = minimal_arguments(canonical);
        let response = client.send(&modern_tools_call(alias, arguments, 1));
        assert_eq!(response.status, 200, "别名 {alias} 必须可用");
        let structured = structured_content(&response);
        assert_eq!(
            structured.get("tool").and_then(Value::as_str),
            Some(canonical),
            "别名必须解析到规范名"
        );
        assert_eq!(
            structured.get("requested_name").and_then(Value::as_str),
            Some(alias),
            "原始名称应保留供诊断"
        );
    }
}

/// modern `server/discover` 必须列出两个受支持版本（P-07/P-23）。
#[test]
fn discover_reports_supported_versions_over_http() {
    let server = TestServer::start();
    let mut client = server.connect();
    let response = client.send(&modern_discover(1));
    assert_eq!(response.status, 200);
    let result = rpc_result(&response);
    let versions: Vec<&str> = result
        .get("supportedVersions")
        .and_then(Value::as_array)
        .expect("supportedVersions")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for version in SUPPORTED_PROTOCOL_VERSIONS {
        assert!(
            versions.contains(&version),
            "discover 必须声明 {version}，实际 {versions:?}"
        );
    }
    assert!(
        result.get("serverInfo").is_some()
            || result
                .get("_meta")
                .and_then(|meta| meta.get("io.modelcontextprotocol/serverInfo"))
                .is_some(),
        "discover 必须带 serverInfo"
    );
}

/// 不支持协议版本 → `-32022` 且 `data.supported` 列出真实支持集（P-06）。
#[test]
fn unsupported_protocol_version_is_rejected() {
    let server = TestServer::start();
    let mut client = server.connect();
    let response = client.send(
        &modern_request("tools/list", json!({}), None, 1)
            .header("MCP-Protocol-Version", "1900-01-01")
            .body_bytes(
                serde_json::to_vec(&json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/list",
                    "params": {
                        "_meta": {
                            "io.modelcontextprotocol/protocolVersion": "1900-01-01",
                            "io.modelcontextprotocol/clientCapabilities": {}
                        }
                    }
                }))
                .expect("serialize"),
            ),
    );
    assert_eq!(response.status, 400, "不支持的版本必须 400");
    assert_eq!(rpc_error_code(&response), -32022);
    let data = rpc_message(&response)
        .get("error")
        .and_then(|error| error.get("data"))
        .cloned()
        .unwrap_or(Value::Null);
    let supported = data
        .get("supported")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        !supported.is_empty(),
        "错误 data 必须列出支持版本，实际 {data}"
    );
}
