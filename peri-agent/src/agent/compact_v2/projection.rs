//! Projection — 消息投影类型和 Provider 能力定义
//!
//! ## render_llm_view 纯函数
//!
//! 根据 `MicroCompactPlan` + `ProviderCapabilities` 渲染 LLM 可见消息列表：
//! - 不修改 Transcript，不写 flags，不调数据库
//! - 正确处理所有 ContentBlock 类型（Text/Image/Document/ToolUse/ToolResult/Reasoning）
//! - Tool input 投影后保持 JSON object 根类型
//! - CJK 截断用字符边界而非字节切片
//! - Image/Document Base64 payload 移除
//!
//! 消息/内容构造（借用式 `MessageDraft` 与逐块投影）在 sibling `projection_render`；
//! 本模块保留计划类型、指令恢复、估算与渲染编排。

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use crate::error::AgentResult;
use crate::messages::{BaseMessage, ContentBlock, MessageContent, MessageId};
use crate::session::transcript::MessageTranscript;
pub use peri_acp_types::projection::{
    MessageProjectionDirective, ProjectionAction, ProjectionActionEntry, ProjectionTarget,
};

pub const PROJECTION_POLICY_VERSION: u32 = 3;

pub(super) fn invalid(detail: impl std::fmt::Display) -> crate::error::AgentError {
    crate::error::AgentError::LlmError(format!("invalid context projection: {detail}"))
}

#[path = "projection_archive.rs"]
mod archive;

#[path = "projection_render.rs"]
mod render;

/// Provider 消息协议类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderProtocol {
    OpenAI,
    OpenAiResponses,
    Anthropic,
    Generic,
}

/// Provider 能力 — 决定哪些投影操作是安全的
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub protocol: ProviderProtocol,
    /// 带签名 reasoning 是否必须整体保留（Anthropic=true）
    pub signed_reasoning_must_be_whole: bool,
}

impl Default for ProviderCapabilities {
    fn default() -> Self {
        Self {
            protocol: ProviderProtocol::Generic,
            signed_reasoning_must_be_whole: false,
        }
    }
}

impl ProviderCapabilities {
    pub fn openai() -> Self {
        Self {
            protocol: ProviderProtocol::OpenAI,
            signed_reasoning_must_be_whole: false,
        }
    }

    pub fn anthropic() -> Self {
        Self {
            protocol: ProviderProtocol::Anthropic,
            signed_reasoning_must_be_whole: true,
        }
    }
}

// ─── MicroCompactPlan ─────────────────────────────────────────────────────────

/// Micro Compact 计划（纯数据，不含消息副本）
#[derive(Debug, Default, Clone)]
pub struct MicroCompactPlan {
    pub policy_version: u32,
    /// Existing source flags used to reject a stale plan while allowing tail appends.
    pub source_flags: Vec<(MessageId, peri_acp_types::store::MessageFlags)>,
    pub source_content: Vec<(MessageId, [u8; 32])>,
    pub catalogs: Vec<peri_acp_types::context_archive::ArchiveCatalog>,
    pub archive_roots: Vec<peri_acp_types::context_archive::ArchiveRoot>,
    pub receipt_budget_tokens: u64,
    pub diagnostics: super::planner::PlanningDiagnostics,
    pub target_reclaim_tokens: u64,
    /// 按 transcript 位置稳定排序的 action 列表
    pub actions: Vec<ProjectionActionEntry>,
    pub estimated_before_tokens: u64,
    pub estimated_after_tokens: u64,
    pub estimated_tokens_saved: u64,
    /// 去重 message_id 数量
    pub changed_messages: usize,
    /// CompactToolInput 中的所有字段总数
    pub changed_fields: usize,
    /// 通过 stale/retention 筛选但无内容的候选数
    pub no_op_candidates: usize,
}

impl MicroCompactPlan {
    /// 估算 token 已节省量是否满足回收目标
    pub fn meets_target(&self) -> bool {
        self.estimated_tokens_saved >= self.target_reclaim_tokens
    }

    /// 投影是否有实际 action 需要应用
    pub fn has_changes(&self) -> bool {
        !self.actions.is_empty()
    }
}

// ─── plan_from_persisted_directives ───────────────────────────────────────────

/// 错误信息常量：transcript 中无可用持久化 directive。
///
/// Reason 保持 canonical 可见视图；新计划只由 Compact 阶段显式生成。
pub const NO_PERSISTED_DIRECTIVES: &str = "no persisted directives in transcript";

/// 错误信息常量：持久化 directive 的 policy_version 与当前不匹配。
pub const DIRECTIVE_VERSION_MISMATCH: &str = "persisted directive version mismatch";

/// 错误信息常量：消息被标记 truncated 但缺少 projection directive（G1 fail-closed）。
pub const CORRUPTED_PROJECTION: &str = "message truncated without projection directive";

/// 当前可解码 persisted directive 的恢复结果。
#[derive(Debug)]
pub enum PersistedDirectiveRestore {
    Absent,
    Valid(RestoredProjectionDirectives),
    Invalid,
}

/// 已恢复并校验通过的持久化投影指令。
///
/// 只表达恢复结果本身，**不携带收益估算指标**：普通 Reason、上下文压力检查与
/// 恢复视图入口都不参与 Compact 决策，不应为估算付出正文扫描与复制成本。
/// 候选收益由 planner 对当前与候选视图使用同一估算器计算。
#[derive(Debug, Clone, Default)]
pub struct RestoredProjectionDirectives {
    pub policy_version: u32,
    /// 按 transcript 顺序恢复出的可执行 action
    pub actions: Vec<ProjectionActionEntry>,
}

/// 当前 renderer 可安全执行、且不会直接或间接影响 ToolCall/ToolUse 的 action。
fn is_safe_projection_action(message: &BaseMessage, entry: &ProjectionActionEntry) -> bool {
    matches!(
        (&entry.target, &entry.action, message),
        (
            ProjectionTarget::Message | ProjectionTarget::ContentBlock { index: 0 },
            ProjectionAction::CompactToolResult { .. },
            BaseMessage::Tool {
                is_error: false,
                ..
            },
        )
    )
}

/// 从 transcript 中恢复当前可解码的 projection directive。
///
/// 纯恢复：只校验并收集可执行 action，不做收益估算（估算属 Compact 决策路径）。
pub fn plan_from_persisted_directives(
    transcript: &MessageTranscript,
    expected_version: u32,
) -> PersistedDirectiveRestore {
    let mut actions = Vec::new();
    let mut has_any_directive = false;

    for source in transcript.entries() {
        let id = source.id();
        if transcript.is_excluded(id) {
            continue;
        }
        // 只读查询：不克隆 projection directive 本体
        let projection = transcript.projection_ref(id);
        let truncated = transcript.is_truncated(id);

        match projection {
            Some(directive) => {
                if directive.recovery_only {
                    return PersistedDirectiveRestore::Invalid;
                }
                has_any_directive = true;
                if directive.format_version != 1
                    || ![2, expected_version].contains(&directive.policy_version)
                {
                    return PersistedDirectiveRestore::Invalid;
                }
                // 当前可解码 legacy entry 只有可证明与 ToolCall/ToolUse 独立的
                // ToolResult message-level action 才能恢复；其余一律 Preserve（丢弃）。
                for entry in &directive.entries {
                    if entry.message_id != id {
                        return PersistedDirectiveRestore::Invalid;
                    }
                    // Reminder metadata participates in restoration checks, but no
                    // projection action is allowed to rewrite its canonical payload.
                    let Some(msg) = source.as_message() else {
                        return PersistedDirectiveRestore::Invalid;
                    };
                    if directive.policy_version == 2 {
                        if is_safe_projection_action(msg, entry) {
                            actions.push(entry.clone());
                        }
                    } else if archive::safe_action(msg, entry) {
                        actions.push(entry.clone());
                    } else {
                        return PersistedDirectiveRestore::Invalid;
                    }
                }
            }
            None => {
                // G1: fail-closed on unknown directives
                // truncated=true + projection=None + not excluded = corrupted state
                // 前面的 entry 级筛选已跳过 excluded，包括 canonical Reminder。
                if truncated {
                    return PersistedDirectiveRestore::Invalid;
                }
                // 无 truncated 标记 → 正常跳过，不生成投影 action
            }
        }
    }

    if !has_any_directive {
        return PersistedDirectiveRestore::Absent;
    }

    PersistedDirectiveRestore::Valid(RestoredProjectionDirectives {
        policy_version: expected_version,
        actions,
    })
}

// ─── 测试专用调用计数 ───────────────────────────────────────────────────────────
//
// 按线程隔离，避免 cargo test 并发执行时测试之间互相污染计数；
// 纯恢复路径（`plan_from_persisted_directives`）不得调用字符估算，由
// projection_restore_test 断言调用次数恒为 0。

#[cfg(test)]
thread_local! {
    static ESTIMATE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// 重置当前线程的字符估算调用计数（测试专用）。
#[cfg(test)]
pub(crate) fn reset_estimate_calls() {
    ESTIMATE_CALLS.with(|calls| calls.set(0));
}

/// 读取当前线程的字符估算调用次数（测试专用）。
#[cfg(test)]
pub(crate) fn estimate_call_count() -> usize {
    ESTIMATE_CALLS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn count_estimate_call() {
    ESTIMATE_CALLS.with(|calls| calls.set(calls.get() + 1));
}

/// 对指定 actions 列表估算实际会被投影的字符数。
///
/// 仅统计 `CompactToolInput` 指定的顶层 string 字段，以及成功 `ToolResult` 的 text；
/// 找不到目标、不符合类型或 helper 不会缩短时均不计入。
///
/// 匹配用一次性构建的 message_id 索引（预期 O(M+A)），正文只做字符统计不复制。
#[cfg(test)]
pub(crate) fn estimate_projection_chars(
    transcript: &MessageTranscript,
    actions: &[ProjectionActionEntry],
) -> (u64, u64) {
    #[cfg(test)]
    count_estimate_call();

    let mut before = 0u64;
    let mut after = 0u64;

    let mut actions_by_id: HashMap<MessageId, Vec<&ProjectionActionEntry>> = HashMap::new();
    for action in actions {
        actions_by_id
            .entry(action.message_id)
            .or_default()
            .push(action);
    }

    for entry in transcript.entries() {
        if transcript.is_excluded(entry.id()) {
            continue;
        }
        let Some(message) = entry.as_message() else {
            continue;
        };
        let BaseMessage::Tool {
            content,
            is_error: false,
            ..
        } = message
        else {
            continue;
        };
        let Some(candidates) = actions_by_id.get(&message.id()) else {
            continue;
        };
        let Some(action) = effective_tool_result_action(candidates) else {
            continue;
        };
        let ProjectionAction::CompactToolResult {
            keep_head,
            keep_tail,
            ..
        } = action
        else {
            continue;
        };
        if let Some((total_chars, projected_chars)) =
            projection_char_counts(content, *keep_head, *keep_tail)
        {
            before += total_chars as u64;
            after += projected_chars as u64;
        }
    }

    (before, after)
}

/// ToolResult 的 Message 与 ContentBlock(0) 指向同一逻辑文本。只有恰好一条
/// CompactToolResult action 时才执行；重复或冲突均 fail-closed 为 Preserve。
fn effective_tool_result_action<'a>(
    entries: &[&'a ProjectionActionEntry],
) -> Option<&'a ProjectionAction> {
    let mut effective = None;
    for entry in entries {
        if !matches!(
            entry.target,
            ProjectionTarget::Message | ProjectionTarget::ContentBlock { index: 0 }
        ) || !matches!(entry.action, ProjectionAction::CompactToolResult { .. })
        {
            continue;
        }
        if effective.is_some() {
            return None;
        }
        effective = Some(&entry.action);
    }
    effective
}

/// Reason 专用：在归档视图之后追加未处理结果事实。
///
/// 受保护条目的 canonical 位置不变（不改写 ID、不重复落库）；这里只把它们的
/// 模型投影从原位置移到历史/摘要之后，确保摘要遗漏终态时主模型仍能看到。
pub(crate) fn render_persisted_llm_view_with_protected(
    transcript: &MessageTranscript,
    caps: &ProviderCapabilities,
    protected: &crate::session::async_result_mailbox::ProtectedResults,
) -> AgentResult<Vec<BaseMessage>> {
    if protected.is_empty() {
        return render_persisted_llm_view(transcript, caps);
    }
    if protected.items.iter().any(|item| {
        transcript
            .projection_ref(item.delivery_id)
            .is_some_and(|directive| !directive.entries.is_empty())
    }) {
        return Err(crate::error::AgentError::LlmError(
            "protected result cannot carry a context projection".into(),
        ));
    }
    let mut messages = render_persisted_llm_view(transcript, caps)?;
    let protected_ids: HashSet<MessageId> = protected
        .items
        .iter()
        .map(|item| item.delivery_id)
        .collect();
    messages.retain(|message| !protected_ids.contains(&message.id()));
    for item in &protected.items {
        let mut body =
            peri_acp_types::system_reminder::encode_system_reminder(item.reminder.as_ref())
                .map_err(|error| crate::error::AgentError::Other(anyhow::Error::new(error)))?;
        if let Some(diagnostic) = &item.diagnostic {
            body.push_str(&format!(
                "\n\n[处理诊断] stage: {}; {}{}",
                diagnostic.stage,
                diagnostic.summary,
                if diagnostic.auto_recovery_allowed {
                    ""
                } else {
                    "；需要用户继续或新的外部输入后重试"
                }
            ));
        }
        messages.push(BaseMessage::Human {
            id: item.delivery_id,
            content: MessageContent::text(body),
        });
    }
    crate::messages::tool_pairing::repair_model_tool_pairing(messages)
}

/// Reason 与 Full 共用的已提交模型视图；无效投影明确失败，禁止全文回灌。
pub fn render_persisted_llm_view(
    transcript: &MessageTranscript,
    caps: &ProviderCapabilities,
) -> AgentResult<Vec<BaseMessage>> {
    match plan_from_persisted_directives(transcript, PROJECTION_POLICY_VERSION) {
        PersistedDirectiveRestore::Valid(restored) => {
            let view = render_llm_view(transcript, &restored.actions, caps)?;
            crate::messages::tool_pairing::repair_model_tool_pairing(view)
        }
        PersistedDirectiveRestore::Invalid => {
            tracing::error!("持久化投影无效；拒绝构建请求，要求重载或修复");
            Err(crate::error::AgentError::LlmError(
                "invalid persisted context projection; reload or repair the session".into(),
            ))
        }
        PersistedDirectiveRestore::Absent => {
            crate::messages::tool_pairing::repair_model_tool_pairing(
                transcript.visible_model_messages()?,
            )
        }
    }
}

// ─── render_llm_view ──────────────────────────────────────────────────────────

/// 根据 actions 和 provider 能力渲染 LLM 可见消息列表。
///
/// 纯函数：不修改 transcript，不写 flags，不调数据库。
/// 只消费 action，不依赖 `MicroCompactPlan` 的收益指标——恢复路径没有估算值。
pub fn render_llm_view(
    transcript: &MessageTranscript,
    actions: &[ProjectionActionEntry],
    caps: &ProviderCapabilities,
) -> AgentResult<Vec<BaseMessage>> {
    archive::render(transcript, actions, &[], &[], None, caps)
}

/// Render a candidate with its not-yet-committed catalogs using the same recovery renderer.
pub fn render_micro_plan(
    transcript: &MessageTranscript,
    plan: &MicroCompactPlan,
    caps: &ProviderCapabilities,
) -> AgentResult<Vec<BaseMessage>> {
    let mut actions = match plan_from_persisted_directives(transcript, PROJECTION_POLICY_VERSION) {
        PersistedDirectiveRestore::Valid(restored) => restored.actions,
        PersistedDirectiveRestore::Absent => Vec::new(),
        PersistedDirectiveRestore::Invalid => {
            return Err(crate::error::AgentError::LlmError(
                "invalid persisted projection".into(),
            ))
        }
    };
    let key = |action: &ProjectionActionEntry| {
        (
            action.message_id,
            match &action.target {
                ProjectionTarget::Message => (0, 0, String::new()),
                ProjectionTarget::ContentBlock { index } => (1, *index, String::new()),
                ProjectionTarget::ToolCall { tool_call_id } => (2, 0, tool_call_id.clone()),
            },
        )
    };
    let replaced: HashSet<_> = plan.actions.iter().map(key).collect();
    actions.retain(|old| !replaced.contains(&key(old)));
    actions.extend(plan.actions.iter().cloned());
    archive::render(
        transcript,
        &actions,
        &plan.catalogs,
        &plan.archive_roots,
        Some(plan.receipt_budget_tokens),
        caps,
    )
}

/// head/tail 投影的几何：`(原始字符数, 投影后字符数)`；`None` 表示不缩短。
///
/// 与 `render::apply_head_tail` 共用同一几何规则（sentinel 由同一格式化函数给出），
/// 供只统计字符数、不复制正文的调用方使用。
fn head_tail_geometry(
    total: usize,
    head_chars: usize,
    tail_chars: usize,
) -> Option<(usize, usize)> {
    if total <= head_chars.saturating_add(tail_chars) {
        return None;
    }
    let skipped = total.saturating_sub(head_chars + tail_chars);
    let sentinel_chars = peri_acp_types::sentinel::format_projection_sentinel_v1(skipped)
        .chars()
        .count();
    // head + "\n" + sentinel + "\n" + tail
    let projected = head_chars + 1 + sentinel_chars + 1 + tail_chars;
    (projected < total).then_some((total, projected))
}

/// 测试辅助：与逐块 renderer 使用同一几何，未知块和媒体不参与文本裁剪。
#[cfg(test)]
fn projection_char_counts(
    content: &MessageContent,
    head_chars: usize,
    tail_chars: usize,
) -> Option<(usize, usize)> {
    fn blocks_count(blocks: &[ContentBlock], head: usize, tail: usize) -> (usize, usize) {
        blocks
            .iter()
            .map(|block| match block {
                ContentBlock::Text { text } => {
                    head_tail_geometry(text.chars().count(), head, tail).unwrap_or_default()
                }
                ContentBlock::ToolResult { content, .. } => blocks_count(content, head, tail),
                _ => (0, 0),
            })
            .fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
    }
    let counts = match content {
        MessageContent::Text(text) => {
            head_tail_geometry(text.chars().count(), head_chars, tail_chars)?
        }
        MessageContent::Blocks(blocks) => blocks_count(blocks, head_chars, tail_chars),
        MessageContent::Raw(_) => (0, 0),
    };
    (counts.0 > counts.1).then_some(counts)
}

// ─── validate_projected_view ──────────────────────────────────────────────────

/// 验证投影后视图的协议不变量
fn validate_projected_view(
    messages: &[BaseMessage],
    caps: &ProviderCapabilities,
) -> AgentResult<()> {
    // 1. tool_call_id 配对检查
    let mut tool_use_ids: HashSet<String> = HashSet::new();
    let mut tool_result_ids: HashSet<String> = HashSet::new();

    for msg in messages {
        match msg {
            BaseMessage::Ai { tool_calls, .. } => {
                for tc in tool_calls {
                    tool_use_ids.insert(tc.id.clone());
                }
            }
            BaseMessage::Tool { tool_call_id, .. } => {
                tool_result_ids.insert(tool_call_id.clone());
            }
            _ => {}
        }
    }

    // 每个 tool_result 必须有对应的 tool_use
    for rid in &tool_result_ids {
        if !tool_use_ids.contains(rid) {
            // 注意：这不是硬错误——tool_use 可能已被 exclude
            // 但我们记录 warning
            tracing::warn!(
                tool_use_id = %rid,
                "ToolResult 无对应 ToolUse（可能已被 compact）"
            );
        }
    }

    // 2. Tool input 类型检查（仅对投影过的 tool_calls 检查 object 根类型）
    // 工具可以合法接受 JSON array 参数——不对非 object 的未投影 tool_calls 报硬错误
    for msg in messages {
        if let BaseMessage::Ai { tool_calls, .. } = msg {
            for tc in tool_calls {
                if !tc.arguments.is_object() {
                    tracing::debug!(
                        tool_name = %tc.name,
                        "非 object tool input（部分工具合法接受 JSON array）"
                    );
                }
            }
        }
    }

    // 3. Signed reasoning 完整性（Anthropic）
    if caps.signed_reasoning_must_be_whole {
        for msg in messages {
            match msg.message_content() {
                MessageContent::Text(_) => {}
                MessageContent::Blocks(blocks) => {
                    for block in blocks {
                        if let ContentBlock::Reasoning {
                            signature: Some(_),
                            text,
                        } = block
                        {
                            // 验证策略：project_block 对 reasoning 块的非 Keep 动作
                            // 会静默 fallthrough 克隆，因此此处只做防御性日志；
                            // 实际安全由 provider adapter 的签名校验保证。
                            tracing::debug!(
                                len = text.chars().count(),
                                "已投影视图中出现带签名的 reasoning block"
                            );
                        }
                    }
                }
                MessageContent::Raw(values) => {
                    for value in values {
                        if value["type"].as_str() == Some("reasoning")
                            && value["signature"].as_str().is_some()
                        {
                            tracing::debug!(
                                len = value["text"].as_str().unwrap_or_default().chars().count(),
                                "已投影视图中出现带签名的 reasoning block"
                            );
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
#[path = "projection_borrowed_test.rs"]
mod borrowed_tests;
