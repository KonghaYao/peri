//! Compact 契约类型与投影提取函数（自 peri-agent 迁入；`peri-agent::agent::compact_v2`
//! 与 `peri-agent::agent::events` 保留 re-export）。

use serde::{Deserialize, Serialize};

use crate::event::CompactFileInfo;
use crate::messages::BaseMessage;

/// 升级到 Full Compact 的原因（事件契约字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FullEscalationReason {
    /// Micro 回收不足
    InsufficientReclaim,
    /// 达到强制 Full 阈值
    ForceThresholdExceeded,
    /// 手动触发
    ManualForce,
}

/// Compact 执行的语义结果（事件契约字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactOutcome {
    /// 本轮未应用 Compact。
    Skipped,
    /// 已应用 Micro Compact。
    MicroApplied,
    /// 已应用 Smart Compact。
    SmartApplied,
    /// 已成功应用 Full Compact。
    FullApplied,
    /// Full Compact 未成功应用。
    FullFailed,
    /// 仅估算了 Compact 收益，未修改 transcript。
    Shadowed,
    /// 已应用 Micro Compact，但后续 Full Compact 失败。
    MicroAppliedThenFullFailed,
    /// 已应用 Smart Compact，但后续 Full Compact 失败。
    SmartAppliedThenFullFailed,
    /// Compact 已提交（transcript 已修改），但在事件发送前被取消（G6）。
    InterruptedAfterCommit,
    /// Compact 被取消且未提交任何变更（S1.4：CompactEnded 结束观测用）。
    Interrupted,
}

impl CompactOutcome {
    /// 是否已在 transcript 中应用 Compact 变更。
    pub fn has_applied_change(self) -> bool {
        matches!(
            self,
            Self::MicroApplied
                | Self::SmartApplied
                | Self::FullApplied
                | Self::MicroAppliedThenFullFailed
                | Self::SmartAppliedThenFullFailed
                | Self::InterruptedAfterCommit
        )
    }

    /// 是否已成功应用 Full Compact。
    pub fn is_full_applied(self) -> bool {
        matches!(self, Self::FullApplied)
    }
}

/// 从消息历史提取保留的文件摘要（full compact 输出解析）。
pub fn extract_file_info(messages: &[BaseMessage]) -> Vec<CompactFileInfo> {
    let mut files = Vec::new();
    for msg in messages {
        let content = msg.content();
        if let Some(rest) = content.strip_prefix("[最近读取的文件: ") {
            let path = rest.lines().next().unwrap_or("");
            let line_count = rest.lines().count().saturating_sub(1);
            if !path.is_empty() {
                files.push(CompactFileInfo {
                    path: path.to_string(),
                    lines: line_count,
                });
            }
        }
    }
    files
}

/// 从消息历史提取保留的 Skill 名称列表（full compact 输出解析）。
pub fn extract_skill_names(messages: &[BaseMessage]) -> Vec<String> {
    let mut skills = Vec::new();
    for msg in messages {
        let content = msg.content();
        if let Some(rest) = content.strip_prefix("[激活的 Skill 指令: ") {
            let name = rest.lines().next().unwrap_or("");
            if !name.is_empty() {
                skills.push(name.to_string());
            }
        }
    }
    skills
}

// ─── CompactConfig（自 peri-agent 迁入；`peri-agent::agent::compact_v2::config`
// 保留 re-export）────────────────────────────────────────────────────────────

use std::collections::HashMap;

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
/// │ Agent            │ 子任务描述（prompt）等结构化参数不可恢复，     │
/// │                  │ 丢失=子 agent 调度失败，必填字段缺失           │
/// │ AskUserQuestion  │ 用户答案不可恢复，丢失=对话断裂               │
/// │ goal             │ 长期目标状态，丢失=agent 漂移方向             │
/// │ TodoWrite        │ 任务列表结构，丢失=agent 工作记忆重置         │
fn default_excluded_tools() -> Vec<String> {
    vec![
        "Agent".to_string(),
        "AskUserQuestion".to_string(),
        "goal".to_string(),
        "TodoWrite".to_string(),
    ]
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
fn default_micro_field_threshold_chars() -> usize {
    500
}
fn default_micro_field_keep_head_chars() -> usize {
    350
}
fn default_micro_field_keep_tail_chars() -> usize {
    100
}
/// serde 反序列化时校验 compact 阈值在 [0.0, 1.0] 范围内，
/// 超出则 clamp 并发出警告。防止配置错误导致 compact 的 Full 升级路径
/// 被静默绕过（如 auto_compact_threshold 误设为 1.2 时 budget_pct 永不满足条件）。
fn deserialize_threshold_range<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let val = f64::deserialize(deserializer)?;
    if !val.is_finite() || val < 0.0 {
        tracing::warn!(
            "compact_v2 配置: auto_compact_threshold={val} 无效（非正数或非有限），已 clamp 到 0.0"
        );
        return Ok(0.0);
    }
    if val > 1.0 {
        tracing::warn!(
            "compact_v2 配置: auto_compact_threshold={val} 超出合法范围 (0.0..=1.0)，已 clamp 到 1.0"
        );
        return Ok(1.0);
    }
    Ok(val)
}

/// Compact 系统配置（纯数据契约；配置来源为外部配置文件）。
///
/// 反序列化时自动 clamp 阈值到 [0.0, 1.0] 并 warn，防止配置错误导致
/// budget 检查被静默绕过。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactConfig {
    #[serde(default = "default_true")]
    pub auto_compact_enabled: bool,
    /// Full Compact 自动触发阈值（百分比，0.0–1.0）。
    #[serde(
        default = "default_threshold_095",
        deserialize_with = "deserialize_threshold_range"
    )]
    pub auto_compact_threshold: f64,
    #[serde(default = "default_threshold_075")]
    pub micro_compact_threshold: f64,
    #[serde(default = "default_stale_steps")]
    pub micro_compact_stale_steps: usize,
    /// 黑名单工具——这些工具的消息（输入+输出）不参与 Micro 截断。
    /// 默认保留 AskUserQuestion、goal、TodoWrite（对话/任务状态不可恢复），其余工具全部截断。
    #[serde(default = "default_excluded_tools")]
    pub micro_excluded_tools: Vec<String>,
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
    /// [DEPRECATED] 不再使用。Smart Compact 已计划废弃并收敛为 Micro Compact。
    /// 当前仅保留字段以兼容旧配置，但运行时始终按 false 处理。
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
    /// 单个工具输入字段触发截断的字符阈值。
    #[serde(default = "default_micro_field_threshold_chars")]
    pub micro_field_threshold_chars: usize,
    /// 单个工具输入字段截断时保留的头部字符数。
    #[serde(default = "default_micro_field_keep_head_chars")]
    pub micro_field_keep_head_chars: usize,
    /// 单个工具输入字段截断时保留的尾部字符数。
    #[serde(default = "default_micro_field_keep_tail_chars")]
    pub micro_field_keep_tail_chars: usize,
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
            micro_field_threshold_chars: default_micro_field_threshold_chars(),
            micro_field_keep_head_chars: default_micro_field_keep_head_chars(),
            micro_field_keep_tail_chars: default_micro_field_keep_tail_chars(),
            shadow_mode_enabled: false,
            cache_aware_enabled: false,
            tool_retention_map: HashMap::new(),
        }
    }
}

impl CompactConfig {
    pub fn has_valid_micro_field_limits(&self) -> bool {
        self.micro_field_threshold_chars > 0
            && self
                .micro_field_keep_head_chars
                .saturating_add(self.micro_field_keep_tail_chars)
                < self.micro_field_threshold_chars
    }

    /// 在已有配置基础上应用环境变量覆盖
    pub fn apply_env_overrides(&mut self) {
        if std::env::var("DISABLE_COMPACT").is_ok() {
            self.auto_compact_enabled = false;
            self.micro_compact_threshold = 1.0;
        }
        if std::env::var("DISABLE_AUTO_COMPACT").is_ok() {
            self.auto_compact_enabled = false;
        }
        if let Ok(val) = std::env::var("COMPACT_THRESHOLD") {
            if let Ok(threshold) = val.parse::<f64>() {
                if (0.0..=1.0).contains(&threshold) {
                    self.auto_compact_threshold = threshold;
                }
            }
        }
    }
}
