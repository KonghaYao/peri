use std::env;

use serde::{Deserialize, Serialize};

const DEFAULT_COMPACTABLE_TOOLS: &[&str] = &["Bash", "Read", "Glob", "Grep", "Write", "Edit"];

fn default_true() -> bool {
    true
}
fn default_threshold_085() -> f64 {
    0.85
}
fn default_threshold_070() -> f64 {
    0.70
}
fn default_stale_steps() -> usize {
    5
}
fn default_compactable_tools() -> Vec<String> {
    DEFAULT_COMPACTABLE_TOOLS
        .iter()
        .map(|s| s.to_string())
        .collect()
}
fn default_summary_max_tokens() -> u32 {
    16000
}
fn default_re_inject_max_files() -> usize {
    5
}
fn default_re_inject_max_tokens_per_file() -> u32 {
    5000
}
fn default_re_inject_file_budget() -> u32 {
    25000
}
fn default_re_inject_skills_budget() -> u32 {
    25000
}
fn default_max_consecutive_failures() -> u32 {
    3
}
fn default_ptl_max_retries() -> u32 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactConfig {
    #[serde(default = "default_true")]
    pub auto_compact_enabled: bool,
    #[serde(default = "default_threshold_085")]
    pub auto_compact_threshold: f64,
    #[serde(default = "default_threshold_070")]
    pub micro_compact_threshold: f64,
    #[serde(default = "default_stale_steps")]
    pub micro_compact_stale_steps: usize,
    #[serde(default = "default_compactable_tools")]
    pub micro_compactable_tools: Vec<String>,
    #[serde(default = "default_summary_max_tokens")]
    pub summary_max_tokens: u32,
    #[serde(default = "default_re_inject_max_files")]
    pub re_inject_max_files: usize,
    #[serde(default = "default_re_inject_max_tokens_per_file")]
    pub re_inject_max_tokens_per_file: u32,
    #[serde(default = "default_re_inject_file_budget")]
    pub re_inject_file_budget: u32,
    #[serde(default = "default_re_inject_skills_budget")]
    pub re_inject_skills_budget: u32,
    #[serde(default = "default_max_consecutive_failures")]
    pub max_consecutive_failures: u32,
    #[serde(default = "default_ptl_max_retries")]
    pub ptl_max_retries: u32,
}

impl Default for CompactConfig {
    fn default() -> Self {
        Self {
            auto_compact_enabled: default_true(),
            auto_compact_threshold: default_threshold_085(),
            micro_compact_threshold: default_threshold_070(),
            micro_compact_stale_steps: default_stale_steps(),
            micro_compactable_tools: default_compactable_tools(),
            summary_max_tokens: default_summary_max_tokens(),
            re_inject_max_files: default_re_inject_max_files(),
            re_inject_max_tokens_per_file: default_re_inject_max_tokens_per_file(),
            re_inject_file_budget: default_re_inject_file_budget(),
            re_inject_skills_budget: default_re_inject_skills_budget(),
            max_consecutive_failures: default_max_consecutive_failures(),
            ptl_max_retries: default_ptl_max_retries(),
        }
    }
}

impl CompactConfig {
    /// 在已有配置基础上应用环境变量覆盖
    pub fn apply_env_overrides(&mut self) {
        if env::var("DISABLE_COMPACT").is_ok() {
            self.auto_compact_enabled = false;
            self.micro_compact_threshold = 1.0;
        }
        if env::var("DISABLE_AUTO_COMPACT").is_ok() {
            self.auto_compact_enabled = false;
        }
        if let Ok(val) = env::var("COMPACT_THRESHOLD") {
            if let Ok(threshold) = val.parse::<f64>() {
                if (0.0..=1.0).contains(&threshold) {
                    self.auto_compact_threshold = threshold;
                }
            }
        }
    }
}

/// Compact 摘要 Human 消息的续接指令标记。
///
/// 作为单一事实源，由三条路径共享：
/// - v2 自动 compact（`crate::agent::compact_v2::full_compact_inner`）
/// - `/compact` 命令路径（`peri-acp/src/session/command/compact/invariant.rs`）
/// - TUI 识别层（`peri-tui/src/ui/message_view/build.rs::COMPACT_HINT`）
///
/// 修改时必须同步三方，否则 compact 输出不被 TUI 折叠显示。
pub const CONTINUATION_HINT: &str =
    "[Context has been compacted. Continue working based on the summary above.]";

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
