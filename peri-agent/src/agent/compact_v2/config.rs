use std::collections::HashMap;
use std::env;

use serde::{Deserialize, Serialize};

use crate::tools::ContextRetention;

fn default_true() -> bool {
    true
}
fn default_false() -> bool {
    false
}
fn default_threshold_095() -> f64 {
    0.95
}
fn default_threshold_075() -> f64 {
    0.75
}
fn default_stale_steps() -> usize {
    3
}
/// Micro Compact 黑名单默认值——这些工具的消息不被截断。
///
/// │ 工具             │ 理由                                          │
/// │──────────────────│───────────────────────────────────────────────│
/// │ AskUserQuestion  │ 用户答案不可恢复，丢失=对话断裂               │
/// │ goal             │ 长期目标状态，丢失=agent 漂移方向             │
/// │ TodoWrite        │ 任务列表结构，丢失=agent 工作记忆重置         │
fn default_excluded_tools() -> Vec<String> {
    vec![
        "AskUserQuestion".to_string(),
        "goal".to_string(),
        "TodoWrite".to_string(),
    ]
}
fn default_micro_min_affected() -> usize {
    5
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
fn default_smart_keep_recent_msgs() -> usize {
    5
}
fn default_smart_keep_recent_tools() -> usize {
    3
}
fn default_headroom_tokens() -> u64 {
    8192
}
fn default_tool_result_keep_chars() -> usize {
    2000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactConfig {
    #[serde(default = "default_true")]
    pub auto_compact_enabled: bool,
    #[serde(default = "default_threshold_095")]
    pub auto_compact_threshold: f64,
    #[serde(default = "default_threshold_075")]
    pub micro_compact_threshold: f64,
    #[serde(default = "default_stale_steps")]
    pub micro_compact_stale_steps: usize,
    /// 黑名单工具——这些工具的消息（输入+输出）不参与 Micro 截断。
    /// 默认保留 AskUserQuestion、goal、TodoWrite（对话/任务状态不可恢复），其余工具全部截断。
    #[serde(default = "default_excluded_tools")]
    pub micro_excluded_tools: Vec<String>,
    /// Micro 压缩量下限——affected_count 低于此值时判定 Micro 无效，升级为 Full。
    #[serde(default = "default_micro_min_affected")]
    pub micro_min_affected: usize,
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

    // ── Smart Compact 配置 ──────────────────────────────────────────────
    /// 是否启用 Smart Compact 策略（替代 Micro Compact），默认 false
    #[serde(default = "default_false")]
    pub smart_compact_enabled: bool,
    /// Smart Compact：保留最近 N 条 User/Assistant 对话消息
    #[serde(default = "default_smart_keep_recent_msgs")]
    pub smart_keep_recent_msgs: usize,
    /// Smart Compact：保留最近 M 个工具调用结果
    #[serde(default = "default_smart_keep_recent_tools")]
    pub smart_keep_recent_tools: usize,

    // ── 投影与压力控制 ──────────────────────────────────────────────────
    /// 目标上下文余量 token 数（用于 ContextPressure 计算）
    #[serde(default = "default_headroom_tokens")]
    pub target_headroom_tokens: u64,
    /// 工具结果保留的最小字符数
    #[serde(default = "default_tool_result_keep_chars")]
    pub tool_result_keep_chars: usize,
    /// Shadow mode：只估算不应用
    #[serde(default)]
    pub shadow_mode_enabled: bool,
    /// Cache-aware 策略：高缓存命中时延迟清理
    #[serde(default)]
    pub cache_aware_enabled: bool,

    // ── Retention Metadata ──────────────────────────────────────────────
    /// 工具 retention 映射（工具名小写 → retention 分类）
    /// 优先于 micro_excluded_tools，为空时使用后者。
    /// planner 使用此映射而非直接访问 BaseTool 实例。
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub tool_retention_map: HashMap<String, ContextRetention>,
}

impl Default for CompactConfig {
    fn default() -> Self {
        Self {
            auto_compact_enabled: default_true(),
            auto_compact_threshold: default_threshold_095(),
            micro_compact_threshold: default_threshold_075(),
            micro_compact_stale_steps: default_stale_steps(),
            micro_excluded_tools: default_excluded_tools(),
            micro_min_affected: default_micro_min_affected(),
            summary_max_tokens: default_summary_max_tokens(),
            re_inject_max_files: default_re_inject_max_files(),
            re_inject_max_tokens_per_file: default_re_inject_max_tokens_per_file(),
            re_inject_file_budget: default_re_inject_file_budget(),
            re_inject_skills_budget: default_re_inject_skills_budget(),
            max_consecutive_failures: default_max_consecutive_failures(),
            ptl_max_retries: default_ptl_max_retries(),
            smart_compact_enabled: default_false(),
            smart_keep_recent_msgs: default_smart_keep_recent_msgs(),
            smart_keep_recent_tools: default_smart_keep_recent_tools(),
            target_headroom_tokens: default_headroom_tokens(),
            tool_result_keep_chars: default_tool_result_keep_chars(),
            shadow_mode_enabled: false,
            cache_aware_enabled: false,
            tool_retention_map: HashMap::new(),
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
