//! `plugin.rs` 的 MCP 配置契约测试（自内联模块迁出，受 STD-SIZE-001 约束）。

use super::*;

/// 仅含缺省字段的 typed 配置（`McpServerConfig` 没有 `Default`）。
fn empty_config() -> McpServerConfig {
    McpServerConfig {
        command: None,
        args: None,
        env: None,
        url: None,
        headers: None,
        oauth: None,
        disabled: None,
        dynamic: false,
        subscriptions: None,
        system_mcp: None,
        system_mcp_tools: None,
        system_mcp_timeout: None,
        source: None,
    }
}

fn parse(json: &str) -> Result<McpServerConfig, serde_json::Error> {
    serde_json::from_str(json)
}

/// 旧 JSON 兼容：新增 key 全部缺省为 None，输出不出现新 key，既有语义不变。
#[test]
fn test_system_mcp_legacy_defaults() {
    let cfg = parse(r#"{"command":"npx"}"#).expect("既有 JSON 必须仍可解析");
    assert!(cfg.system_mcp.is_none(), "旧 JSON 不得推断出启动依赖");
    assert!(cfg.system_mcp_tools.is_none());
    assert!(cfg.system_mcp_timeout.is_none());
    assert!(cfg.validate().is_ok(), "缺省 System 字段必须合法");
    assert!(cfg.source.is_none(), "source 是运行时标记，不从 wire 读取");

    let json = serde_json::to_value(&cfg).unwrap();
    for key in ["system_mcp", "system_mcp_tools", "system_mcp_timeout"] {
        assert!(json.get(key).is_none(), "缺省不得序列化 {key}: {json}");
    }
    assert!(json.get("source").is_none(), "source 不进入 wire: {json}");

    let legacy = parse(r#"{"command":"npx","disabled":true,"args":["-y"]}"#).unwrap();
    assert_eq!(legacy.disabled, Some(true));
    assert_eq!(legacy.args, Some(vec!["-y".to_string()]));
}

/// 契约 1：`system_mcp_tools` 没有 `system_mcp = true` 一律非法，含显式 `[]`。
#[test]
fn test_system_mcp_tools_requires_true() {
    for json in [
        r#"{"command":"npx","system_mcp_tools":[]}"#,
        r#"{"command":"npx","system_mcp_tools":["Read"]}"#,
        r#"{"command":"npx","system_mcp":false,"system_mcp_tools":[]}"#,
        r#"{"command":"npx","system_mcp":false,"system_mcp_tools":["Read"]}"#,
        r#"{"command":"npx","systemMcpTools":["Read"]}"#,
    ] {
        let err = parse(json).expect_err("非法组合必须解析失败");
        assert!(
            err.to_string()
                .contains("system_mcp_tools requires system_mcp = true"),
            "固定规则正文必须保留: {json} -> {err}"
        );
    }

    let typed = McpServerConfig {
        system_mcp: Some(false),
        system_mcp_tools: Some(vec![]),
        ..empty_config()
    };
    assert_eq!(
        typed.validate().unwrap_err(),
        McpServerConfigValidationError::SystemMcpToolsRequiresSystemMcp
    );
    let missing = McpServerConfig {
        system_mcp_tools: Some(vec!["Read".to_string()]),
        ..empty_config()
    };
    assert_eq!(
        missing.validate().unwrap_err(),
        McpServerConfigValidationError::SystemMcpToolsRequiresSystemMcp
    );

    for tools in ["[]", r#"["Read","Write"]"#] {
        let json = format!(r#"{{"command":"npx","system_mcp":true,"system_mcp_tools":{tools}}}"#);
        let cfg = parse(&json).expect("true + 工具数组必须合法");
        assert_eq!(cfg.system_mcp, Some(true));
        assert!(cfg.validate().is_ok());
    }
}

/// 契约 4 配置语义：`true + []` 与 `true + 缺省 tools` 必须保持可区分并无损写回。
#[test]
fn test_system_mcp_empty_tools_roundtrip() {
    let cfg = parse(r#"{"command":"npx","system_mcp":true,"system_mcp_tools":[]}"#).unwrap();
    assert_eq!(cfg.system_mcp, Some(true));
    assert_eq!(cfg.system_mcp_tools, Some(vec![]));

    let json = serde_json::to_value(&cfg).unwrap();
    assert_eq!(json["system_mcp"], serde_json::json!(true));
    assert_eq!(
        json["system_mcp_tools"],
        serde_json::json!([]),
        "Some([]) 不得被省略: {json}"
    );
    let back: McpServerConfig = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(back.system_mcp_tools, Some(vec![]));
    assert_eq!(serde_json::to_value(&back).unwrap(), json, "往返必须无损");

    let no_tools = parse(r#"{"command":"npx","system_mcp":true}"#).unwrap();
    assert!(
        no_tools.system_mcp_tools.is_none(),
        "未声明 tools 保持 None，不自动补空数组"
    );
    let no_tools_json = serde_json::to_value(&no_tools).unwrap();
    assert!(
        no_tools_json.get("system_mcp_tools").is_none(),
        "None 不得序列化: {no_tools_json}"
    );
}

/// snake_case 为 canonical；camelCase 别名输入等价；两种拼法同时出现报 duplicate field。
#[test]
fn test_system_mcp_key_aliases() {
    let snake = parse(
        r#"{"command":"npx","system_mcp":true,"system_mcp_tools":["Read"],"system_mcp_timeout":1500}"#,
    )
    .unwrap();
    let camel = parse(
        r#"{"command":"npx","systemMcp":true,"systemMcpTools":["Read"],"systemMcpTimeout":1500}"#,
    )
    .unwrap();
    assert_eq!(camel.system_mcp, snake.system_mcp);
    assert_eq!(camel.system_mcp_tools, snake.system_mcp_tools);
    assert_eq!(camel.system_mcp_timeout, snake.system_mcp_timeout);

    let json = serde_json::to_value(&camel).unwrap();
    assert_eq!(json["system_mcp"], serde_json::json!(true));
    assert_eq!(json["system_mcp_tools"], serde_json::json!(["Read"]));
    assert_eq!(json["system_mcp_timeout"], serde_json::json!(1500));
    let text = serde_json::to_string(&camel).unwrap();
    assert!(
        !text.contains("systemMcp"),
        "canonical 输出只允许 snake_case: {text}"
    );

    for json in [
        r#"{"system_mcp":true,"systemMcp":true}"#,
        r#"{"system_mcp":true,"system_mcp_tools":[],"systemMcpTools":[]}"#,
        r#"{"system_mcp":true,"system_mcp_timeout":1000,"systemMcpTimeout":1000}"#,
    ] {
        let err = parse(json).expect_err("两种拼法同时出现必须失败");
        assert!(
            err.to_string().contains("duplicate field"),
            "必须报 duplicate field: {json} -> {err}"
        );
    }
}

/// 新增 key 的显式 null 与错误类型一律拒绝，不得降级为 None / 空数组。
#[test]
fn test_system_mcp_rejects_null_and_wrong_types() {
    for json in [
        r#"{"command":"npx","system_mcp":null}"#,
        r#"{"command":"npx","system_mcp":"true"}"#,
        r#"{"command":"npx","system_mcp":1}"#,
        r#"{"command":"npx","system_mcp_tools":null}"#,
        r#"{"command":"npx","system_mcp":true,"system_mcp_tools":"Read"}"#,
        r#"{"command":"npx","system_mcp":true,"system_mcp_tools":[1]}"#,
        r#"{"command":"npx","system_mcp":true,"system_mcp_tools":[null]}"#,
        r#"{"command":"npx","system_mcp_timeout":null}"#,
        r#"{"command":"npx","system_mcp":true,"system_mcp_timeout":"30000"}"#,
        r#"{"command":"npx","system_mcp":true,"system_mcp_timeout":-1}"#,
    ] {
        assert!(parse(json).is_err(), "必须拒绝: {json}");
    }

    let err = parse(r#"{"system_mcp":true,"system_mcp_tools":null}"#).unwrap_err();
    assert!(
        err.to_string().contains("invalid type: null"),
        "显式 null 不得被当成未配置: {err}"
    );

    let ok = parse(
        r#"{"command":"npx","system_mcp":true,"system_mcp_tools":[],"system_mcp_timeout":30000}"#,
    )
    .expect("合法组合不得被误拒");
    assert!(ok.validate().is_ok());
}

/// 工具名数组逐项保真：不 trim / 不排序 / 不去重 / 不展开 `${...}` / 不加前缀。
#[test]
fn test_system_mcp_tools_preserve_exact_values() {
    let json = r#"{"command":"npx","system_mcp":true,"system_mcp_tools":["Read","read","READ","Read","","${VAR}","  spaced  ","mcp__other__Tool"]}"#;
    let cfg = parse(json).unwrap();
    let expected = serde_json::json!([
        "Read",
        "read",
        "READ",
        "Read",
        "",
        "${VAR}",
        "  spaced  ",
        "mcp__other__Tool"
    ]);
    assert_eq!(
        cfg.system_mcp_tools,
        Some(
            expected
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        )
    );
    let back = serde_json::to_value(&cfg).unwrap();
    assert_eq!(back["system_mcp_tools"], expected, "值与顺序必须原样写回");
}

/// `system_mcp_timeout` 只与 `system_mcp = true` 配合，缺省为常量 30_000ms。
#[test]
fn test_system_mcp_timeout_requires_true() {
    for json in [
        r#"{"command":"npx","system_mcp_timeout":1000}"#,
        r#"{"command":"npx","system_mcp":false,"system_mcp_timeout":1000}"#,
        r#"{"command":"npx","systemMcpTimeout":1000}"#,
    ] {
        let err = parse(json).expect_err("无 system_mcp = true 时 timeout 必须非法");
        assert!(
            err.to_string()
                .contains("system_mcp_timeout requires system_mcp = true"),
            "固定规则正文必须保留: {json} -> {err}"
        );
    }

    let typed = McpServerConfig {
        system_mcp: Some(false),
        system_mcp_timeout: Some(1000),
        ..empty_config()
    };
    assert_eq!(
        typed.validate().unwrap_err(),
        McpServerConfigValidationError::SystemMcpTimeoutRequiresSystemMcp
    );
    assert_eq!(McpServerConfig::DEFAULT_SYSTEM_MCP_TIMEOUT_MS, 30_000);
    assert_eq!(McpServerConfig::MIN_SYSTEM_MCP_TIMEOUT_MS, 1);
    assert_eq!(McpServerConfig::MAX_SYSTEM_MCP_TIMEOUT_MS, 600_000);
}

/// `system_mcp_timeout` 区间 1..=600_000 毫秒；区间内往返保真，缺省不写回。
#[test]
fn test_system_mcp_timeout_range_and_roundtrip() {
    for ms in [1u64, 30_000, 600_000] {
        let json = format!(r#"{{"system_mcp":true,"system_mcp_timeout":{ms}}}"#);
        let cfg = parse(&json).unwrap();
        assert_eq!(cfg.system_mcp_timeout, Some(ms));
        assert_eq!(
            serde_json::to_value(&cfg).unwrap()["system_mcp_timeout"],
            serde_json::json!(ms)
        );
    }

    for ms in [0u64, 600_001] {
        let json = format!(r#"{{"system_mcp":true,"system_mcp_timeout":{ms}}}"#);
        let err = parse(&json).expect_err("越界 timeout 必须解析失败");
        assert!(
            err.to_string()
                .contains("system_mcp_timeout must be within 1..=600000 milliseconds"),
            "固定规则正文必须保留: {err}"
        );
        let typed = McpServerConfig {
            system_mcp: Some(true),
            system_mcp_timeout: Some(ms),
            ..empty_config()
        };
        assert_eq!(
            typed.validate().unwrap_err(),
            McpServerConfigValidationError::SystemMcpTimeoutOutOfRange
        );
    }

    let none = parse(r#"{"system_mcp":true}"#).unwrap();
    assert!(none.system_mcp_timeout.is_none());
    assert_eq!(
        none.system_mcp_timeout
            .unwrap_or(McpServerConfig::DEFAULT_SYSTEM_MCP_TIMEOUT_MS),
        30_000,
        "缺省有效值为 30_000ms"
    );
    assert!(serde_json::to_value(&none)
        .unwrap()
        .get("system_mcp_timeout")
        .is_none());
}

/// 缺省 eager：`dynamic` 缺失或显式 `false` 都不写回，既有初始化策略不变。
#[test]
fn test_dynamic_legacy_defaults_to_eager() {
    for json in [
        r#"{"command":"npx"}"#,
        r#"{"command":"npx","dynamic":false}"#,
    ] {
        let cfg = parse(json).unwrap();
        assert!(!cfg.dynamic, "缺失或 false 必须落到 eager: {json}");
        assert_eq!(cfg.validate(), Ok(()));
        let value = serde_json::to_value(&cfg).unwrap();
        assert!(value.get("dynamic").is_none(), "false 不得写回: {value}");
    }
}

/// `dynamic = true` 无损往返，且不改变其他字段。
#[test]
fn test_dynamic_true_roundtrip() {
    let cfg = parse(r#"{"url":"https://example.test/mcp","dynamic":true}"#).unwrap();
    assert!(cfg.dynamic);
    assert_eq!(cfg.url.as_deref(), Some("https://example.test/mcp"));
    let value = serde_json::to_value(&cfg).unwrap();
    assert_eq!(value["dynamic"], serde_json::json!(true));
    let back: McpServerConfig = serde_json::from_value(value).unwrap();
    assert!(back.dynamic);
}

/// `dynamic` 严格解析：显式 `null` / 非布尔一律拒绝，不得降级为 eager。
#[test]
fn test_dynamic_rejects_null_and_non_boolean() {
    for json in [
        r#"{"command":"npx","dynamic":null}"#,
        r#"{"command":"npx","dynamic":"true"}"#,
        r#"{"command":"npx","dynamic":1}"#,
        r#"{"command":"npx","dynamic":[]}"#,
        r#"{"command":"npx","dynamic":{}}"#,
    ] {
        assert!(parse(json).is_err(), "必须拒绝非布尔 dynamic: {json}");
    }

    let err = parse(r#"{"command":"npx","dynamic":null}"#).unwrap_err();
    assert!(
        err.to_string().contains("invalid type: null"),
        "显式 null 不得被当成缺失: {err}"
    );
}

/// `dynamic` 与 `system_mcp = true` 互斥；`dynamic + disabled` 合法且不连接。
#[test]
fn test_dynamic_conflicts_with_system_mcp() {
    // 冲突诊断需要来源名，解析期只做 wire 级严格校验，准入由 validate 拒绝。
    let cfg = parse(r#"{"command":"npx","dynamic":true,"system_mcp":true}"#).unwrap();
    assert_eq!(
        cfg.validate().unwrap_err(),
        McpServerConfigValidationError::DynamicWithSystemMcp
    );
    assert_eq!(
        McpServerConfigValidationError::DynamicWithSystemMcp.to_string(),
        "dynamic = true cannot be combined with system_mcp = true"
    );

    let typed = McpServerConfig {
        dynamic: true,
        system_mcp: Some(true),
        ..empty_config()
    };
    assert_eq!(
        typed.validate().unwrap_err(),
        McpServerConfigValidationError::DynamicWithSystemMcp
    );

    // 既有优先级不变：同时触发两种冲突时先报 disabled 组合。
    let both = McpServerConfig {
        dynamic: true,
        disabled: Some(true),
        system_mcp: Some(true),
        ..empty_config()
    };
    assert_eq!(
        both.validate().unwrap_err(),
        McpServerConfigValidationError::DisabledWithSystemMcp
    );

    let lazy_disabled = McpServerConfig {
        dynamic: true,
        disabled: Some(true),
        ..empty_config()
    };
    assert_eq!(lazy_disabled.validate(), Ok(()), "dynamic + disabled 合法");
}
