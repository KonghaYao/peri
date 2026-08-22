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

/// 返回工具名按字典序排序后的逗号分隔字符串（含空格）。
///
/// 输入必须来自当前 session 的实际 direct tool 集合。
pub fn direct_tools_sorted_csv<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    let mut names: Vec<&str> = names.into_iter().collect();
    names.sort_unstable();
    names.dedup();
    names.join(", ")
}

/// 将当前 session 的 direct tool 集合格式化为稳定的能力说明。
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
