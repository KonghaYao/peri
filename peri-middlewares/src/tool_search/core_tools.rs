//! Core Tools 白名单定义与延迟加载判定逻辑

use std::{collections::HashSet, sync::LazyLock};

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

/// 核心工具白名单（始终发送给 LLM，共 12 个）
///
/// - 文件操作 (6): Read, Write, Edit, Glob, Grep, folder_operations
/// - 执行 (1): Bash
/// - Web (2): WebFetch, WebSearch
/// - 交互 (2): Agent, AskUserQuestion
/// - 管理 (1): TodoWrite
pub static CORE_TOOLS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        // 文件操作
        TOOL_READ,
        TOOL_WRITE,
        TOOL_EDIT,
        TOOL_GLOB,
        TOOL_GREP,
        TOOL_FOLDER_OPS,
        // 执行
        TOOL_BASH,
        // Web
        TOOL_WEBFETCH,
        TOOL_WEBSEARCH,
        // 交互
        TOOL_AGENT,
        TOOL_ASK_USER,
        // 管理
        TOOL_TODO,
    ]
    .into_iter()
    .collect()
});

/// 元工具集合（Tool Search 延迟加载机制的工具，始终发送给 LLM）
pub static META_TOOLS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [SEARCH_EXTRA_TOOLS_NAME, EXECUTE_EXTRA_TOOL_NAME]
        .into_iter()
        .collect()
});

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

/// 判定工具是否为延迟加载工具（Deferred Tool）
///
/// 返回 `true` 表示该工具应从 LLM 可见工具列表中移除，
/// 通过 SearchExtraTools 按需发现，ExecuteExtraTool 代理执行。
///
/// # Examples
///
/// ```ignore
/// assert_eq!(is_deferred_tool("Read"), false);           // Core Tool
/// assert_eq!(is_deferred_tool("SearchExtraTools"), false); // Meta Tool
/// assert_eq!(is_deferred_tool("CronRegister"), true);     // Deferred Tool
/// assert_eq!(is_deferred_tool("mcp__slack__send_message"), true); // MCP Tool
/// ```
pub fn is_deferred_tool(tool_name: &str) -> bool {
    !CORE_TOOLS.contains(tool_name) && !META_TOOLS.contains(tool_name)
}

/// 返回 CORE_TOOLS 按字典序排序后的逗号分隔字符串（含空格）。
///
/// 用于动态生成 Meta 工具 description 中的 Core 列表，确保跨调用稳定（CORE_TOOLS
/// 是 HashSet 迭代序不稳定，必须排序以保护 prompt cache 前缀，P1-1）。
pub fn core_tools_sorted_csv() -> String {
    let mut names: Vec<&str> = CORE_TOOLS.iter().copied().collect();
    names.sort_unstable();
    names.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("core_tools_test.rs");
}
