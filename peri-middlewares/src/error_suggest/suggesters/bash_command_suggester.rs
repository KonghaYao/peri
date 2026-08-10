use crate::error_suggest::context::ErrorContext;
use crate::error_suggest::format::did_you_mean_summary;
use crate::error_suggest::matcher::fuzzy_filter_min;
use crate::error_suggest::registry::{ErrorSuggester, Suggestion};

/// C1：Bash 命令不存在建议
pub struct BashCommandSuggester;

/// fuzzy 相似度下限：低于此分视为"无关候选"。
/// Skim 子序列匹配下，丢字符类拼错（dockr→docker）≥ 90，
/// 短查询/泛化子序列噪声（xy→xylophone）≤ 51，60 为干净分隔点。
const MIN_FUZZY_SCORE: i64 = 60;

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

        // 从 PATH 中扫描所有可执行文件，fuzzy 匹配。
        // 双重过滤：score 阈值剔除短查询噪声（xy→xylophone），
        // 首字符约束剔除跨长字符串的稀疏子序列噪声（carg→lli-child-target）——
        // 拼错命令名时首字符几乎不会错，此约束不会误杀真实拼错（dockr→docker）。
        let candidates = scan_path_executables();
        let first_char = cmd_name.chars().next();
        let matched = fuzzy_filter_min(&candidates, cmd_name, MIN_FUZZY_SCORE);
        let matched: Vec<String> = match first_char {
            Some(fc) => matched.into_iter().filter(|c| c.starts_with(fc)).collect(),
            None => matched,
        };
        let top3: Vec<String> = matched.into_iter().take(3).collect();

        if top3.is_empty() {
            // 无相似候选：不点名任何命令，给出环境类诊断（command not found
            // 多数情况是环境问题而非拼写错误）
            return Some(Suggestion::new(format!(
                "Command {cmd_name:?} not found in PATH. Verify it is installed or check for typos. If it is installed, the environment (PATH / conda / venv) may not be activated."
            )));
        }

        let summary = did_you_mean_summary("command", &top3);
        Some(Suggestion::new(summary))
    }
}

/// 扫描 PATH 中所有可执行文件名（去重）。
///
/// 配额策略：每目录最多取 MAX_PER_DIR 个（去重后），全局上限 MAX_TOTAL——
/// 必须遍历完所有 PATH 目录，否则 PATH 前部目录（如系统 bin）会占满池子，
/// 后部目录（如 ~/.cargo/bin）被饿死，导致建议候选不完整。
fn scan_path_executables() -> Vec<String> {
    const MAX_PER_DIR: usize = 100;
    const MAX_TOTAL: usize = 3000;
    let path_env = match std::env::var_os("PATH") {
        Some(p) => p,
        None => return Vec::new(),
    };
    let mut seen = std::collections::HashSet::new();
    let mut all: Vec<String> = Vec::new();
    for dir in std::env::split_paths(&path_env) {
        let mut dir_collected = 0;
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if seen.insert(name.to_string()) {
                        all.push(name.to_string());
                        dir_collected += 1;
                    }
                }
                if dir_collected >= MAX_PER_DIR || all.len() >= MAX_TOTAL {
                    break;
                }
            }
        }
        if all.len() >= MAX_TOTAL {
            return all;
        }
    }
    all
}
