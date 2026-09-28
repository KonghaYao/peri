// 基础设施（trait / registry / context / matcher / format）从 peri-agent re-export
// 避免循环依赖：peri-agent 不依赖 peri-middlewares，所以核心类型定义在底层
pub use peri_agent::error_suggest::{
    context, format, matcher, registry, ErrorContext, ErrorSuggestRegistry, ErrorSuggester,
    Suggestion, ToolRegistrySnapshot,
};

pub mod default_registry;
pub mod suggesters;

pub use default_registry::{build_default_registry, build_tool_registry_snapshot};

use peri_acp_types::builtin_mcp::original_tool_name_of_effective;

/// 归一后的原始工具名（未命中归一表返回原样）——5 个 suggester 的唯一归一入口。
///
/// builtin 一等工具的模型面名字是 effective name（`mcp__<实例>__<原始名>`，如
/// `mcp__workspace__Bash`）；suggester 的工具名门槛比较必须经此归一，否则门槛
/// 静默失效、建议不再产生。未命中（未知 / 外部 `mcp__*`）返回原样 ⇒ 与迁移前
/// 逐位一致（保守语义不变）。名字字面量只在 `peri_acp_types::builtin_mcp`
/// 声明一份（IF-D15 唯一入口），本模块不复制、不反拆。
pub(crate) fn normalized_tool_name(name: &str) -> &str {
    original_tool_name_of_effective(name).unwrap_or(name)
}

// Suggester 测试入口（测试文件留在 peri-middlewares 因为它们测试 peri-middlewares 的 suggesters）
#[cfg(test)]
#[path = "suggesters/path_suggester_test.rs"]
mod path_suggester_test;

#[cfg(test)]
#[path = "suggesters/range_suggester_test.rs"]
mod range_suggester_test;

#[cfg(test)]
#[path = "suggesters/glob_pattern_suggester_test.rs"]
mod glob_pattern_suggester_test;

#[cfg(test)]
#[path = "suggesters/regex_suggester_test.rs"]
mod regex_suggester_test;

#[cfg(test)]
#[path = "suggesters/json_schema_suggester_test.rs"]
mod json_schema_suggester_test;

#[cfg(test)]
#[path = "suggesters/bash_command_suggester_test.rs"]
mod bash_command_suggester_test;

#[cfg(test)]
#[path = "suggesters/subagent_suggester_test.rs"]
mod subagent_suggester_test;
