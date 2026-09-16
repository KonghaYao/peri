//! `mcp::catalog` 的单元测试（WP-005）。
//!
//! 测试目标不是"代码能跑"，而是**投影没有偷偷改写源契约**：夹具是唯一事实源，
//! MCP 条目必须与夹具逐字一致，且不得凭空多出 title/annotations/outputSchema 这类
//! 源实现里不存在的元数据。

use serde_json::Value;

use super::{catalog, metadata, metadata_for, tools};
use crate::wire::{resolve_tool_name, TOOL_ALIASES, TOOL_NAMES};

const READ_RAW: &str = include_str!("../../tests/fixtures/schemas/read.json");
const BASH_RAW: &str = include_str!("../../tests/fixtures/schemas/bash.json");

fn fixture(raw: &str) -> Value {
    serde_json::from_str(raw).expect("夹具必须是合法 JSON")
}

#[test]
fn test_catalog_exposes_exactly_seven_tools_in_frozen_order() {
    let names: Vec<&str> = tools().iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(
        names,
        TOOL_NAMES.to_vec(),
        "tools/list 顺序必须等于冻结注册面"
    );
    assert_eq!(metadata().len(), TOOL_NAMES.len());
}

#[test]
fn test_catalog_description_and_schema_match_fixture_verbatim() {
    for (index, raw) in super::FIXTURES.iter().map(|(_, raw)| *raw).enumerate() {
        let expected = fixture(raw);
        let tool = &tools()[index];
        let expected_name = expected["tool"].as_str().unwrap_or_default();
        assert_eq!(tool.name.as_ref(), expected_name);
        assert_eq!(
            tool.description.as_deref(),
            expected["description"].as_str(),
            "{expected_name} 的 description 必须与夹具逐字一致"
        );
        assert_eq!(
            Value::Object((*tool.input_schema).clone()),
            expected["inputSchema"],
            "{expected_name} 的 inputSchema 必须与夹具逐字一致"
        );
    }
}

#[test]
fn test_catalog_aliases_match_the_frozen_alias_table() {
    for entry in metadata() {
        let expected: Vec<&str> = TOOL_ALIASES
            .iter()
            .filter(|(_, canonical)| *canonical == entry.name)
            .map(|(alias, _)| *alias)
            .collect();
        assert_eq!(
            entry.aliases, expected,
            "{} 的别名必须与 wire::TOOL_ALIASES 一致",
            entry.name
        );
        for alias in &entry.aliases {
            assert!(
                !TOOL_NAMES.contains(&alias.as_str()),
                "别名不得成为工具条目"
            );
            assert_eq!(resolve_tool_name(alias), Some(entry.name));
        }
    }
}

#[test]
fn test_catalog_preserves_namespace_and_host_projection_facts() {
    let read = metadata_for("Read").expect("Read 必须存在");
    let bash = metadata_for("Bash").expect("Bash 必须存在");
    assert_eq!(read.namespace, "filesystem");
    assert_eq!(bash.namespace, "execution");
    assert_eq!(
        read.host_projection["prefers_persist"].as_bool(),
        Some(true),
        "Read 的 prefers_persist 事实不得在投影中丢失"
    );
    assert_eq!(
        bash.host_projection["output_char_limit_chars"].as_u64(),
        Some(10_000),
        "Bash 的宿主外层 10000 字符投影不得在投影中丢失"
    );
    assert!(
        !bash.host_projection["note"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "宿主投影必须保留说明文本，避免被误读为工具内部截断"
    );
}

#[test]
fn test_catalog_does_not_invent_tool_metadata() {
    for tool in tools() {
        assert!(tool.title.is_none(), "源实现没有 title，不得发明");
        assert!(tool.output_schema.is_none(), "源实现没有 outputSchema");
        assert!(tool.annotations.is_none(), "源实现没有 annotations");
        assert!(tool.icons.is_none(), "源实现没有 icons");
        assert!(tool.meta.is_none(), "不得自造工具 _meta 冒充源元数据");
    }
}

#[test]
fn test_projected_schema_stays_on_default_2020_12_baseline() {
    for tool in tools() {
        let schema = Value::Object((*tool.input_schema).clone());
        assert_eq!(schema["type"].as_str(), Some("object"));
        assert!(schema.get("$schema").is_none(), "省略即默认 2020-12");
        for keyword in ["oneOf", "anyOf", "allOf", "$ref", "$defs"] {
            assert!(schema.get(keyword).is_none(), "投影不得引入 {keyword}");
        }
        let properties = schema["properties"].as_object().expect("properties");
        assert!(!properties.is_empty());
        for (name, entry) in properties {
            assert!(
                entry.get("type").is_some(),
                "{}.{name} 缺少 type",
                tool.name
            );
            assert!(
                !entry["description"].as_str().unwrap_or_default().is_empty(),
                "{}.{name} 缺少 description",
                tool.name
            );
        }
    }
}

#[test]
fn test_catalog_is_parsed_once_and_shared() {
    let first = catalog() as *const _;
    let second = catalog() as *const _;
    assert_eq!(first, second, "目录必须是进程内唯一实例");
    assert_eq!(tools().len(), 7);
    assert_eq!(fixture(BASH_RAW)["tool"].as_str(), Some("Bash"));
    assert_eq!(fixture(READ_RAW)["tool"].as_str(), Some("Read"));
}
