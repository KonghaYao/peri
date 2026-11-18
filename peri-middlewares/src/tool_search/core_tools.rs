//! Core Tools 白名单定义与延迟加载判定逻辑

// ─── 共享常量 ────────────────────────────────────────────────────────────────

pub use peri_acp_types::tool_proxy::{
    EXECUTE_EXTRA_TOOL_NAME, LEGACY_PROXY_ARGUMENTS_FIELD, LEGACY_PROXY_NAME_FIELD,
    PROXY_ARGUMENTS_FIELD, PROXY_NAME_FIELD, SEARCH_EXTRA_TOOLS_NAME, TOOL_INVOKE_NAME,
    TOOL_SEARCH_NAME,
};
use serde_json::Value;

/// 迁移期调用代理字段：目标工具名（值固定为 `tool_name`）。
pub const EXTRA_TOOL_NAME_FIELD: &str = LEGACY_PROXY_NAME_FIELD;
/// 迁移期调用代理字段：目标工具参数（值固定为 `params`）。
pub const EXTRA_TOOL_PARAMS_FIELD: &str = LEGACY_PROXY_ARGUMENTS_FIELD;

// ─── Core tool name constants ──────────────────────────────────────────────

pub const TOOL_BASH: &str = "Bash";
pub const TOOL_WRITE: &str = "Write";
pub const TOOL_EDIT: &str = "Edit";
pub const TOOL_READ: &str = "Read";
pub const TOOL_GLOB: &str = "Glob";
pub const TOOL_GREP: &str = "Grep";
pub const TOOL_FOLDER_OPS: &str = "folder_operations";
pub const TOOL_AGENT: &str = "Agent";
pub const TOOL_WEBFETCH: &str = "WebFetch";
pub const TOOL_WEBSEARCH: &str = "WebSearch";
pub const TOOL_ASK_USER: &str = "AskUserQuestion";
pub const TOOL_TODO: &str = "TodoWrite";
pub const TOOL_SKILL: &str = "SkillTool";
pub const TOOL_DISCOVER_SKILLS: &str = "DiscoverSkillsTool";

/// 归一化后的代理调用（新字段与旧字段共用同一份事实）。
#[derive(Debug, Clone, PartialEq)]
pub struct ProxyCall {
    pub target_name: String,
    pub arguments: Value,
}

/// 统一解析 ToolInvoke 与 ExecuteExtraTool 的外层参数。
///
/// 新旧字段先归一化再校验：字段同时出现且值不同即拒绝，未知字段忽略。
pub fn parse_proxy_call(input: &Value) -> Result<ProxyCall, String> {
    Ok(ProxyCall {
        target_name: parse_proxy_name(input)?,
        arguments: parse_proxy_arguments(input)?,
    })
}

fn non_empty_str(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
}

fn parse_proxy_name(input: &Value) -> Result<String, String> {
    let canonical = non_empty_str(input.get(PROXY_NAME_FIELD));
    let legacy = non_empty_str(input.get(LEGACY_PROXY_NAME_FIELD));
    match (canonical, legacy) {
        (Some(canonical), Some(legacy)) if canonical != legacy => {
            Err("conflicting ToolInvoke name fields".to_string())
        }
        (Some(name), _) | (_, Some(name)) => Ok(name.to_string()),
        (None, None) => {
            Err("malformed ToolInvoke invocation: missing non-empty 'name'".to_string())
        }
    }
}

fn parse_proxy_arguments(input: &Value) -> Result<Value, String> {
    let canonical = input.get(PROXY_ARGUMENTS_FIELD);
    let legacy = input.get(LEGACY_PROXY_ARGUMENTS_FIELD);
    let canonical_args = canonical.and_then(Value::as_object);
    let legacy_args = legacy.and_then(Value::as_object);
    if (canonical.is_some() && canonical_args.is_none())
        || (legacy.is_some() && legacy_args.is_none())
    {
        return Err("malformed ToolInvoke invocation: arguments must be an object".to_string());
    }
    match (canonical_args, legacy_args) {
        (Some(canonical), Some(legacy)) if canonical != legacy => {
            Err("conflicting ToolInvoke arguments fields".to_string())
        }
        (Some(arguments), _) => Ok(Value::Object(arguments.clone())),
        (_, Some(arguments)) => Ok(Value::Object(arguments.clone())),
        (None, None) => {
            Err("malformed ToolInvoke invocation: missing 'arguments' object".to_string())
        }
    }
}

/// ExecuteExtraTool 的兼容入口：唯一转发到 [`parse_proxy_call`]。
pub fn parse_extra_tool_call(input: &Value) -> Result<(String, Value), String> {
    parse_proxy_call(input).map(|call| (call.target_name, call.arguments))
}

/// 解析有效的工具名称
///
/// 当 tool_name 为调用代理（[`TOOL_INVOKE_NAME`] 或旧名 [`EXECUTE_EXTRA_TOOL_NAME`]）时，
/// 从统一解析器提取目标工具名，用于 HITL 权限判断。解析失败（字段冲突/畸形）时返回
/// 外层名，由权限层按 fail-closed 处理，避免畸形代理调用拿到目标工具的免审批资格。
#[must_use]
pub fn resolve_effective_tool_name(tool_name: &str, input: &Value) -> String {
    if peri_acp_types::tool_proxy::is_invoke_proxy_name(tool_name) {
        parse_proxy_call(input)
            .map(|call| call.target_name)
            .unwrap_or_else(|_| tool_name.to_string())
    } else {
        tool_name.to_string()
    }
}

/// 返回工具名按字典序排序后的逗号分隔字符串（含空格）。
///
/// 输入必须来自当前 session 的实际 direct tool 集合。
#[must_use]
pub fn direct_tools_sorted_csv<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    let mut names: Vec<&str> = names.into_iter().collect();
    names.sort_unstable();
    names.dedup();
    names.join(", ")
}

/// 将当前 session 的 direct tool 集合格式化为稳定的能力说明。
#[must_use]
pub fn direct_tools_description<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    let names = direct_tools_sorted_csv(names);
    if names.is_empty() {
        "No other tools are directly available in this session.".to_string()
    } else {
        format!("Tools directly available in this session: {names}.")
    }
}

#[cfg(test)]
#[path = "core_tools_test.rs"]
mod tests;
