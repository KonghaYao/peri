//! Compact 契约类型与投影提取函数（自 peri-agent 迁入；`peri-agent::agent::compact_v2`
//! 与 `peri-agent::agent::events` 保留 re-export）。

use serde::{Deserialize, Serialize};

use crate::event::CompactFileInfo;
use crate::messages::BaseMessage;

/// Full Compact 摘要消息的内部续接标记。
pub const CONTINUATION_HINT: &str =
    "[Context has been compacted. Continue working based on the summary above.]";

/// 自动 Micro 最小净收益分母：完整上下文窗口的 1/10，向上取整。
///
/// 这是收益定义的一部分，不是可调旋钮；若需改变比例，必须作为独立决策重新评估。
pub const MICRO_MIN_GAIN_DIVISOR: u64 = 10;

/// 升级到 Full Compact 的原因（事件契约字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FullEscalationReason {
    /// Micro 回收不足
    InsufficientReclaim,
    /// 达到强制 Full 阈值
    ForceThresholdExceeded,
    /// 历史驻留字节超过独立预算
    ResidentHistoryBudgetExceeded,
    /// 手动触发
    ManualForce,
}

/// Compact 执行的语义结果（事件契约字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactOutcome {
    /// 本轮未应用 Compact。
    Skipped,
    /// Micro 规划或提交失败；未发布新投影。
    MicroFailed,
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
    /// 达到 Micro 触发区间，但合法候选的净收益不足或候选为空；本轮延后提交。
    ///
    /// 延后不是失败：不写 transcript、不做归档正文验证、不增加失败计数。
    Deferred,
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

    /// 是否为「收益不足的主动延后」。延后不是失败。
    pub fn is_deferred(self) -> bool {
        matches!(self, Self::Deferred)
    }
}

/// 自动 Micro 延后的结构化原因（事件契约字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactDeferralReason {
    /// 净收益 G 低于最小收益门槛 M。
    InsufficientGain,
    /// 没有通过内容与协议校验的合法候选。
    NoCandidates,
    /// 缺少上下文窗口（C == 0），不执行比例型自动 Micro。
    MissingContextBudget,
}

/// 延后事实：原因与同一冻结请求中的收益口径。
///
/// `estimated_gain_tokens` 是最终完整渲染后的净减少量（G），
/// `min_gain_tokens` 是完整上下文窗口的 10% 向上取整（M）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactDeferral {
    pub reason: CompactDeferralReason,
    pub estimated_gain_tokens: u64,
    pub min_gain_tokens: u64,
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
    2
}
/// 显式配置只可增加保护；语义策略来自工具目录，不再按名字永久豁免。
fn default_excluded_tools() -> Vec<String> {
    Vec::new()
}
fn default_summary_max_tokens() -> u32 {
    16000
}
/// 单次 Full Compact 内允许的逻辑摘要生成次数（首轮 + 恢复）。
fn default_summary_max_requests() -> u32 {
    3
}
fn default_max_consecutive_failures() -> u32 {
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
/// 历史正文驻留预算默认 128 MiB（`own + inherited` 的 `reserved` 下界）。
fn default_max_resident_history_bytes() -> u64 {
    128 * 1024 * 1024
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// 显式附加保护工具名；默认空，工具目录中的语义保护仍优先。
    #[serde(default = "default_excluded_tools")]
    pub micro_excluded_tools: Vec<String>,
    /// Full Compact 摘要的**长度目标上限**（不是 provider 输出上限的覆写）。
    ///
    /// 有效目标 = `min(summary_max_tokens, 当前模型已解析的单次输出上限)`；目标只
    /// 影响提示词中的长度说明，实际请求预算仍来自同一已解析上限，二者同源（H6）。
    /// 模型上限无法解析时目标取本值，请求沿用 provider 默认。零值不是「不限」，
    /// 而是明确的配置错误（`CompactSummaryBudgetInvalid`）。
    #[serde(default = "default_summary_max_tokens")]
    pub summary_max_tokens: u32,
    /// 单次 Full Compact 内允许的逻辑摘要生成次数（首轮 1 次 + 恢复至多 N-1 次）。
    ///
    /// 空响应重新生成、截断续写与格式补全共用这一个预算：每次调用摘要模型前
    /// 递增计数，绝不发起第 N+1 次逻辑生成。值为 1 时只运行首次生成（不恢复）；
    /// 0 不是「不限」，而是明确的配置错误（`CompactSummaryBudgetInvalid`）。
    #[serde(default = "default_summary_max_requests")]
    pub summary_max_requests: u32,
    #[serde(default = "default_max_consecutive_failures")]
    pub max_consecutive_failures: u32,

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
    /// 历史正文驻留字节上限（`own + inherited` 的 `reserved` 下界之和）。
    ///
    /// 与 token 压力独立：超过后强制一次 Full，Full 成功后仍超限只记录残余，
    /// 不做硬准入或终止运行。0 表示关闭该门控。
    #[serde(default = "default_max_resident_history_bytes")]
    pub max_resident_history_bytes: u64,
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
    /// 只可增加保护，不可覆盖工具目录中的硬语义保护或授权未知副作用退场。
    /// planner 从当前工具目录快照获取权威策略，配置不重新定义工具语义。
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
            summary_max_requests: default_summary_max_requests(),
            max_consecutive_failures: default_max_consecutive_failures(),
            smart_compact_enabled: default_false(),
            smart_keep_recent_msgs: default_smart_keep_recent_msgs(),
            smart_keep_recent_tools: default_smart_keep_recent_tools(),
            target_headroom_tokens: default_headroom_tokens(),
            max_resident_history_bytes: default_max_resident_history_bytes(),
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

/// 已删除的 compact 配置键（旧配置文件仍可能携带）。
///
/// `ptl_max_retries` 从不参与运行，已从配置模型、默认值与文档中删除。类型化模型
/// 不再认识该键，serde 会静默忽略它——所以调用方必须在**原始文档**上核对并给出
/// 显式迁移诊断，不能让它悄悄换语义。
pub const LEGACY_COMPACT_KEYS: [&str; 1] = ["ptl_max_retries"];

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
