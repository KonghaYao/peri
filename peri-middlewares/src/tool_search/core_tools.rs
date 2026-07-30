//! Core Tools 白名单定义与延迟加载判定逻辑

// ─── 共享常量 ────────────────────────────────────────────────────────────────

/// ExecuteExtraTool 元工具名称
pub const EXECUTE_EXTRA_TOOL_NAME: &str = "ExecuteExtraTool";
/// SearchExtraTools 元工具名称
pub const SEARCH_EXTRA_TOOLS_NAME: &str = "SearchExtraTools";
/// ExecuteExtraTool 输入字段名：目标工具名
pub const EXTRA_TOOL_NAME_FIELD: &str = "tool_name";
/// ExecuteExtraTool 输入字段名：目标工具参数
pub const EXTRA_TOOL_PARAMS_FIELD: &str = "params";

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

pub fn parse_extra_tool_call(
    input: &serde_json::Value,
) -> Result<(String, serde_json::Value), String> {
    let tool_name = input
        .get(EXTRA_TOOL_NAME_FIELD)
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "malformed ExecuteExtraTool invocation".to_string())?;
    let params = input
        .get(EXTRA_TOOL_PARAMS_FIELD)
        .filter(|value| value.is_object())
        .cloned()
        .ok_or_else(|| "malformed ExecuteExtraTool invocation".to_string())?;
    Ok((tool_name.to_string(), params))
}

/// 解析有效的工具名称
///
/// 当 tool_name 为 [`EXECUTE_EXTRA_TOOL_NAME`] 时，从 `input[EXTRA_TOOL_NAME_FIELD]` 提取目标工具名，
/// 用于 HITL 权限判断。否则直接返回原始工具名。
pub fn resolve_effective_tool_name(tool_name: &str, input: &serde_json::Value) -> String {
    if tool_name == EXECUTE_EXTRA_TOOL_NAME {
        input
            .get(EXTRA_TOOL_NAME_FIELD)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| tool_name.to_string())
    } else {
        tool_name.to_string()
    }
}

/// Core 工具名常量列表（避免分散引用）
const CORE_TOOL_NAMES: &[&str] = &[
    TOOL_READ,
    TOOL_WRITE,
    TOOL_EDIT,
    TOOL_GLOB,
    TOOL_GREP,
    TOOL_FOLDER_OPS,
    TOOL_BASH,
    TOOL_WEBFETCH,
    TOOL_WEBSEARCH,
    TOOL_AGENT,
    TOOL_ASK_USER,
    TOOL_TODO,
    TOOL_SKILL,
    TOOL_DISCOVER_SKILLS,
];

/// 返回 CORE_TOOL_NAMES 按字典序排序后的逗号分隔字符串（含空格）。
///
/// 用于动态生成 Meta 工具 description 中的 Core 列表，确保跨调用稳定。
pub fn core_tools_sorted_csv() -> String {
    let mut names: Vec<&str> = CORE_TOOL_NAMES.to_vec();
    names.sort_unstable();
    names.join(", ")
}

#[cfg(test)]
#[path = "core_tools_test.rs"]
mod tests;
