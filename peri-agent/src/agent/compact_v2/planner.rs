//! Planner — Compact 计划和策略类型
//!
//! planner 只能读取 MessageTranscript 和 CompactConfig，绝对不能调用
//! set_truncated、set_excluded、send_persist、invalidate_context_cache 或 provider。

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use crate::messages::{BaseMessage, MessageId};
use crate::session::transcript::{MessageTranscript, TranscriptEntry};
use crate::tools::ContextRetention;

use super::config::CompactConfig;
use super::projection::{
    estimate_projection_chars, MicroCompactPlan, ProjectionAction, ProjectionActionEntry,
    ProjectionTarget, PROJECTION_POLICY_VERSION,
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
    pub tool_input: serde_json::Value,
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
                        tool_input: tc.arguments.clone(),
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

/// 返回需要压缩的顶层字符串字段名。
fn compactable_top_level_string_fields(
    input: &serde_json::Value,
    config: &CompactConfig,
) -> Vec<String> {
    if !config.has_valid_micro_field_limits() {
        tracing::warn!(
            threshold = config.micro_field_threshold_chars,
            keep_head = config.micro_field_keep_head_chars,
            keep_tail = config.micro_field_keep_tail_chars,
            "invalid micro compact field limits"
        );
        return vec![];
    }

    let Some(object) = input.as_object() else {
        return vec![];
    };

    let mut fields: Vec<_> = object
        .iter()
        .filter_map(|(key, value)| {
            value
                .as_str()
                .filter(|text| text.chars().count() > config.micro_field_threshold_chars)
                .map(|_| key.clone())
        })
        .collect();
    fields.sort();
    fields
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
    let mut no_op_candidates: usize = 0;

    for (gi, group) in groups.iter().enumerate() {
        // 跳过最近 N 轮
        if gi >= stale_limit {
            continue;
        }

        for exchange in group.tool_exchanges() {
            // 跳过已有 truncated flag 且 directive 版本一致的消息（避免重复）
            // 仅在 Compact 阶段跳过（skip_existing_truncated=true），
            // Reason 阶段（skip_existing_truncated=false）需要为已标记消息生成完整投影
            // v1 directive 被视为脏数据，不跳过，允许重新规划为 v2
            if skip_existing_truncated
                && transcript.flags(exchange.ai_message_id).truncated
                && transcript
                    .get_flags(exchange.ai_message_id)
                    .and_then(|f| f.projection)
                    .is_some_and(|d| {
                        d.policy_version == super::projection::PROJECTION_POLICY_VERSION
                    })
            {
                continue;
            }
            // 受保护工具 → 跳过（per-call 粒度）
            // 优先使用 retention_map（新），fallback 到 micro_excluded_tools（旧）
            let is_protected = should_preserve_tool(&exchange.tool_name, config);
            if is_protected {
                continue;
            }

            let mut has_any_action = false;

            let fields = compactable_top_level_string_fields(&exchange.tool_input, config);
            if !fields.is_empty() {
                has_any_action = true;
                actions.push(ProjectionActionEntry {
                    message_id: exchange.ai_message_id,
                    target: ProjectionTarget::ToolCall {
                        tool_call_id: exchange.tool_call_id.clone(),
                    },
                    action: ProjectionAction::CompactToolInput {
                        fields,
                        keep_head: config.micro_field_keep_head_chars,
                        keep_tail: config.micro_field_keep_tail_chars,
                    },
                });
            } else if config.has_valid_micro_field_limits() && exchange.tool_input.is_object() {
                // 兜底：无超长字段时整条压缩（fields 空 = 整条占位语义）。
                // 防止普通工具调用（短参数）永远不产生 action，导致 Micro Compact
                // 在常规对话中静默失效（plan 空 → Skip → 无压缩也无通知）。
                has_any_action = true;
                actions.push(ProjectionActionEntry {
                    message_id: exchange.ai_message_id,
                    target: ProjectionTarget::ToolCall {
                        tool_call_id: exchange.tool_call_id.clone(),
                    },
                    action: ProjectionAction::CompactToolInput {
                        fields: vec![],
                        keep_head: config.micro_field_keep_head_chars,
                        keep_tail: config.micro_field_keep_tail_chars,
                    },
                });
            }

            // 仅压缩超过阈值的成功 ToolResult；错误结果保留诊断信息。
            if config.has_valid_micro_field_limits() {
                for (_, result_entry) in &exchange.tool_result_entries {
                    if skip_existing_truncated
                        && transcript.flags(result_entry.message.id()).truncated
                        && transcript
                            .get_flags(result_entry.message.id())
                            .and_then(|f| f.projection)
                            .is_some_and(|d| {
                                d.policy_version == super::projection::PROJECTION_POLICY_VERSION
                            })
                    {
                        continue;
                    }
                    let BaseMessage::Tool {
                        content, is_error, ..
                    } = &result_entry.message
                    else {
                        continue;
                    };
                    if *is_error
                        || content.text_content().chars().count()
                            <= config.micro_field_threshold_chars
                    {
                        continue;
                    }
                    has_any_action = true;
                    actions.push(ProjectionActionEntry {
                        message_id: result_entry.message.id(),
                        target: ProjectionTarget::Message,
                        action: ProjectionAction::CompactToolResult {
                            keep_head: config.micro_field_keep_head_chars,
                            keep_tail: config.micro_field_keep_tail_chars,
                            preserve_recovery_handle: true,
                        },
                    });
                }
            }

            if !has_any_action {
                no_op_candidates += 1;
            }
        }
    }

    // 无 action → 提前返回，避免不必要的 token 估算
    if actions.is_empty() {
        return MicroCompactPlan {
            policy_version: PROJECTION_POLICY_VERSION,
            target_reclaim_tokens: config.target_headroom_tokens,
            actions,
            estimated_before_tokens: 0,
            estimated_after_tokens: 0,
            estimated_tokens_saved: 0,
            changed_messages: 0,
            changed_fields: 0,
            no_op_candidates,
        };
    }

    // 统计：去重 message_id 数量
    let changed_messages: usize = actions
        .iter()
        .map(|a| a.message_id)
        .collect::<HashSet<_>>()
        .len();
    // 统计：CompactToolInput 中的所有字段总数
    let changed_fields: usize = actions
        .iter()
        .filter_map(|a| match &a.action {
            ProjectionAction::CompactToolInput { fields, .. } => Some(fields.len()),
            _ => None,
        })
        .sum();

    // 估算投影前后 token 数量
    let (before_chars, after_chars) = estimate_projection_chars(transcript, &actions);
    // 整条压缩兜底（fields 空）会把短参数替换为 `{"_compact_note": ...}` 占位，
    // 估算如实反映该占位长度，短输入下 after 可大于 before（设计权衡：宁可放大也不静默 no-op）。
    let has_placeholder_fallback = actions.iter().any(|a| {
        matches!(
            &a.action,
            ProjectionAction::CompactToolInput { fields, .. } if fields.is_empty()
        )
    });
    debug_assert!(
        after_chars <= before_chars || has_placeholder_fallback,
        "投影后字符数不应大于投影前: after={after_chars} > before={before_chars}"
    );
    let before = before_chars / 4;
    let after = after_chars / 4;
    let estimated_tokens_saved = before_chars.saturating_sub(after_chars) / 4;

    MicroCompactPlan {
        policy_version: PROJECTION_POLICY_VERSION,
        target_reclaim_tokens: config.target_headroom_tokens,
        actions,
        estimated_before_tokens: before,
        estimated_after_tokens: after,
        estimated_tokens_saved,
        changed_messages,
        changed_fields,
        no_op_candidates,
    }
}

#[cfg(test)]
#[path = "planner_test.rs"]
mod tests;
