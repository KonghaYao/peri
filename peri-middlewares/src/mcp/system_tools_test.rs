//! `system_tools` 的 crate 内可观察层测试（C-INJ-02）。
//!
//! 断言范围止于 bridge 层：`prepare_system_tools` 的返回值、`is_direct()` 标志、
//! 错误变体、参数保真与「空数组零注入」。首个 LLM 请求的 tools 入参、真实 session
//! catalog / `run_reason` 的断言由 B-07 在 `peri-acp` host seam 承担（主 plan §5 R9），
//! 本文件不调用 `build_session_tool_view`、不 mock catalog。
//!
//! 所有 fixture 都用真实 `rmcp::model::Tool` + `peer: None` 的句柄构造：被测函数
//! 不读连接状态，因此这些 fixture **不构成「协议 ready」证据**，也不能被读成
//! 「System MCP 启动完成」。

use super::*;
use std::sync::Arc;

use rmcp::model::Tool;
use serde_json::json;

use crate::mcp::client::{ClientStatus, McpClientHandle};
use crate::mcp::dynamic::admission::DynamicMcpAdmissionGate;

// ─── fixtures ───────────────────────────────────────────────────────────────

/// 纯匹配用句柄：`peer: None` + `Failed`，避免被误读为已完成握手。
fn fixture_handle(server: &str) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        name: server.to_string(),
        version: None,
        cache_version: None,
        peer: None,
        tools: vec![],
        resources: vec![],
        status: ClientStatus::Failed("fixture: no live transport".to_string()),
        oauth_status: Default::default(),
        source: None,
        url: None,
        skills_capable: false,
        channel_capable: false,
    })
}

/// 经真实反序列化构造 rmcp Tool：`_meta` 与结构非法的 schema 都从这里进入。
fn fixture_tool(tool: serde_json::Value) -> Tool {
    serde_json::from_value(tool).expect("fixture tool 必须能被 rmcp Tool 接收")
}

fn read_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": { "path": { "type": "string" } },
        "required": ["path"]
    })
}

fn bridge(server: &str, tool: &str) -> McpToolBridge {
    bridge_with_schema(server, tool, read_schema())
}

fn bridge_with_schema(server: &str, tool: &str, schema: serde_json::Value) -> McpToolBridge {
    let tool = fixture_tool(json!({
        "name": tool,
        "description": "fixture",
        "inputSchema": schema
    }));
    McpToolBridge::new(server, &tool, fixture_handle(server))
}

/// `_meta.ui.visibility = ["app"]`：只对 App 可见，不可注入模型工具列表。
fn app_only_bridge(server: &str, tool: &str) -> McpToolBridge {
    let tool = fixture_tool(json!({
        "name": tool,
        "description": "fixture",
        "inputSchema": read_schema(),
        "_meta": { "ui": { "visibility": ["app"] } }
    }));
    McpToolBridge::new(server, &tool, fixture_handle(server))
}

fn required(entries: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
    entries
        .iter()
        .map(|(server, tools)| {
            (
                (*server).to_string(),
                tools.iter().map(|tool| (*tool).to_string()).collect(),
            )
        })
        .collect()
}

fn names(bridges: &[McpToolBridge]) -> Vec<String> {
    bridges
        .iter()
        .map(|bridge| bridge.name().to_string())
        .collect()
}

fn direct_names(bridges: &[McpToolBridge]) -> Vec<String> {
    bridges
        .iter()
        .filter(|bridge| bridge.is_direct())
        .map(|bridge| bridge.name().to_string())
        .collect()
}

/// `Vec<McpToolBridge>` 未实现 `Debug`，无法使用 `unwrap_err` / `expect_err`。
fn expect_ok(
    bridges: Vec<McpToolBridge>,
    required: &BTreeMap<String, Vec<String>>,
) -> Vec<McpToolBridge> {
    match prepare_system_tools(bridges, required) {
        Ok(prepared) => prepared,
        Err(error) => panic!("期望 Ok，实际返回 {error}"),
    }
}

fn expect_error(
    bridges: Vec<McpToolBridge>,
    required: &BTreeMap<String, Vec<String>>,
) -> SystemToolError {
    match prepare_system_tools(bridges, required) {
        Ok(_) => panic!("期望 Err（all-or-nothing），实际返回 Ok"),
        Err(error) => error,
    }
}

// ─── 解析与命名空间 ─────────────────────────────────────────────────────────

#[test]
fn test_system_required_tools_resolve_in_own_namespace() {
    let before = vec![
        "mcp__workspace__Read".to_string(),
        "mcp__archive__Read".to_string(),
    ];
    let before_params: Vec<serde_json::Value> = vec![read_schema(), read_schema()];

    let prepared = expect_ok(
        vec![bridge("workspace", "Read"), bridge("archive", "Read")],
        &required(&[("workspace", &["Read"])]),
    );

    // 长度、顺序与有效名集合不变；只有所属 namespace 命中项被提升。
    assert_eq!(names(&prepared), before);
    assert_eq!(
        prepared
            .iter()
            .map(|bridge| bridge.parameters())
            .collect::<Vec<_>>(),
        before_params
    );
    assert_eq!(
        direct_names(&prepared),
        vec!["mcp__workspace__Read".to_string()]
    );
}

#[test]
fn test_system_required_tool_names_are_case_sensitive() {
    let error = expect_error(
        vec![bridge("workspace", "read")],
        &required(&[("workspace", &["Read"])]),
    );

    // 不折叠大小写：报错内容与配置项逐字对应。
    assert_eq!(
        error,
        SystemToolError::MissingTool {
            server: "workspace".to_string(),
            tool: "Read".to_string(),
        }
    );
}

#[test]
fn test_system_effective_prefix_is_not_stripped() {
    let error = expect_error(
        vec![bridge("workspace", "Read")],
        &required(&[("workspace", &["mcp__workspace__Read"])]),
    );

    // 配置数组按**原始工具名**匹配，不接受 effective name 作为替代写法。
    assert_eq!(
        error,
        SystemToolError::MissingTool {
            server: "workspace".to_string(),
            tool: "mcp__workspace__Read".to_string(),
        }
    );
}

#[test]
fn test_system_missing_tool_returns_explicit_error() {
    // 另一 server 的同名工具不能补足本 server 的必需项，也不退化为「空 Ok」。
    let error = expect_error(
        vec![bridge("workspace", "Glob"), bridge("archive", "Read")],
        &required(&[("workspace", &["Read"])]),
    );

    assert_eq!(
        error,
        SystemToolError::MissingTool {
            server: "workspace".to_string(),
            tool: "Read".to_string(),
        }
    );
}

#[test]
fn test_system_error_selection_is_deterministic() {
    let bridges = vec![bridge("workspace", "Read")];
    let required = required(&[("zulu", &["Read"]), ("alpha", &["Read"])]);

    let error = expect_error(bridges.clone(), &required);
    assert_eq!(
        error,
        SystemToolError::MissingTool {
            server: "alpha".to_string(),
            tool: "Read".to_string(),
        }
    );

    // 同批 fixture 的克隆不携带任何提升状态，重复调用结论一致。
    assert_eq!(expect_error(bridges, &required), error);
}

#[test]
fn test_system_ambiguous_raw_tool_is_rejected() {
    let error = expect_error(
        vec![bridge("workspace", "Read"), bridge("workspace", "Read")],
        &required(&[("workspace", &["Read"])]),
    );

    assert_eq!(
        error,
        SystemToolError::AmbiguousTool {
            server: "workspace".to_string(),
            tool: "Read".to_string(),
            matches: vec![
                "mcp__workspace__Read".to_string(),
                "mcp__workspace__Read".to_string(),
            ],
        }
    );
}

#[test]
fn test_system_effective_name_collision_is_rejected() {
    // 净化碰撞：不同原始 server 名落到同一 effective name。
    let error = expect_error(
        vec![bridge("a.b", "Read"), bridge("a_b", "Read")],
        &required(&[("a.b", &["Read"])]),
    );
    assert_eq!(
        error,
        SystemToolError::EffectiveNameCollision {
            effective_name: "mcp__a_b__Read".to_string(),
        }
    );

    // ASCII 大小写折叠冲突：执行期无法区分这两个名字。
    let error = expect_error(
        vec![bridge("workspace", "Read"), bridge("workspace", "read")],
        &required(&[("workspace", &["Read"])]),
    );
    assert_eq!(
        error,
        SystemToolError::EffectiveNameCollision {
            effective_name: "mcp__workspace__Read".to_string(),
        }
    );

    // 与必需项无关的普通 deferred 冲突沿用既有策略，本函数不改写。
    let prepared = expect_ok(
        vec![
            bridge("a.b", "Other"),
            bridge("a_b", "Other"),
            bridge("workspace", "Read"),
        ],
        &required(&[("workspace", &["Read"])]),
    );
    assert_eq!(
        direct_names(&prepared),
        vec!["mcp__workspace__Read".to_string()]
    );
}

#[test]
fn test_system_app_only_required_tool_is_rejected() {
    let bridges = vec![app_only_bridge("workspace", "Read")];
    // fixture 自检：`_meta` 确实把该工具设为 app-only。
    assert!(!bridges[0].visible_to_model());

    let error = expect_error(bridges, &required(&[("workspace", &["Read"])]));

    // 必需项不覆盖 model visibility，也不会被改造成模型工具后算作成功。
    assert_eq!(
        error,
        SystemToolError::NotModelVisible {
            server: "workspace".to_string(),
            tool: "Read".to_string(),
        }
    );
}

// ─── schema 结构解析 ────────────────────────────────────────────────────────

#[test]
fn test_system_invalid_schema_returns_explicit_error() {
    let cases: Vec<(&str, serde_json::Value, &str, &str)> = vec![
        (
            "properties 非映射",
            json!({ "type": "object", "properties": 42 }),
            "/properties",
            RULE_MAP,
        ),
        (
            "properties 值非 schema",
            json!({ "type": "object", "properties": { "path": "oops" } }),
            "/properties/path",
            RULE_NODE,
        ),
        (
            "required 非数组",
            json!({ "type": "object", "required": "path" }),
            "/required",
            RULE_REQUIRED,
        ),
        (
            "required 元素非字符串",
            json!({ "type": "object", "required": ["path", 1] }),
            "/required/1",
            RULE_REQUIRED_ITEM,
        ),
        (
            "required 重复",
            json!({ "type": "object", "required": ["path", "path"] }),
            "/required/1",
            RULE_REQUIRED_ITEM,
        ),
        (
            "嵌套 type 非法",
            json!({ "type": "object", "properties": { "path": { "type": "json" } } }),
            "/properties/path/type",
            RULE_TYPE,
        ),
        (
            "type 数组为空",
            json!({ "type": "object", "properties": { "path": { "type": [] } } }),
            "/properties/path/type",
            RULE_TYPE,
        ),
        (
            "type 数组重复",
            json!({ "type": "object", "properties": { "path": { "type": ["string", "string"] } } }),
            "/properties/path/type/1",
            RULE_TYPE,
        ),
        (
            "allOf 非数组",
            json!({ "type": "object", "allOf": { "type": "object" } }),
            "/allOf",
            RULE_LIST,
        ),
        (
            "allOf 元素非 schema",
            json!({ "type": "object", "allOf": [3] }),
            "/allOf/0",
            RULE_NODE,
        ),
        (
            "$defs 值非 schema",
            json!({ "type": "object", "$defs": { "leaf": 7 } }),
            "/$defs/leaf",
            RULE_NODE,
        ),
        (
            "enum 空数组",
            json!({ "type": "object", "enum": [] }),
            "/enum",
            RULE_ENUM,
        ),
        (
            "maxLength 非非负整数",
            json!({ "type": "object", "properties": { "path": { "maxLength": -1 } } }),
            "/properties/path/maxLength",
            RULE_INTEGER,
        ),
        (
            "uniqueItems 非布尔",
            json!({ "type": "object", "properties": { "path": { "uniqueItems": "no" } } }),
            "/properties/path/uniqueItems",
            RULE_BOOLEAN,
        ),
        (
            "$ref 非字符串",
            json!({ "type": "object", "$ref": 42 }),
            "/$ref",
            RULE_STRING,
        ),
        (
            "minimum 非数值",
            json!({ "type": "object", "properties": { "path": { "minimum": "1" } } }),
            "/properties/path/minimum",
            RULE_NUMBER,
        ),
    ];

    for (label, schema, path, rule) in cases {
        let error = expect_error(
            vec![bridge_with_schema("workspace", "Read", schema)],
            &required(&[("workspace", &["Read"])]),
        );
        let SystemToolError::InvalidSchema {
            server,
            tool,
            reason,
        } = &error
        else {
            panic!("{label}: 期望 InvalidSchema，实际 {error}");
        };
        assert_eq!(server, "workspace", "{label}");
        assert_eq!(tool, "Read", "{label}");
        assert!(
            reason.contains(path),
            "{label}: 缺少字段路径 {path}，实际 {reason}"
        );
        assert!(
            reason.contains(rule),
            "{label}: 缺少固定原因，实际 {reason}"
        );
    }
}

#[test]
fn test_system_schema_root_must_be_object() {
    for schema in [json!([]), json!("oops"), json!(42), json!(true)] {
        let reason = validate_input_schema(&schema).expect_err("根非 object 必须失败");
        assert!(reason.starts_with(RULE_ROOT_OBJECT), "{reason}");
        assert!(reason.ends_with(" at /"), "{reason}");
    }

    // `{}` 是未声明约束的 object schema，合法。
    assert_eq!(validate_input_schema(&json!({})), Ok(()));

    // 根 `type` 若存在必须是 "object"。
    for schema in [json!({ "type": "array" }), json!({ "type": ["object"] })] {
        let reason = validate_input_schema(&schema).expect_err("根 type 非 object 必须失败");
        assert!(reason.contains(RULE_ROOT_TYPE), "{reason}");
    }
    assert_eq!(validate_input_schema(&json!({ "type": "object" })), Ok(()));
}

#[test]
fn test_system_invalid_schema_reason_does_not_leak_values() {
    let schema = json!({
        "type": "object",
        "properties": {
            "path": { "type": "bogus-type", "default": "fixture-marker-do-not-print" }
        }
    });
    let error = expect_error(
        vec![bridge_with_schema("workspace", "Read", schema)],
        &required(&[("workspace", &["Read"])]),
    );

    let SystemToolError::InvalidSchema { reason, .. } = &error else {
        panic!("期望 InvalidSchema，实际 {error}");
    };
    assert!(reason.contains("/properties/path/type"), "{reason}");
    // 错误只含字段路径与固定规则，不回显非法值或 annotation 数据。
    assert!(!reason.contains("bogus-type"), "{reason}");
    assert!(!reason.contains("fixture-marker-do-not-print"), "{reason}");

    let error = expect_error(
        vec![bridge_with_schema(
            "workspace",
            "Read",
            json!({ "type": "object", "properties": 42 }),
        )],
        &required(&[("workspace", &["Read"])]),
    );
    let SystemToolError::InvalidSchema { reason, .. } = &error else {
        panic!("期望 InvalidSchema，实际 {error}");
    };
    assert!(!reason.contains("42"), "{reason}");
}

#[test]
fn test_system_valid_schema_preserves_extensions_and_data() {
    let schema = json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "minLength": 1,
                "maxLength": 4096,
                "format": "uri-reference",
                "pattern": "^/",
                "examples": ["/tmp/a"]
            },
            "mode": { "enum": ["read", "write"], "default": "read", "const": null },
            "flag": { "type": ["boolean", "null"], "deprecated": false, "readOnly": true },
            "options": {
                "type": "object",
                "properties": { "recursive": true },
                "additionalProperties": false,
                "x-vendor-extension": { "anything": [1, "two", null] }
            },
            "window": {
                "if": { "type": "object" },
                "then": { "required": ["start"] },
                "else": false,
                "not": { "type": "array", "items": { "type": "integer" }, "uniqueItems": true }
            },
            "bounds": { "minimum": 0, "exclusiveMaximum": 10, "multipleOf": 0.5 },
            "quota": { "$ref": "#/$defs/quota", "$comment": "vendor note" }
        },
        "required": ["path"],
        "patternProperties": { "^x-": { "type": "string" } },
        "propertyNames": { "pattern": "^[a-z]" },
        "dependentSchemas": { "path": { "required": ["mode"] } },
        "$defs": {
            "quota": { "type": "array", "prefixItems": [{ "type": "integer" }, true] }
        },
        "definitions": { "legacy": { "type": "object" } },
        "allOf": [{ "anyOf": [{ "type": "object" }, { "type": "null" }] }],
        "oneOf": [{ "contains": { "type": "string" } }],
        "unevaluatedPropertyLookup": { "unknown-keyword": [1, 2] }
    });

    let prepared = expect_ok(
        vec![bridge_with_schema("workspace", "Read", schema.clone())],
        &required(&[("workspace", &["Read"])]),
    );

    // MCP 原始 schema 被完整保留，未被改写或补齐。
    assert_eq!(prepared[0].parameters(), schema);
    assert_eq!(
        direct_names(&prepared),
        vec!["mcp__workspace__Read".to_string()]
    );
}

// ─── 空数组、幂等与 all-or-nothing ──────────────────────────────────────────

#[test]
fn test_system_empty_required_array_adds_no_direct_tools() {
    // 空数组只表示「不注入额外工具」，ready 由 B 的闸门判定：本测试不是 ready 测试。
    let prepared = expect_ok(
        vec![
            bridge("workspace", "Read"),
            bridge("workspace", "Write"),
            // 非必需工具的结构非法 schema 不参与必需检查。
            bridge_with_schema(
                "other",
                "Broken",
                json!({ "type": "object", "properties": 42 }),
            ),
        ],
        &required(&[("workspace", &[])]),
    );

    assert_eq!(prepared.len(), 3);
    assert!(direct_names(&prepared).is_empty());

    // 该 server 一个 bridge 都没有时，空数组也不做存在性判断（C 无法证明 ready）。
    let prepared = expect_ok(vec![bridge("other", "Read")], &required(&[("absent", &[])]));
    assert_eq!(prepared.len(), 1);
    assert!(direct_names(&prepared).is_empty());
}

#[test]
fn test_system_duplicate_requirements_do_not_duplicate_registration() {
    let prepared = expect_ok(
        vec![bridge("workspace", "Read"), bridge("workspace", "Write")],
        &required(&[("workspace", &["Read", "Read"])]),
    );

    assert_eq!(prepared.len(), 2, "重复配置不得增加注册数");
    assert_eq!(
        names(&prepared)
            .iter()
            .filter(|name| name.as_str() == "mcp__workspace__Read")
            .count(),
        1,
        "Vec 层不得出现第二份同名注册"
    );
    assert_eq!(
        direct_names(&prepared),
        vec!["mcp__workspace__Read".to_string()]
    );
}

#[test]
fn test_system_repeated_admission_is_idempotent() {
    let required = required(&[("workspace", &["Read"])]);
    let first = expect_ok(
        vec![bridge("workspace", "Read"), bridge("workspace", "Glob")],
        &required,
    );
    let second = expect_ok(first, &required);

    assert_eq!(second.len(), 2);
    assert_eq!(
        direct_names(&second),
        vec!["mcp__workspace__Read".to_string()]
    );
}

#[test]
fn test_system_validation_is_all_or_nothing() {
    let broken = json!({ "type": "object", "properties": 42 });
    let error = expect_error(
        vec![
            bridge("workspace", "Read"),
            bridge_with_schema("workspace", "Write", broken.clone()),
        ],
        &required(&[("workspace", &["Read", "Write"])]),
    );
    assert!(
        matches!(error, SystemToolError::InvalidSchema { .. }),
        "{error}"
    );

    // 失败后重建同一批 fixture：没有任何 direct 标记泄漏到共享状态（无部分成功）。
    let rebuilt = vec![
        bridge("workspace", "Read"),
        bridge_with_schema("workspace", "Write", broken),
    ];
    assert!(rebuilt.iter().all(|bridge| !bridge.is_direct()));

    // 非法项留在集合中但不被要求时，合法必需项可独立通过。
    let prepared = expect_ok(rebuilt, &required(&[("workspace", &["Read"])]));
    assert_eq!(prepared.len(), 2);
    assert_eq!(
        direct_names(&prepared),
        vec!["mcp__workspace__Read".to_string()]
    );
}

#[test]
fn test_system_dynamic_control_remains_deferred() {
    let tool = fixture_tool(json!({
        "name": "ShadowTool",
        "description": "fixture",
        "inputSchema": read_schema()
    }));
    let dynamic = McpToolBridge::new_dynamic(
        "dynamic",
        &tool,
        fixture_handle("dynamic"),
        DynamicMcpAdmissionGate::new(),
    )
    .expect("动态 fixture 名称合法");
    assert!(!dynamic.is_direct(), "动态 bridge 缺省保持 deferred");

    let prepared = expect_ok(
        vec![bridge("workspace", "Read"), dynamic],
        &required(&[("workspace", &["Read"])]),
    );

    assert_eq!(
        direct_names(&prepared),
        vec!["mcp__workspace__Read".to_string()]
    );
    let dynamic = prepared
        .iter()
        .find(|bridge| bridge.name() == "mcp__dynamic__ShadowTool")
        .expect("动态 bridge 仍在集合中");
    assert!(!dynamic.is_direct(), "动态 gate 路径不得被自动提升");
}
