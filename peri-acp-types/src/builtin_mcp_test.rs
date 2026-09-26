//! `builtin_mcp.rs`（builtin 注册表纯数据）的契约测试。
//!
//! 边界：本文件只断言**数据形态**（唯一性 / 非空 / 名字合法 / 保留名覆盖 /
//! policy_key 唯一 / 冻结字面量与直连声明 / 归一查表命中与未命中 / 用户无法从
//! wire 构造 `ConfigSource::Builtin`）。
//! 「字面量 == `effective_tool_name()` 输出」与「`direct` 集合 == `system_mcp_tools`
//! 集合」两条**计算侧**一致性断言归 `cargo test -p peri-middlewares --lib --
//! mcp::builtin::tests`（规则一份实现、字面量一份声明）。

use std::collections::HashSet;

use crate::builtin_mcp::{
    find, is_reserved_instance_name, original_tool_name_of_effective, BUILTIN_MCP_INSTANCES,
    BUILTIN_RESERVED_INSTANCE_NAMES,
};
use crate::plugin::{ConfigSource, McpServerConfig};

/// 名字合法性：与模型面工具名的字符集约束一致（`^[A-Za-z0-9_-]+$`）。
fn is_legal_component(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

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
        protocol_version: None,
        subscriptions: None,
        system_mcp: None,
        system_mcp_tools: None,
        system_mcp_timeout: None,
        source: None,
    }
}

#[test]
fn instances_are_non_empty_and_unique() {
    assert!(!BUILTIN_MCP_INSTANCES.is_empty(), "已实现实例表不得为空");
    let mut names = HashSet::new();
    let mut identities = HashSet::new();
    for instance in BUILTIN_MCP_INSTANCES {
        assert!(names.insert(instance.name), "实例名重复: {}", instance.name);
        assert!(
            identities.insert(instance.instance),
            "实例身份重复: {}",
            instance.instance
        );
        assert!(
            !instance.tools.is_empty(),
            "实例 {} 的工具表不得为空",
            instance.name
        );
    }
}

#[test]
fn tools_are_non_empty_and_unique_per_instance() {
    for instance in BUILTIN_MCP_INSTANCES {
        let mut originals = HashSet::new();
        let mut effectives = HashSet::new();
        for tool in instance.tools {
            assert!(
                originals.insert(tool.original_name),
                "实例 {} 的原始工具名重复: {}",
                instance.name,
                tool.original_name
            );
            assert!(
                effectives.insert(tool.effective_name),
                "实例 {} 的 effective name 重复: {}",
                instance.name,
                tool.effective_name
            );
            assert!(
                tool.prompt_declaration.is_some(),
                "实例 {} 的工具 {} 必须携带声明模板（A9：声明段不得丢失）",
                instance.name,
                tool.original_name
            );
            assert!(
                tool.prompt_declaration
                    .is_some_and(|template| template.contains("{{name}}")),
                "实例 {} 的工具 {} 的声明模板必须含 {{{{name}}}} 占位符（渲染后为 effective name）",
                instance.name,
                tool.original_name
            );
        }
    }
}

#[test]
fn names_and_identities_are_legal_components() {
    for instance in BUILTIN_MCP_INSTANCES {
        assert!(
            is_legal_component(instance.name),
            "非法实例名: {}",
            instance.name
        );
        assert!(
            is_legal_component(instance.instance),
            "非法实例身份: {}",
            instance.instance
        );
        assert!(
            is_legal_component(instance.policy_key),
            "非法关闭键: {}",
            instance.policy_key
        );
        for tool in instance.tools {
            assert!(
                is_legal_component(tool.original_name),
                "非法原始工具名: {}",
                tool.original_name
            );
            assert!(
                is_legal_component(tool.effective_name),
                "非法 effective name: {}",
                tool.effective_name
            );
        }
    }
}

#[test]
fn instance_identity_equals_server_name() {
    // 本批次身份三等价：配置 key == server name == TransportConfig::Builtin.instance。
    for instance in BUILTIN_MCP_INSTANCES {
        assert_eq!(
            instance.instance, instance.name,
            "实例身份与 server name 必须一致（当前两者同值）"
        );
    }
}

#[test]
fn reserved_names_superset_of_implemented_instances() {
    for instance in BUILTIN_MCP_INSTANCES {
        assert!(
            is_reserved_instance_name(instance.name),
            "已实现实例 {} 必须在保留名表内",
            instance.name
        );
    }
    // 预留未实现名（后续波次）：本批次只登记，不注入、不解析。
    for reserved in ["web", "artifact", "cron", "lsp", "workspace"] {
        assert!(
            BUILTIN_RESERVED_INSTANCE_NAMES.contains(&reserved),
            "保留名表缺失: {reserved}"
        );
        assert!(is_reserved_instance_name(reserved));
    }
    assert!(!is_reserved_instance_name("some-user-server"));
}

#[test]
fn policy_keys_are_unique_and_frozen() {
    let keys: HashSet<&str> = BUILTIN_MCP_INSTANCES
        .iter()
        .map(|instance| instance.policy_key)
        .collect();
    assert_eq!(
        keys.len(),
        BUILTIN_MCP_INSTANCES.len(),
        "关闭键必须逐实例唯一"
    );
    let expected: HashSet<&str> = ["WebMiddleware", "ArtifactMiddleware"]
        .into_iter()
        .collect();
    assert_eq!(
        keys, expected,
        "关闭键集合必须与 BUILTIN_INSTANCE_POLICY_KEYS 的冻结内容相等"
    );
}

#[test]
fn find_hits_only_implemented_instances() {
    let web = find("web").expect("web 是已实现实例");
    assert_eq!(web.name, "web");
    assert_eq!(web.tools.len(), 2);
    let artifact = find("artifact").expect("artifact 是已实现实例");
    assert_eq!(artifact.name, "artifact");
    assert_eq!(artifact.tools.len(), 1);

    // 预留但未实现：不得被解析（否则会构造出无 runtime 的 transport）。
    for reserved in ["cron", "lsp", "workspace"] {
        assert!(find(reserved).is_none(), "{reserved} 本批次未实现");
    }
    assert!(find("").is_none());
    assert!(find("Web").is_none(), "实例名匹配区分大小写");
    assert!(find("mcp__web__WebSearch").is_none());
}

#[test]
fn original_tool_name_of_effective_hits_frozen_literals() {
    assert_eq!(
        original_tool_name_of_effective("mcp__web__WebSearch"),
        Some("WebSearch")
    );
    assert_eq!(
        original_tool_name_of_effective("mcp__web__WebFetch"),
        Some("WebFetch")
    );
    assert_eq!(
        original_tool_name_of_effective("mcp__artifact__artifact"),
        Some("artifact")
    );
}

#[test]
fn original_tool_name_of_effective_misses_unknown_and_external() {
    // 未命中 ⇒ 消费点沿用既有保守语义（`mcp__*` 一律敏感；不得反拆名字）。
    for unknown in [
        "",
        "WebSearch",
        "artifact",
        "mcp__some_tool",
        "mcp__web__Unknown",
        "mcp__web__websearch",
        "mcp__web__WebSearch::extra",
    ] {
        assert_eq!(
            original_tool_name_of_effective(unknown),
            None,
            "{unknown} 不得被归一（纯查表 + 区分大小写）"
        );
    }
}

#[test]
fn wave1_tools_are_all_declared_direct() {
    // wave 1 的三个工具迁移前均 `is_direct() == true`（web_fetch / web_search /
    // artifact），声明表必须逐位保持；后续波次的 Cron / LSP 工具才预留 false。
    for instance in BUILTIN_MCP_INSTANCES {
        for tool in instance.tools {
            assert!(
                tool.direct,
                "实例 {} 的工具 {} 在 wave 1 必须声明 direct",
                instance.name, tool.original_name
            );
        }
    }
}

#[test]
fn builtin_source_cannot_be_forged_from_wire() {
    // 身份由 `source`（`#[serde(skip)]`）承载：用户配置既不能伪造，也不会写进 wire。
    let forged: McpServerConfig = serde_json::from_str(
        r#"{"command":"npx","source":{"Builtin":{"instance":"web"}},"sourceFile":"/tmp/x"}"#,
    )
    .expect("未知 key 不得使解析失败（沿用既有宽容语义）");
    assert!(
        forged.source.is_none(),
        "用户配置不得构造 ConfigSource::Builtin: {:?}",
        forged.source
    );

    let mut config = empty_config();
    config.command = Some("npx".to_string());
    config.source = Some(ConfigSource::Builtin {
        instance: "web".to_string(),
    });
    let json = serde_json::to_value(&config).expect("序列化不得失败");
    assert!(
        json.get("source").is_none(),
        "source 是运行时标记，不得进入 wire: {json}"
    );
    assert_eq!(json["command"], serde_json::json!("npx"));
}
