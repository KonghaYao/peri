//! System MCP 必需工具的解析、批量验证与 direct 提升（冻结接口 IF-M2）。
//!
//! 本模块是纯函数 seam：不读配置、不访问网络、不等待、不读连接状态，只消费
//! 调用方（B 的启动闸门）已经证明 transport / initialize / 能力协商 /
//! `tools/list` 成功的静态 bridge 快照。因此这里的任何结果都不构成「协议完成」
//! 或「ready」证据——ready 的判定与发布归 B。
//!
//! 匹配口径：必需工具按**所属 server 的原始工具名**精确匹配（不折叠大小写、
//! 不剥离 effective name 前缀、不跨 server 搜索）；对模型暴露的名字仍由 bridge
//! 现有的 `mcp__{server}__{tool}` 命名规则给出。

use std::collections::{BTreeMap, BTreeSet};

use peri_agent::tools::BaseTool;
use serde_json::{Map, Value};
use thiserror::Error;

use super::tool_bridge::McpToolBridge;

/// System MCP 必需工具的解析错误。
///
/// 变体只描述失败事实：不含 schema 内容、默认值或任何工具 payload。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum SystemToolError {
    #[error("system MCP server \"{server}\" 未提供必需工具 \"{tool}\"")]
    MissingTool { server: String, tool: String },
    #[error("system MCP server \"{server}\" 的必需工具 \"{tool}\" 存在多个同名注册")]
    AmbiguousTool {
        server: String,
        tool: String,
        /// 命中的全部注册的 effective name，顺序与输入一致。原始名相同时
        /// 各项文本相同，**长度**即冲突的注册数，不能据此任意取第一项。
        matches: Vec<String>,
    },
    #[error("system MCP server \"{server}\" 的工具 \"{tool}\" input schema 结构非法: {reason}")]
    InvalidSchema {
        server: String,
        tool: String,
        /// 只含字段路径与固定规则文本，不含 schema 值。
        reason: String,
    },
    #[error("system MCP server \"{server}\" 的必需工具 \"{tool}\" 对模型不可见")]
    NotModelVisible { server: String, tool: String },
    #[error("system 必需工具的 effective name \"{effective_name}\" 与其他工具冲突")]
    EffectiveNameCollision { effective_name: String },
}

/// 把同一次 typed 构建得到的静态 bridge 集合中，所有必需工具提升为 direct。
///
/// 语义：
/// - **all-or-nothing**：先验证全部必需项，再统一调用 `with_direct`；任何错误
///   只返回 `Err`，不存在部分成功的集合，也不产生共享副作用。
/// - 输入与输出的**长度、顺序、身份一致**，只有命中项 `is_direct()` 变为 true。
/// - `required` 的 value 允许为空数组：该 server 不做必需工具检查，也不提升任何
///   工具（它的普通工具保持 deferred，不是「删除该 MCP 的工具」）。
/// - 未被要求的 bridge 不参与 schema 校验，沿用既有 deferred 冲突策略。
/// - 重复配置项幂等，不产生第二次注册。
///
/// 调用方必须先保证 `required` key 对应的 server 已经 ready；本函数无法从零匹配
/// 集合证明 server 存在。
#[allow(dead_code)] // 生产接线归 B-03（W4）；此处先落地接口并有 crate 内测试覆盖
pub(crate) fn prepare_system_tools(
    bridges: Vec<McpToolBridge>,
    required: &BTreeMap<String, Vec<String>>,
) -> Result<Vec<McpToolBridge>, SystemToolError> {
    // 阶段 1：逐项解析与验证。此阶段不修改任何 bridge，因此失败时无可发布的半成品。
    let mut selected: BTreeSet<usize> = BTreeSet::new();
    for (server, tools) in required {
        for tool in unique_tools(tools) {
            let index = resolve_required_bridge(&bridges, server, tool)?;
            let bridge = &bridges[index];
            if !bridge.visible_to_model() {
                return Err(SystemToolError::NotModelVisible {
                    server: server.clone(),
                    tool: tool.to_string(),
                });
            }
            validate_input_schema(&bridge.parameters()).map_err(|reason| {
                SystemToolError::InvalidSchema {
                    server: server.clone(),
                    tool: tool.to_string(),
                    reason,
                }
            })?;
            selected.insert(index);
        }
    }

    // 阶段 2：必需工具的 effective name 必须在整批静态 bridge 内唯一。
    validate_effective_names(&bridges, &selected)?;

    // 阶段 3：统一提升。整体替换原集合，不 append 第二份注册。
    Ok(bridges
        .into_iter()
        .enumerate()
        .map(|(index, bridge)| {
            if selected.contains(&index) {
                bridge.with_direct()
            } else {
                bridge
            }
        })
        .collect())
}

/// 配置数组按出现顺序去重：重复项幂等，且不重排 `required` 的语义顺序。
fn unique_tools(tools: &[String]) -> Vec<&str> {
    let mut unique: Vec<&str> = Vec::with_capacity(tools.len());
    for tool in tools {
        if !unique.contains(&tool.as_str()) {
            unique.push(tool.as_str());
        }
    }
    unique
}

/// 在所属 server 的原始工具名上精确匹配：0 个命中 → `MissingTool`，
/// 多于 1 个 → `AmbiguousTool`。
fn resolve_required_bridge(
    bridges: &[McpToolBridge],
    server: &str,
    tool: &str,
) -> Result<usize, SystemToolError> {
    let matches: Vec<usize> = bridges
        .iter()
        .enumerate()
        .filter(|(_, bridge)| {
            bridge.mcp_server_name() == Some(server) && bridge.original_tool_name() == tool
        })
        .map(|(index, _)| index)
        .collect();
    match matches.as_slice() {
        [] => Err(SystemToolError::MissingTool {
            server: server.to_string(),
            tool: tool.to_string(),
        }),
        [index] => Ok(*index),
        indices => Err(SystemToolError::AmbiguousTool {
            server: server.to_string(),
            tool: tool.to_string(),
            matches: indices
                .iter()
                .map(|index| bridges[*index].name().to_string())
                .collect(),
        }),
    }
}

/// 必需工具的 effective name 必须在整批静态 bridge 内唯一，且 ASCII 大小写折叠后
/// 也不得与其他工具同名（净化碰撞与执行期大小写歧义都 fail closed）。
///
/// 只检查必需项：与必需项无关的普通 deferred 工具沿用既有冲突策略。
fn validate_effective_names(
    bridges: &[McpToolBridge],
    selected: &BTreeSet<usize>,
) -> Result<(), SystemToolError> {
    for index in selected {
        let name = bridges[*index].name();
        let folded = name.to_ascii_lowercase();
        let collides = bridges
            .iter()
            .enumerate()
            .any(|(other, bridge)| other != *index && bridge.name().to_ascii_lowercase() == folded);
        if collides {
            return Err(SystemToolError::EffectiveNameCollision {
                effective_name: name.to_string(),
            });
        }
    }
    Ok(())
}

// ─── input schema 结构解析 ──────────────────────────────────────────────────

/// `type` 允许的 JSON Schema 类型名。
const SCHEMA_TYPE_NAMES: &[&str] = &[
    "array", "boolean", "integer", "null", "number", "object", "string",
];

/// 值是单个 schema 节点的已知关键字。
const SCHEMA_NODE_KEYWORDS: &[&str] = &[
    "additionalProperties",
    "contains",
    "else",
    "if",
    "items",
    "not",
    "propertyNames",
    "then",
];

/// 值是 schema 数组的已知关键字。
const SCHEMA_LIST_KEYWORDS: &[&str] = &["allOf", "anyOf", "oneOf", "prefixItems"];

/// 值是 `名称 → schema` 映射的已知关键字。
const SCHEMA_MAP_KEYWORDS: &[&str] = &[
    "$defs",
    "definitions",
    "dependentSchemas",
    "patternProperties",
    "properties",
];

/// 值是字符串的已知关键字（引用、标识与注记）。
const STRING_KEYWORDS: &[&str] = &[
    "$anchor",
    "$comment",
    "$id",
    "$ref",
    "$schema",
    "contentEncoding",
    "contentMediaType",
    "description",
    "format",
    "pattern",
    "title",
];

/// 值是数值的已知关键字。
const NUMBER_KEYWORDS: &[&str] = &[
    "exclusiveMaximum",
    "exclusiveMinimum",
    "maximum",
    "minimum",
    "multipleOf",
];

/// 值是非负整数的已知关键字。
const NON_NEGATIVE_INTEGER_KEYWORDS: &[&str] = &[
    "maxItems",
    "maxLength",
    "maxProperties",
    "minItems",
    "minLength",
    "minProperties",
];

/// 值是布尔量的已知关键字。
const BOOLEAN_KEYWORDS: &[&str] = &["deprecated", "readOnly", "uniqueItems", "writeOnly"];

const RULE_NODE: &str = "must be an object or boolean schema node";
const RULE_ROOT_OBJECT: &str = "root must be a JSON object";
const RULE_ROOT_TYPE: &str = "root \"type\" must be \"object\"";
const RULE_MAP: &str = "must be a JSON object mapping names to schema nodes";
const RULE_LIST: &str = "must be an array of schema nodes";
const RULE_TYPE: &str =
    "\"type\" must be a JSON Schema type name or a non-empty array of unique type names";
const RULE_REQUIRED: &str = "\"required\" must be an array";
const RULE_REQUIRED_ITEM: &str = "\"required\" entries must be unique strings";
const RULE_ENUM: &str = "\"enum\" must be a non-empty array";
const RULE_STRING: &str = "must be a string";
const RULE_NUMBER: &str = "must be a number";
const RULE_INTEGER: &str = "must be a non-negative integer";
const RULE_BOOLEAN: &str = "must be a boolean";

/// MCP `inputSchema` 的结构解析检查。
///
/// 只检查根为 object、已知关键字的类型与嵌套位置合法；**不是**完整 JSON Schema
/// draft 的元 schema 校验或 instance validation，也不解析 `$ref`。annotation、
/// vendor extension 以及 `default` / `examples` / `enum` / `const` 中的普通数据不按
/// schema 递归。失败只返回「固定规则 + 字段路径」，不返回 schema 值。
fn validate_input_schema(schema: &Value) -> Result<(), String> {
    let Some(root) = schema.as_object() else {
        return Err(invalid("/", RULE_ROOT_OBJECT));
    };
    if root.get("type").is_some_and(|value| value != "object") {
        return Err(invalid("/type", RULE_ROOT_TYPE));
    }
    validate_schema_object(root, "/")
}

fn validate_schema_node(node: &Value, path: &str) -> Result<(), String> {
    match node {
        Value::Bool(_) => Ok(()),
        Value::Object(object) => validate_schema_object(object, path),
        _ => Err(invalid(path, RULE_NODE)),
    }
}

fn validate_schema_object(object: &Map<String, Value>, path: &str) -> Result<(), String> {
    for (keyword, value) in object {
        let child = child_path(path, keyword);
        if SCHEMA_NODE_KEYWORDS.contains(&keyword.as_str()) {
            validate_schema_node(value, &child)?;
        } else if SCHEMA_LIST_KEYWORDS.contains(&keyword.as_str()) {
            let Some(nodes) = value.as_array() else {
                return Err(invalid(&child, RULE_LIST));
            };
            for (index, node) in nodes.iter().enumerate() {
                validate_schema_node(node, &format!("{child}/{index}"))?;
            }
        } else if SCHEMA_MAP_KEYWORDS.contains(&keyword.as_str()) {
            let Some(nodes) = value.as_object() else {
                return Err(invalid(&child, RULE_MAP));
            };
            for (name, node) in nodes {
                validate_schema_node(node, &child_path(&child, name))?;
            }
        } else if keyword == "type" {
            validate_type_keyword(value, &child)?;
        } else if keyword == "required" {
            validate_required_keyword(value, &child)?;
        } else if keyword == "enum" {
            if !matches!(value, Value::Array(items) if !items.is_empty()) {
                return Err(invalid(&child, RULE_ENUM));
            }
        } else if STRING_KEYWORDS.contains(&keyword.as_str()) && !value.is_string() {
            return Err(invalid(&child, RULE_STRING));
        } else if NUMBER_KEYWORDS.contains(&keyword.as_str()) && !value.is_number() {
            return Err(invalid(&child, RULE_NUMBER));
        } else if NON_NEGATIVE_INTEGER_KEYWORDS.contains(&keyword.as_str())
            && value.as_u64().is_none()
        {
            return Err(invalid(&child, RULE_INTEGER));
        } else if BOOLEAN_KEYWORDS.contains(&keyword.as_str()) && !value.is_boolean() {
            return Err(invalid(&child, RULE_BOOLEAN));
        }
        // 其余关键字（annotation / vendor extension / const / default / examples）
        // 不是 schema 位置：不递归、不校验内容。
    }
    Ok(())
}

fn validate_type_keyword(value: &Value, path: &str) -> Result<(), String> {
    if let Some(name) = value.as_str() {
        return if SCHEMA_TYPE_NAMES.contains(&name) {
            Ok(())
        } else {
            Err(invalid(path, RULE_TYPE))
        };
    }
    let Some(names) = value.as_array() else {
        return Err(invalid(path, RULE_TYPE));
    };
    if names.is_empty() {
        return Err(invalid(path, RULE_TYPE));
    }
    let mut seen: Vec<&str> = Vec::with_capacity(names.len());
    for (index, name) in names.iter().enumerate() {
        let Some(name) = name
            .as_str()
            .filter(|name| SCHEMA_TYPE_NAMES.contains(name))
        else {
            return Err(invalid(&format!("{path}/{index}"), RULE_TYPE));
        };
        if seen.contains(&name) {
            return Err(invalid(&format!("{path}/{index}"), RULE_TYPE));
        }
        seen.push(name);
    }
    Ok(())
}

fn validate_required_keyword(value: &Value, path: &str) -> Result<(), String> {
    let Some(names) = value.as_array() else {
        return Err(invalid(path, RULE_REQUIRED));
    };
    let mut seen: Vec<&str> = Vec::with_capacity(names.len());
    for (index, name) in names.iter().enumerate() {
        let Some(name) = name.as_str() else {
            return Err(invalid(&format!("{path}/{index}"), RULE_REQUIRED_ITEM));
        };
        if seen.contains(&name) {
            return Err(invalid(&format!("{path}/{index}"), RULE_REQUIRED_ITEM));
        }
        seen.push(name);
    }
    Ok(())
}

/// JSON Pointer 风格路径：根为 `/`；token 内按 RFC 6901 转义 `~` 与 `/`。
fn child_path(path: &str, token: &str) -> String {
    let token = token.replace('~', "~0").replace('/', "~1");
    if path == "/" {
        format!("/{token}")
    } else {
        format!("{path}/{token}")
    }
}

fn invalid(path: &str, rule: &'static str) -> String {
    format!("{rule} at {path}")
}

#[cfg(test)]
#[path = "system_tools_test.rs"]
mod tests;
