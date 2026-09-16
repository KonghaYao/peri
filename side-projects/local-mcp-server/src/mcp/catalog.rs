//! 七工具目录：把冻结夹具投影成 MCP `Tool`（WP-005）。
//!
//! 事实源是 `tests/fixtures/schemas/**`——WP-001 从**只读** Peri 源码逐字提取的黄金
//! 夹具（见 `artifacts/designs/WP-001/interfaces.md` §6）。本模块**不重写**任何
//! `description` / `inputSchema` 文本：夹具经 `include_str!` 在编译期进入本单元，
//! 因此"`tools/list` 与夹具精确一致"由构造保证，而不是靠人工同步保证。
//!
//! 冻结投影规则：
//! 1. `tools/list` 恰好 [`TOOL_NAMES`] 七项，顺序等于夹具顺序；
//! 2. `name` = 规范名，`description` = 夹具逐字，`inputSchema` = 夹具逐字；
//! 3. 别名（`reading` / `Shell`）**不是**列表条目，只在 `tools/call` 名称解析生效；
//! 4. 源事实 `namespace` 与 `host_projection`（Bash 外层 10000 字符投影、Read
//!    `prefers_persist` 等）进入 [`ToolMetadata`] 供协议层与证据核对，**不**被塞进
//!    自造的 `_meta`/annotations 冒充源元数据；
//! 5. 夹具解析失败是硬失败：夹具是构建期契约，静默降级会让 `tools/list` 悄悄偏离
//!    源实现而没有任何 wire 症状。

use std::sync::{Arc, OnceLock};

use rmcp::model::{JsonObject, Tool};
use serde_json::Value;

use crate::wire::TOOL_NAMES;

/// 夹具文件清单；顺序必须等于 [`TOOL_NAMES`]。
const FIXTURES: [(&str, &str); 7] = [
    (
        "read.json",
        include_str!("../../tests/fixtures/schemas/read.json"),
    ),
    (
        "write.json",
        include_str!("../../tests/fixtures/schemas/write.json"),
    ),
    (
        "edit.json",
        include_str!("../../tests/fixtures/schemas/edit.json"),
    ),
    (
        "glob.json",
        include_str!("../../tests/fixtures/schemas/glob.json"),
    ),
    (
        "grep.json",
        include_str!("../../tests/fixtures/schemas/grep.json"),
    ),
    (
        "folder_operations.json",
        include_str!("../../tests/fixtures/schemas/folder_operations.json"),
    ),
    (
        "bash.json",
        include_str!("../../tests/fixtures/schemas/bash.json"),
    ),
];

/// 单个工具的源事实登记（除 MCP 工具条目之外的部分）。
///
/// 这些字段来自夹具，属于"源实现有、MCP wire 不直接表达"的事实：别名用于
/// `tools/call` 名称解析，namespace 与宿主投影用于证据核对与结构化输出。
#[derive(Debug, Clone)]
pub struct ToolMetadata {
    /// 规范工具名。
    pub name: &'static str,
    /// 逐字 description（夹具原文）。
    pub description: String,
    /// 逐字 inputSchema（夹具原文，未改一个字节）。
    pub input_schema: Arc<JsonObject>,
    /// 源别名（`reading` / `Shell`），只在 `tools/call` 生效。
    pub aliases: Vec<String>,
    /// 源 namespace（`filesystem` / `execution`）。
    pub namespace: String,
    /// 宿主投影事实（外层截断、`prefers_persist` 等），必须如实呈现不得丢弃。
    pub host_projection: Value,
}

/// 解析后的目录：MCP 条目与源事实元数据一一对应。
#[derive(Debug)]
pub struct Catalog {
    tools: Vec<Tool>,
    metadata: Vec<ToolMetadata>,
}

impl Catalog {
    /// 解析全部夹具。任何一处不合契约都必须 panic：这是构建期契约，不是运行时输入。
    fn load() -> Self {
        let mut tools = Vec::with_capacity(TOOL_NAMES.len());
        let mut metadata = Vec::with_capacity(TOOL_NAMES.len());

        for (index, (file, raw)) in FIXTURES.iter().enumerate() {
            let fixture: Value = serde_json::from_str(raw)
                .unwrap_or_else(|error| panic!("冻结夹具 {file} 必须是合法 JSON：{error}"));

            let name = fixture["tool"]
                .as_str()
                .unwrap_or_else(|| panic!("冻结夹具 {file} 缺少字符串字段 `tool`"));
            let expected = TOOL_NAMES[index];
            assert_eq!(
                name, expected,
                "夹具 {file} 的工具名/顺序必须与 wire::TOOL_NAMES 一致"
            );

            let description = fixture["description"]
                .as_str()
                .unwrap_or_else(|| panic!("冻结夹具 {file} 缺少字符串字段 `description`"))
                .to_string();

            let input_schema: JsonObject = match fixture["inputSchema"].clone() {
                Value::Object(map) => map,
                other => panic!("冻结夹具 {file} 的 inputSchema 必须是 JSON 对象：{other}"),
            };

            let aliases: Vec<String> = fixture["aliases"]
                .as_array()
                .unwrap_or_else(|| panic!("冻结夹具 {file} 缺少 `aliases` 数组"))
                .iter()
                .map(|alias| {
                    alias
                        .as_str()
                        .unwrap_or_else(|| panic!("冻结夹具 {file} 的别名必须是字符串"))
                        .to_string()
                })
                .collect();

            let namespace = fixture["namespace"]
                .as_str()
                .unwrap_or_else(|| panic!("冻结夹具 {file} 缺少字符串字段 `namespace`"))
                .to_string();

            let host_projection = fixture
                .get("host_projection")
                .cloned()
                .unwrap_or_else(|| panic!("冻结夹具 {file} 缺少 `host_projection` 事实登记"));

            let input_schema = Arc::new(input_schema);
            let tool = Tool::new(name.to_string(), description.clone(), input_schema.clone());

            metadata.push(ToolMetadata {
                name: expected,
                description,
                input_schema,
                aliases,
                namespace,
                host_projection,
            });
            tools.push(tool);
        }

        Self { tools, metadata }
    }
}

/// 进程内唯一目录（解析一次，之后只读共享）。
pub fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(Catalog::load)
}

/// `tools/list` 的条目（顺序固定为 [`TOOL_NAMES`]）。
pub fn tools() -> &'static [Tool] {
    &catalog().tools
}

/// 全部源事实登记（顺序同 [`tools`]）。
pub fn metadata() -> &'static [ToolMetadata] {
    &catalog().metadata
}

/// 按规范名取源事实；未知名称返回 `None`。
pub fn metadata_for(name: &str) -> Option<&'static ToolMetadata> {
    metadata().iter().find(|entry| entry.name == name)
}

#[cfg(test)]
#[path = "catalog_test.rs"]
mod tests;
