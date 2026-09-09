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

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use crate::error::AgentResult;
use crate::messages::{BaseMessage, ContentBlock, MessageContent, MessageId};
use crate::session::transcript::MessageTranscript;
pub use peri_acp_types::projection::{
    MessageProjectionDirective, ProjectionAction, ProjectionActionEntry, ProjectionTarget,
};

pub const PROJECTION_POLICY_VERSION: u32 = 2;

/// Provider 消息协议类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderProtocol {
    OpenAI,
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
/// 调用方应识别此特定消息并回退到 `plan_micro`。
pub const NO_PERSISTED_DIRECTIVES: &str = "no persisted directives in transcript";

/// 错误信息常量：持久化 directive 的 policy_version 与当前不匹配。
pub const DIRECTIVE_VERSION_MISMATCH: &str = "persisted directive version mismatch";

/// 错误信息常量：消息被标记 truncated 但缺少 projection directive（G1 fail-closed）。
pub const CORRUPTED_PROJECTION: &str = "message truncated without projection directive";

/// 当前可解码 persisted directive 的恢复结果。
#[derive(Debug)]
pub enum PersistedDirectiveRestore {
    Absent,
    Valid(MicroCompactPlan),
    Invalid,
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
pub fn plan_from_persisted_directives(
    transcript: &MessageTranscript,
    expected_version: u32,
) -> PersistedDirectiveRestore {
    let visible = transcript.visible_messages();
    let mut actions = Vec::new();
    let mut has_any_directive = false;

    for msg in &visible {
        let id = msg.id();
        let flags = transcript.flags(id);

        match flags.projection {
            Some(ref directive) => {
                has_any_directive = true;
                if directive.policy_version != expected_version {
                    return PersistedDirectiveRestore::Invalid;
                }
                // 当前可解码 legacy entry 只有可证明与 ToolCall/ToolUse 独立的
                // ToolResult message-level action 才能恢复；其余一律 Preserve（丢弃）。
                for entry in &directive.entries {
                    if entry.message_id != id {
                        return PersistedDirectiveRestore::Invalid;
                    }
                    if is_safe_projection_action(msg, entry) {
                        actions.push(entry.clone());
                    }
                }
            }
            None => {
                // G1: fail-closed on unknown directives
                // truncated=true + projection=None + not excluded = corrupted state
                // （visible_messages() 已过滤 excluded，此处消息必然非 excluded）
                if flags.truncated {
                    return PersistedDirectiveRestore::Invalid;
                }
                // 无 truncated 标记 → 正常跳过，不生成投影 action
            }
        }
    }

    if !has_any_directive {
        return PersistedDirectiveRestore::Absent;
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
    // 持久化 directive 无 stale/retention 筛选 → no_op_candidates = 0
    let no_op_candidates = 0;

    // 估算 token（与 plan_micro 保持一致）
    let (before_chars, after_chars) = estimate_projection_chars(transcript, &actions);
    let before = before_chars / 4;
    let after = after_chars / 4;
    let estimated_tokens_saved = before_chars.saturating_sub(after_chars) / 4;

    PersistedDirectiveRestore::Valid(MicroCompactPlan {
        policy_version: expected_version,
        target_reclaim_tokens: 0, // 持久化 directive 不依赖 dynamic config target
        actions,
        estimated_before_tokens: before,
        estimated_after_tokens: after,
        estimated_tokens_saved,
        changed_messages,
        changed_fields,
        no_op_candidates,
    })
}

/// 对指定 actions 列表估算实际会被投影的字符数。
///
/// 仅统计 `CompactToolInput` 指定的顶层 string 字段，以及成功 `ToolResult` 的 text；
/// 找不到目标、不符合类型或 helper 不会缩短时均不计入。
pub(crate) fn estimate_projection_chars(
    transcript: &MessageTranscript,
    actions: &[ProjectionActionEntry],
) -> (u64, u64) {
    let mut before = 0u64;
    let mut after = 0u64;

    for entry in transcript.entries() {
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
        let candidates: Vec<_> = actions
            .iter()
            .filter(|action| action.message_id == message.id())
            .collect();
        let Some(action) = effective_tool_result_action(&candidates) else {
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
        let text = content.text_content();
        if let Some(projected) = apply_head_tail(&text, *keep_head, *keep_tail) {
            before += text.chars().count() as u64;
            after += projected.chars().count() as u64;
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

// ─── render_llm_view ──────────────────────────────────────────────────────────

/// 根据 plan 和 provider 能力渲染 LLM 可见消息列表。
///
/// 纯函数：不修改 transcript，不写 flags，不调数据库。
pub fn render_llm_view(
    transcript: &MessageTranscript,
    plan: &MicroCompactPlan,
    caps: &ProviderCapabilities,
) -> AgentResult<Vec<BaseMessage>> {
    // 1. 使用与 normal Reason 相同的 canonical projection，确保 reminder 恰好一次且非空。
    let visible = transcript.visible_model_messages()?;

    // 2. 按 message_id 索引 plan.actions
    let mut actions_by_id: HashMap<MessageId, Vec<&ProjectionActionEntry>> = HashMap::new();
    for action in &plan.actions {
        actions_by_id
            .entry(action.message_id)
            .or_default()
            .push(action);
    }

    // 3. 逐消息投影
    let mut projected = Vec::with_capacity(visible.len());
    for msg in &visible {
        let id = msg.id();
        match actions_by_id.get(&id) {
            Some(entries) => {
                projected.push(project_message(msg, entries, caps));
            }
            None => {
                // 没有 action → 原样保留
                projected.push((*msg).clone());
            }
        }
    }

    // 4. 验证
    validate_projected_view(&projected, caps)?;

    Ok(projected)
}

// ─── project_message ──────────────────────────────────────────────────────────

/// 对单条消息应用投影 action
fn project_message(
    msg: &BaseMessage,
    entries: &[&ProjectionActionEntry],
    caps: &ProviderCapabilities,
) -> BaseMessage {
    // 按 target 分类 actions
    let mut block_actions: HashMap<usize, &ProjectionActionEntry> = HashMap::new();

    for e in entries {
        match &e.target {
            ProjectionTarget::Message => {}
            ProjectionTarget::ContentBlock { index } => {
                block_actions.insert(*index, e);
            }
            ProjectionTarget::ToolCall { .. } => {}
        }
    }

    match msg {
        // Human/System 消息不做消息级投影，但 ContentBlock 级的 ReplaceMedia 仍需应用
        // （移除 Base64 payload，保留占位符）
        BaseMessage::Human { id, content } => {
            if block_actions.is_empty() {
                return msg.clone();
            }
            let projected_content = project_content(content, &block_actions, caps);
            BaseMessage::Human {
                id: *id,
                content: projected_content,
            }
        }
        BaseMessage::System { id, content } => {
            if block_actions.is_empty() {
                return msg.clone();
            }
            let projected_content = project_content(content, &block_actions, caps);
            BaseMessage::System {
                id: *id,
                content: projected_content,
            }
        }

        BaseMessage::Ai {
            id,
            content,
            tool_calls,
        } => {
            // ToolCall 与 ToolUse 是 canonical execution data。无论 plan 中包含何种
            // legacy/非法组合，renderer 都只允许投影独立的非 ToolUse content block。
            let projected_content = project_content(content, &block_actions, caps);

            BaseMessage::Ai {
                id: *id,
                content: projected_content,
                tool_calls: tool_calls.clone(),
            }
        }

        BaseMessage::Tool {
            id,
            tool_call_id,
            content,
            is_error,
        } => {
            if *is_error {
                return msg.clone(); // 错误结果不变
            }

            // Message 与 block 0 是同一逻辑 ToolResult 文本；重复或冲突时
            // fail-closed，不执行任何投影。estimator 复用同一选择函数。
            let content_action =
                effective_tool_result_action(entries).unwrap_or(&ProjectionAction::Keep);

            let projected_content = project_tool_result_content(content, content_action);

            BaseMessage::Tool {
                id: *id,
                tool_call_id: tool_call_id.clone(),
                content: projected_content,
                is_error: *is_error,
            }
        }
    }
}

// ─── project_content ──────────────────────────────────────────────────────────

/// 对 MessageContent 中的每个 ContentBlock 应用对应 action
fn project_content(
    content: &MessageContent,
    block_actions: &HashMap<usize, &ProjectionActionEntry>,
    caps: &ProviderCapabilities,
) -> MessageContent {
    let blocks = content.content_blocks();
    if blocks.is_empty() {
        return content.clone();
    }

    let mut projected_blocks = Vec::with_capacity(blocks.len());

    for (i, block) in blocks.iter().enumerate() {
        let action = block_actions.get(&i).map(|a| &a.action);
        projected_blocks.push(project_block(block, action, caps));
    }

    // 保留原始 variant：原先是 Text → 保持 Text（但已被截断处理），
    // 原先是 Blocks → 保持 Blocks
    match content {
        MessageContent::Text(_) => {
            // Text 消息只有一个块（在 content_blocks() 中展开为单个 Text block）
            // 截断已在 project_block 中处理
            if projected_blocks.len() == 1 {
                if let ContentBlock::Text { ref text } = projected_blocks[0] {
                    return MessageContent::text(text.clone());
                }
            }
            MessageContent::Blocks(projected_blocks)
        }
        MessageContent::Blocks(_) => MessageContent::Blocks(projected_blocks),
        MessageContent::Raw(_) => {
            // Raw 内容无法逐块投影——原样保留
            content.clone()
        }
    }
}

/// 对 tool result 的完整文本流应用 CompactToolResult action。
fn project_tool_result_content(
    content: &MessageContent,
    action: &ProjectionAction,
) -> MessageContent {
    let ProjectionAction::CompactToolResult {
        keep_head,
        keep_tail,
        ..
    } = action
    else {
        return content.clone();
    };

    let text = content.text_content();
    apply_head_tail(&text, *keep_head, *keep_tail)
        .map(MessageContent::text)
        .unwrap_or_else(|| content.clone())
}

// ─── project_block ────────────────────────────────────────────────────────────

/// 投影单个 ContentBlock
fn project_block(
    block: &ContentBlock,
    action: Option<&ProjectionAction>,
    _caps: &ProviderCapabilities,
) -> ContentBlock {
    if matches!(block, ContentBlock::ToolUse { .. }) {
        return block.clone();
    }

    match action {
        None | Some(ProjectionAction::Keep) => block.clone(),

        Some(ProjectionAction::ReplaceMedia { placeholder }) => match block {
            ContentBlock::Image { .. } => ContentBlock::Text {
                text: format!("[图片已压缩: {}]", placeholder),
            },
            ContentBlock::Document { title, .. } => ContentBlock::Text {
                text: format!(
                    "[文档已压缩{}: {}]",
                    title
                        .as_ref()
                        .map(|t| format!(" ({})", t))
                        .unwrap_or_default(),
                    placeholder
                ),
            },
            _ => block.clone(),
        },

        Some(ProjectionAction::CompactToolResult {
            keep_head,
            keep_tail,
            ..
        }) => match block {
            ContentBlock::Text { text } => apply_head_tail(text, *keep_head, *keep_tail)
                .map(|text| ContentBlock::Text { text })
                .unwrap_or_else(|| block.clone()),
            // Image/Document 在 tool result 中不常见，保留原样
            _ => block.clone(),
        },

        Some(ProjectionAction::Exclude) => ContentBlock::Text {
            text: "[已排除]".to_string(),
        },

        Some(ProjectionAction::CompactText { max_chars }) => match block {
            ContentBlock::Text { text } => {
                let chars: Vec<char> = text.chars().collect();
                if chars.len() <= *max_chars {
                    return block.clone();
                }
                let truncated: String = chars[..*max_chars].iter().collect();
                ContentBlock::Text {
                    text: format!("{}\n[内容已压缩]", truncated),
                }
            }
            _ => block.clone(),
        },

        _ => {
            if let ContentBlock::Reasoning {
                ref signature,
                ref text,
            } = block
            {
                if signature.is_some() {
                    tracing::warn!(
                        len = text.chars().count(),
                        "Reasoning block with signature received projection action; \
                         block preserved unchanged because signed reasoning must remain whole"
                    );
                }
            }
            block.clone()
        }
    }
}

// ─── apply_head_tail ──────────────────────────────────────────────────────────

/// 安全的 head/tail 截断（CJK 安全）。
///
/// 仅当截断后的文本确实更短时返回 Some，避免省略标记使短文本膨胀。
fn apply_head_tail(text: &str, head_chars: usize, tail_chars: usize) -> Option<String> {
    let total: usize = text.chars().count();
    if total <= head_chars.saturating_add(tail_chars) {
        return None;
    }

    let head: String = text.chars().take(head_chars).collect();
    let tail: String = text
        .chars()
        .rev()
        .take(tail_chars)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let skipped = total.saturating_sub(head_chars + tail_chars);
    let sentinel = peri_acp_types::sentinel::format_projection_sentinel_v1(skipped);
    let projected = format!("{head}\n{sentinel}\n{tail}");

    (projected.chars().count() < total).then_some(projected)
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
            let blocks = msg.message_content().content_blocks();
            for block in blocks {
                if let ContentBlock::Reasoning { signature, text } = block {
                    if signature.is_some() {
                        // 验证策略：project_block（见下文）对 reasoning 块的非 Keep 动作
                        // 会静默 fallthrough 到 _ => block.clone()，因此此处只做防御性日志；
                        // 实际安全由 provider adapter 的签名校验保证。
                        tracing::debug!(
                            len = text.chars().count(),
                            "已投影视图中出现带签名的 reasoning block"
                        );
                    }
                }
            }
        }
    }

    Ok(())
}
