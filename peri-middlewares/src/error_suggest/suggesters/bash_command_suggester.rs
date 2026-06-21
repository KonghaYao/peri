use crate::error_suggest::context::ErrorContext;
use crate::error_suggest::format::did_you_mean_summary;
use crate::error_suggest::matcher::fuzzy_filter;
use crate::error_suggest::registry::{ErrorSuggester, Suggestion};

/// C1：Bash 命令不存在建议
pub struct BashCommandSuggester;

impl ErrorSuggester for BashCommandSuggester {
    fn suggest(&self, ctx: &ErrorContext) -> Option<Suggestion> {
        if ctx.tool_name != "Bash" {
            return None;
        }

        // 识别信号：stderr 含 "command not found" + 输出含 [Exit code: 127]
        let lower = ctx.error_message.to_lowercase();
        if !lower.contains("command not found") && !lower.contains("not found in path") {
            return None;
        }
        if !ctx.error_message.contains("[Exit code: 127]") {
            return None;
        }

        // 从 input 提取命令名
        let cmd = ctx.tool_input.get("command").and_then(|v| v.as_str())?;
        let cmd_name = cmd.split_whitespace().next()?;

        // 从 PATH 中扫描所有可执行文件，fuzzy 匹配
        let candidates = scan_path_executables();
        let matched = fuzzy_filter(&candidates, cmd_name);
        let top3: Vec<String> = matched.into_iter().take(3).collect();

        if top3.is_empty() {
            return Some(Suggestion::new(format!(
                "Command {cmd_name:?} not found in PATH. Verify it is installed or check for typos."
            )));
        }

        let summary = did_you_mean_summary("command", &top3);
        Some(Suggestion::new(summary))
    }
}

/// 扫描 PATH 中所有可执行文件名（去重）
fn scan_path_executables() -> Vec<String> {
    let path_env = match std::env::var_os("PATH") {
        Some(p) => p,
        None => return Vec::new(),
    };
    let mut seen = std::collections::HashSet::new();
    let mut all: Vec<String> = Vec::new();
    for dir in std::env::split_paths(&path_env) {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if seen.insert(name.to_string()) {
                        all.push(name.to_string());
                    }
                }
                if all.len() > 500 {
                    return all; // 性能保护
                }
            }
        }
    }
    all
}
