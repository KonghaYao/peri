use crate::error_suggest::context::ToolRegistrySnapshot;
use crate::error_suggest::registry::{ErrorSuggestRegistry, ErrorSuggester};
use crate::error_suggest::suggesters::{
    bash_command_suggester::BashCommandSuggester, glob_pattern_suggester::GlobPatternSuggester,
    json_schema_suggester::JsonSchemaSuggester, path_suggester::PathSuggester,
    range_suggester::RangeSuggester, regex_suggester::RegexSuggester,
};
use std::sync::Arc;

/// 构造默认 registry，按短路顺序注册
/// 顺序：参数语法类（廉价）-> 范围 -> 路径 -> 命令。
/// Agent 建议由 SubAgentTool 在实际 loader 调用中生成，不能从 session snapshot 推断。
pub fn build_default_registry() -> Arc<ErrorSuggestRegistry> {
    let suggesters: Vec<Box<dyn ErrorSuggester>> = vec![
        Box::new(JsonSchemaSuggester),  // B5 最先：参数级错误最廉价
        Box::new(GlobPatternSuggester), // B3
        Box::new(RegexSuggester),       // B4
        Box::new(RangeSuggester),       // B2
        Box::new(PathSuggester),        // A1-A4（需 IO）
        Box::new(BashCommandSuggester), // C1（需 PATH 扫描）
    ];
    Arc::new(ErrorSuggestRegistry::new(suggesters))
}

/// 从 collect_tools 结果 + .claude/agents/ 目录构建 snapshot
pub fn build_tool_registry_snapshot(
    tool_names: impl IntoIterator<Item = String>,
    agents_dir: Option<&std::path::Path>,
) -> ToolRegistrySnapshot {
    let mut all_tool_names: std::collections::HashSet<String> = tool_names.into_iter().collect();

    let mut subagent_types: std::collections::HashSet<String> =
        crate::subagent::built_in_agent_types()
            .iter()
            .map(|s| s.to_string())
            .collect();

    // 扫描 .claude/agents/：扁平 {id}.md 和嵌套 {id}/agent.md 两种格式
    // 与 subagent::scan_agents 的扫描逻辑保持一致
    if let Some(dir) = agents_dir {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if path.extension().and_then(|e| e.to_str()) != Some("md") {
                        continue;
                    }
                    if let Some(stem) = path.file_name().and_then(|n| n.to_str()) {
                        if let Some(id) = stem.strip_suffix(".md") {
                            subagent_types.insert(id.to_string());
                        }
                    }
                } else if path.is_dir() {
                    let nested = path.join("agent.md");
                    if !nested.is_file() {
                        continue;
                    }
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        subagent_types.insert(name.to_string());
                    }
                }
            }
        }
    }

    // subagent_type 也是有效"工具名"候补
    for t in &subagent_types {
        all_tool_names.insert(t.clone());
    }

    ToolRegistrySnapshot {
        all_tool_names,
        subagent_types,
    }
}
