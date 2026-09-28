//! 工具显示名与参数摘要格式化。
//!
//! 对应 spec/global/domains/tui/tui-rendering.md §2.4.2 的工具名映射表和参数摘要规则。

use crate::i18n;
use peri_acp_types::builtin_mcp::original_tool_name_of_effective;

/// 将原始 `tool_name` 映射为用户友好的显示名。
///
/// **匹配型归一（IF-D6 ④ / A8）**：builtin 一等工具的模型面名字是 effective name
/// （如 `mcp__workspace__Bash`），判定先经 [`original_tool_name_of_effective`]
/// （IF-D15 唯一归一入口）换回原始工具名，使别名分支（`Bash` / `folder_operations`）
/// 对迁移后的名字继续命中。别名分支**未命中**时回落传入名原样——既包括归一表
/// 未命中（未知 / 外部 `mcp__*`），也包括无别名的 builtin 名字（`Read` 等显示
/// 模型面原名）：两种情况都与「不新增 effective name 分支」同一口径。
pub fn format_tool_name(raw: &str) -> String {
    match original_tool_name_of_effective(raw).unwrap_or(raw) {
        "Bash" => i18n::tr("tool-name-shell"),
        "folder_operations" => i18n::tr("tool-name-folder"),
        _ => raw.to_string(),
    }
}

/// 从工具参数 JSON 值中提取显示摘要（按 tool_name 选择关键字段）。
///
/// 对应 spec/global/domains/tui/tui-rendering.md §2.4.2 的 `format_tool_args` 规则。
/// 当 ACP view_mapper 已将 args 预摘要为 `input_summary` 字符串时，
/// 优先使用预摘要；本函数用于需要从原始 args 提取的场合。
///
/// builtin 一等工具的 effective name（`mcp__<实例>__<原始名>`）与迁移前的裸名映射到
/// 同一分支：**匹配型归一**（IF-D6 ④ / A8）先按传入名匹配，未命中再用 IF-D15 的
/// [`original_tool_name_of_effective`] 取原始工具名重试一次。
pub fn format_tool_args(tool_name: &str, args: &serde_json::Value) -> String {
    // 原样优先：未知 / 外部 `mcp__*` 两次都不命中 ⇒ 空串，与迁移前逐位一致。
    if let Some(summary) = format_tool_args_by_name(tool_name, args) {
        return summary;
    }
    if let Some(original) = original_tool_name_of_effective(tool_name)
        && let Some(summary) = format_tool_args_by_name(original, args)
    {
        return summary;
    }
    String::new()
}

/// [`format_tool_args`] 的按名分支本体：未命中任何分支返回 `None`，交由调用方用归一
/// helper 的原始工具名重试。名字字面量只在 `peri-acp-types` 的 builtin 声明表里存一份，
/// 本模块不得硬编码 effective name，也不得自建第二张反查表（A4 / A8）。
fn format_tool_args_by_name(tool_name: &str, args: &serde_json::Value) -> Option<String> {
    let truncate = |s: &str, max: usize| -> String {
        if s.chars().count() > max {
            format!("{}...", s.chars().take(max).collect::<String>())
        } else {
            s.to_string()
        }
    };
    match tool_name {
        "Bash" => Some(
            args.get("command")
                .and_then(|v| v.as_str())
                .map(|s| truncate(s, 400))
                .unwrap_or_default(),
        ),
        "Read" | "Write" | "Edit" => Some(
            args.get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        ),
        "Glob" | "Grep" => Some(
            args.get("pattern")
                .and_then(|v| v.as_str())
                .map(|s| truncate(s, 200))
                .unwrap_or_default(),
        ),
        "folder_operations" => {
            let op = args.get("operation").and_then(|v| v.as_str()).unwrap_or("");
            let path = args
                .get("folder_path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            Some(format!("{} {}", op, path))
        }
        "WebSearch" => Some(
            args.get("query")
                .and_then(|v| v.as_str())
                .map(|s| truncate(s, 60))
                .unwrap_or_default(),
        ),
        "WebFetch" => Some(
            args.get("url")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        ),
        "ExecuteExtraTool" | "SearchExtraTools" => {
            let key = if tool_name == "ExecuteExtraTool" {
                "tool_name"
            } else {
                "query"
            };
            Some(
                args.get(key)
                    .and_then(|v| v.as_str())
                    .map(|s| truncate(s, 40))
                    .unwrap_or_default(),
            )
        }
        "AgentResult" => Some(
            args.get("task_id")
                .and_then(|v| v.as_str())
                .map(|s| truncate(s, 12))
                .unwrap_or_default(),
        ),
        "artifact" => Some(
            args.get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        ),
        "LSP" => Some(
            args.get("operation")
                .and_then(|v| v.as_str())
                .map(|s| truncate(s, 40))
                .unwrap_or_default(),
        ),
        _ => None,
    }
}

#[cfg(test)]
#[path = "tool_display_test.rs"]
mod tests;
