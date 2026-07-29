//! Planner — Compact 计划和策略类型
//!
//! planner 只能读取 MessageTranscript 和 CompactConfig，绝对不能调用
//! set_truncated、set_excluded、send_persist、invalidate_context_cache 或 provider。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::messages::{BaseMessage, MessageId};
use crate::session::transcript::{MessageTranscript, TranscriptEntry};
use crate::tools::ContextRetention;

use super::config::CompactConfig;
use super::projection::{
    MicroCompactPlan, ProjectionAction, ProjectionActionEntry, ProjectionTarget,
};

/// 上下文压力 — 用于决定是否需要 compact 及回收目标
#[derive(Debug, Clone)]
pub struct ContextPressure {
    pub estimated_tokens: u64,
    pub context_window: u32,
    pub output_reserve: u32,
    pub predicted_tool_growth: u32,
    pub safety_buffer: u32,
    pub cache_hit_rate: f64,
}

impl ContextPressure {
    /// 目标 token 用量上限
    pub fn target_tokens(&self) -> u64 {
        let reserve = self.output_reserve as u64
            + self.predicted_tool_growth as u64
            + self.safety_buffer as u64;
        self.context_window.saturating_sub(reserve as u32) as u64
    }

    /// 需要回收的 token 数量（饱和减法，不溢出）。
    ///
    /// 为防止 reclaim_target=0 阻断 Full 升级，加 2% 窗口最小值。
    pub fn target_reclaim_tokens(&self) -> u64 {
        let raw = self.estimated_tokens.saturating_sub(self.target_tokens());
        let min_floor = (self.context_window as u64 * 2) / 100;
        raw.max(min_floor)
    }
}

/// 需要升级到 Full Compact 的原因
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FullEscalationReason {
    /// Micro 回收不足
    InsufficientReclaim,
    /// 达到强制 Full 阈值
    ForceThresholdExceeded,
    /// 手动触发
    ManualForce,
}

/// Compact 策略配置
#[derive(Debug, Clone)]
pub struct CompactPolicy {
    /// 目标回收 token 下限
    pub target_reclaim_tokens: u64,
    /// 强制升级 Full 的阈值百分比（0.0-1.0）
    pub force_full_threshold: f64,
    /// Shadow mode：只估算不应用
    pub shadow_mode: bool,
    /// Cache-aware：高缓存命中时延迟清理
    pub cache_aware: bool,
}

impl Default for CompactPolicy {
    fn default() -> Self {
        Self {
            target_reclaim_tokens: 0,
            force_full_threshold: 0.95,
            shadow_mode: false,
            cache_aware: false,
        }
    }
}

/// Compact 应用结果报告
#[derive(Debug, Clone)]
pub struct ApplyReport {
    pub candidate_count: usize,
    pub changed_messages: usize,
    pub changed_fields: usize,
    pub no_op_candidates: usize,
    pub estimated_tokens_saved: u64,
    pub persistence_batch_size: usize,
}

// ─── TurnGroup / ToolExchange ──────────────────────────────────────────────────

/// 一次人类交流（从 Human 消息开始，到下一条 Human 前结束）
#[derive(Debug, Clone)]
pub struct TurnGroup {
    /// 该组内 Human 消息
    pub human_entry: TranscriptEntry,
    /// AI 消息及其位置
    pub ai_entries: Vec<(usize, TranscriptEntry)>,
    /// ToolResult 消息及其位置（按 tool_call_id 索引）
    pub tool_results: HashMap<String, (usize, TranscriptEntry)>,
}

/// 工具调用交换（AI tool_use + 对应所有 ToolResult）
#[derive(Debug, Clone)]
pub struct ToolExchange {
    pub tool_call_id: String,
    pub tool_name: String,
    pub ai_message_id: MessageId,
    pub tool_result_entries: Vec<(usize, TranscriptEntry)>,
}

impl TurnGroup {
    /// 从 transcript 自有消息中构建 TurnGroup 列表
    ///
    /// 跳过 `ancestor_len` 之前的祖先消息，仅处理自有消息。
    /// 每个 TurnGroup 以 Human 消息开头。
    pub fn collect(entries: &[TranscriptEntry], ancestor_len: usize) -> Vec<TurnGroup> {
        let mut groups = Vec::new();
        let mut current: Option<TurnGroup> = None;

        for (i, entry) in entries.iter().enumerate() {
            if i < ancestor_len {
                continue;
            }
            match &entry.message {
                BaseMessage::Human { .. } => {
                    if let Some(g) = current.take() {
                        groups.push(g);
                    }
                    current = Some(TurnGroup {
                        human_entry: entry.clone(),
                        ai_entries: Vec::new(),
                        tool_results: HashMap::new(),
                    });
                }
                BaseMessage::Ai { .. } => {
                    if let Some(ref mut g) = current {
                        g.ai_entries.push((i, entry.clone()));
                    }
                }
                BaseMessage::Tool { tool_call_id, .. } => {
                    if let Some(ref mut g) = current {
                        g.tool_results
                            .insert(tool_call_id.clone(), (i, entry.clone()));
                    }
                }
                _ => {}
            }
        }
        if let Some(g) = current.take() {
            groups.push(g);
        }
        groups
    }

    /// 从本组中提取所有 ToolExchange
    pub fn tool_exchanges(&self) -> Vec<ToolExchange> {
        let mut exchanges = Vec::new();
        for (_, ai_entry) in &self.ai_entries {
            if let BaseMessage::Ai { tool_calls, .. } = &ai_entry.message {
                for tc in tool_calls {
                    let result_entries: Vec<_> = self
                        .tool_results
                        .get(&tc.id)
                        .map(|(pos, e)| vec![(*pos, e.clone())])
                        .unwrap_or_default();
                    exchanges.push(ToolExchange {
                        tool_call_id: tc.id.clone(),
                        tool_name: tc.name.clone(),
                        ai_message_id: ai_entry.message.id(),
                        tool_result_entries: result_entries,
                    });
                }
            }
        }
        exchanges
    }
}

// ─── plan_micro ────────────────────────────────────────────────────────────────

/// 判断是否应保留完整的工具调用（不压缩）
///
/// 优先级：
/// 1. 先查 `config.tool_retention_map`（新 metadata-based 方法）
/// 2. Fallback 到 `config.micro_excluded_tools`（旧黑名单）
/// 3. 默认：非 Preserve → 可压缩
fn should_preserve_tool(tool_name: &str, config: &CompactConfig) -> bool {
    // 1. 先查 retention_map
    let name_lower = tool_name.to_lowercase();
    if let Some(retention) = config.tool_retention_map.get(&name_lower) {
        return matches!(
            retention,
            ContextRetention::Preserve | ContextRetention::StateBearing
        );
    }

    // 2. Fallback 到旧黑名单
    let is_excluded = config
        .micro_excluded_tools
        .iter()
        .any(|e| e.eq_ignore_ascii_case(tool_name));

    if is_excluded {
        return true; // 旧黑名单中的工具 → 保留
    }

    // 3. 默认：非 Preserve → 可压缩
    false
}

/// 生成 Micro Compact 计划（纯数据，零副作用）
///
/// 遍历 TurnGroup，跳过最近 `micro_compact_stale_steps` 轮，对每个 tool exchange
/// 按 context retention 和 safety 规则生成 ProjectionAction。
///
/// # 规则
/// - 跳过最近 N 轮（`stale_steps`）
/// - 已 truncated 的消息 → 当 `skip_existing_truncated` 时跳过
/// - 受保护工具（`micro_excluded_tools`）→ 跳过
/// - 错误 ToolResult → 跳过 ToolResult 的 compact，但 tool_use 仍可压缩
/// - 安全可压缩的工具 → CompactToolInput（per tool_call_id）+ CompactToolResult
pub fn plan_micro(
    transcript: &MessageTranscript,
    config: &CompactConfig,
    skip_existing_truncated: bool,
) -> MicroCompactPlan {
    let ancestor_len = transcript.ancestor_len();
    let entries = transcript.entries();
    let groups = TurnGroup::collect(entries, ancestor_len);

    let total_groups = groups.len();
    let stale_limit = total_groups.saturating_sub(config.micro_compact_stale_steps);

    let mut actions = Vec::new();

    for (gi, group) in groups.iter().enumerate() {
        // 跳过最近 N 轮
        if gi >= stale_limit {
            continue;
        }

        for exchange in group.tool_exchanges() {
            // 跳过已有 truncated flag 的消息（避免重复）
            // 仅在 Compact 阶段跳过（skip_existing_truncated=true），
            // Reason 阶段（skip_existing_truncated=false）需要为已标记消息生成完整投影
            if skip_existing_truncated && transcript.flags(exchange.ai_message_id).truncated {
                continue;
            }
            // 受保护工具 → 跳过（per-call 粒度）
            // 优先使用 retention_map（新），fallback 到 micro_excluded_tools（旧）
            let is_protected = should_preserve_tool(&exchange.tool_name, config);
            if is_protected {
                continue;
            }

            // 检查是否有错误 ToolResult
            let has_error = exchange
                .tool_result_entries
                .iter()
                .any(|(_, e)| matches!(&e.message, BaseMessage::Tool { is_error: true, .. }));

            // tool_use 输入 → CompactToolInput（per tool_call_id 粒度，不误伤同消息其他调用）
            actions.push(ProjectionActionEntry {
                message_id: exchange.ai_message_id,
                target: ProjectionTarget::ToolCall {
                    tool_call_id: exchange.tool_call_id.clone(),
                },
                action: ProjectionAction::CompactToolInput {
                    fields: vec![],
                    preserve_shape: true,
                },
            });

            // 成功的 ToolResult → CompactToolResult（错误结果保留诊断信息）
            if !has_error {
                for (_, result_entry) in &exchange.tool_result_entries {
                    if skip_existing_truncated
                        && transcript.flags(result_entry.message.id()).truncated
                    {
                        continue;
                    }
                    actions.push(ProjectionActionEntry {
                        message_id: result_entry.message.id(),
                        target: ProjectionTarget::Message,
                        action: ProjectionAction::CompactToolResult {
                            keep_head: config.tool_result_keep_chars,
                            keep_tail: 200,
                            preserve_recovery_handle: true,
                        },
                    });
                }
            }
        }
    }

    // 估算投影前后 token 数量
    let (before, after) = estimate_tokens(transcript, &actions);
    let estimated_tokens_saved = before.saturating_sub(after);

    MicroCompactPlan {
        policy_version: 1,
        target_reclaim_tokens: config.target_headroom_tokens,
        actions,
        estimated_before_tokens: before,
        estimated_after_tokens: after,
        estimated_tokens_saved,
    }
}

// ─── estimate_tokens ────────────────────────────────────────────────────────────

/// 简单 token 估算：对 transcript 中指定消息的原始/投影内容做字符计数估算
///
/// 使用 chars / 4 的粗略估算（与 TokenTracker 保持一致），
/// 后续 Phase 7 shadow mode 会用真实 input_tokens 校准。
fn estimate_tokens(
    transcript: &MessageTranscript,
    actions: &[ProjectionActionEntry],
) -> (u64, u64) {
    let mut before = 0u64;
    let mut after = 0u64;

    let entries = transcript.entries();

    for entry in entries {
        let id = entry.message.id();
        let content_str = entry.message.message_content().text_content();
        let chars = content_str.chars().count() as u64;

        // 检查是否有针对此消息的 action
        let has_action = actions.iter().any(|a| a.message_id == id);

        if has_action {
            // 有 action → 投影后减少
            let projected_chars = (chars / 3).max(1);
            before += chars;
            after += projected_chars;
        }
        // 无 action 的消息不计入节省量（它们不变）
    }

    // 除以 4 转换为 token 估算
    (before / 4, after / 4)
}
