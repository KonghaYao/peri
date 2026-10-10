//! `command not found` 的模型诊断：工具在执行点选择恢复文本，
//! 从 PATH 扫描条目名做模糊匹配给出 did-you-mean 候选；
//! 无合适候选时不硬凑候选，给出环境类兜底诊断。

use crate::fuzzy::fuzzy_filter_min;
use std::ffi::OsString;

/// fuzzy 相似度下限：低于此分视为"无关候选"。
/// Skim 子序列匹配下，丢字符类拼错（dockr→docker）≥ 90，
/// 短查询/泛化子序列噪声（xy→xylophone）≤ 51，60 为干净分隔点。
const MIN_FUZZY_SCORE: i64 = 60;

/// 识别 `command not found` 并生成恢复建议；不匹配返回 `None`（原输出不变）。
pub(crate) fn command_not_found_hint(
    command: &str,
    output: &str,
    exit_code: Option<i32>,
) -> Option<String> {
    let lower = output.to_lowercase();
    let missing_command = match exit_code {
        Some(127) => lower.contains("command not found") || lower.contains("not found in path"),
        Some(1) => output.lines().any(|line| {
            line.split_once(':').is_some_and(|(label, value)| {
                label.trim().trim_start_matches('+').trim() == "FullyQualifiedErrorId"
                    && value.trim() == "CommandNotFoundException"
            })
        }),
        _ => false,
    };
    if !missing_command {
        return None;
    }
    let name = extract_missing_command(output, command);
    if name.is_empty() {
        return None;
    }
    let candidates = scan_path_executables_in(std::env::var_os("PATH"));
    Some(build_hint(&name, &candidates))
}

/// 从候选生成诊断文本（候选已在 PATH 扫描阶段去重）。
fn build_hint(name: &str, candidates: &[String]) -> String {
    // 双重过滤：score 阈值剔除短查询噪声（xy→xylophone），
    // 首字符约束剔除跨长字符串的稀疏子序列噪声（carg→lli-child-target）——
    // 拼错命令名时首字符几乎不会错，此约束不会误杀真实拼错（dockr→docker）。
    let first_char = name.chars().next();
    let matched: Vec<String> = fuzzy_filter_min(candidates, name, MIN_FUZZY_SCORE)
        .into_iter()
        .filter(|c| first_char.is_none_or(|fc| c.starts_with(fc)))
        .take(3)
        .collect();
    if matched.is_empty() {
        // 无相似候选：不点名任何命令，给出环境类诊断（command not found
        // 多数情况是环境问题而非拼写错误）
        return format!(
            "Command `{name}` not found in PATH. Verify it is installed or check for typos. If it is installed, the environment (PATH / conda / venv) may not be activated."
        );
    }
    format!(
        "Command `{name}` not found in PATH. Did you mean: {}?",
        matched.join(", ")
    )
}

/// 从 shell 错误消息提取缺失的命令名；无法提取时回退命令首词。
///
/// 已知格式：
/// - zsh：`zsh:1: command not found: gti`
/// - bash / sudo：`bash: line 1: gti: command not found`、`sudo: gti: command not found`
/// - `not found in path`：格式不定 → 回退命令首词
fn extract_missing_command(output: &str, command: &str) -> String {
    for line in output.lines() {
        let lower = line.to_lowercase();
        if lower.contains("not recognized") {
            if let Some((name, _)) = line.split_once(" : ") {
                return trim_quotes(name.trim()).to_string();
            }
        }
        // zsh 形态：`<...>command not found: NAME`
        const AFTER: &str = "command not found:";
        if let Some(idx) = lower.find(AFTER) {
            if let Some(name) = line[idx + AFTER.len()..].split_whitespace().next() {
                return trim_quotes(name).to_string();
            }
        }
        // bash / sudo 形态：`<...>: NAME: command not found`
        if let Some(idx) = lower.find(": command not found") {
            let head = line[..idx].trim_end();
            if let Some(name) = head.rsplit(':').next().map(str::trim) {
                if !name.is_empty() {
                    return trim_quotes(name).to_string();
                }
            }
        }
    }
    command
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Shell 消息可能把命令名包在引号里（如 fish 风格 `'gti'`）。
fn trim_quotes(name: &str) -> &str {
    name.trim_matches(|c| c == '\'' || c == '"' || c == '`')
}

/// 扫描 PATH 中所有条目名（去重）。
///
/// 配额策略：每目录最多取 MAX_PER_DIR 个（去重后），全局上限 MAX_TOTAL。
/// 两个上限都要能容纳真实 PATH（`/usr/bin` 约千条、Homebrew bin 数百条）——
/// 每目录配额过小会让目录条目只被随机截取一部分（read_dir 顺序不可控），
/// 常见命令（git 等）大概率缺席候选池；全局上限防止病态 PATH 拖慢失败路径。
fn scan_path_executables_in(path_env: Option<OsString>) -> Vec<String> {
    const MAX_PER_DIR: usize = 2000;
    const MAX_TOTAL: usize = 5000;
    let Some(path_env) = path_env else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut all: Vec<String> = Vec::new();
    for dir in std::env::split_paths(&path_env) {
        if all.len() >= MAX_TOTAL {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut dir_collected = 0;
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if seen.insert(name.clone()) {
                all.push(name);
                dir_collected += 1;
            }
            if dir_collected >= MAX_PER_DIR {
                break;
            }
        }
    }
    all
}

#[cfg(test)]
#[path = "shell_hints_test.rs"]
mod tests;
