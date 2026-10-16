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
/// 需要收益口径时走 [`estimate_projection_chars`]（Compact 决策路径）。
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
    let visible = transcript.visible_messages();
    let mut actions = Vec::new();
    let mut has_any_directive = false;

    for msg in &visible {
        let id = msg.id();
        // 只读查询：不克隆 projection directive 本体
        let projection = transcript.projection_ref(id);
        let truncated = transcript.is_truncated(id);

        match projection {
            Some(directive) => {
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

/// Reason 与 Full 共用的已提交模型视图；无效投影保留 canonical 内容。
pub(crate) fn render_persisted_llm_view(
    transcript: &MessageTranscript,
    caps: &ProviderCapabilities,
) -> AgentResult<Vec<BaseMessage>> {
    match plan_from_persisted_directives(transcript, PROJECTION_POLICY_VERSION) {
        PersistedDirectiveRestore::Valid(restored) => {
            match render_llm_view(transcript, &restored.actions, caps) {
                Ok(view) => return crate::messages::tool_pairing::repair_model_tool_pairing(view),
                Err(error) => tracing::warn!(%error, "已提交投影渲染失败，保留 canonical 内容"),
            }
        }
        PersistedDirectiveRestore::Invalid => {
            tracing::warn!("持久化 directive 无效；保留 canonical 内容，不重新规划");
        }
        PersistedDirectiveRestore::Absent => {}
    }
    crate::messages::tool_pairing::repair_model_tool_pairing(transcript.visible_model_messages()?)
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
    // 1. 使用与 normal Reason 相同的 canonical projection，确保 reminder 恰好一次且非空。
    let visible = transcript.visible_model_messages()?;

    // 2. 消费 owned 消息：无 action 的消息直接移动，不做第二次正文复制。
    let mut actions_by_id: HashMap<MessageId, Vec<&ProjectionActionEntry>> = HashMap::new();
    for action in actions {
        actions_by_id
            .entry(action.message_id)
            .or_default()
            .push(action);
    }

    let mut projected = Vec::with_capacity(visible.len());
    for msg in visible {
        match actions_by_id.get(&msg.id()) {
            Some(entries) => projected.push(project_message(msg, entries, caps)),
            None => projected.push(msg),
        }
    }

    // 3. 验证
    validate_projected_view(&projected, caps)?;

    Ok(projected)
}

// ─── project_message ──────────────────────────────────────────────────────────

/// 对单条消息应用投影 action。
///
/// 消费 owned 消息：不需要投影的分支直接移动原对象，需要投影的分支才重建内容。
fn project_message(
    msg: BaseMessage,
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
                return BaseMessage::Human { id, content };
            }
            BaseMessage::Human {
                id,
                content: project_content(&content, &block_actions, caps),
            }
        }
        BaseMessage::System { id, content } => {
            if block_actions.is_empty() {
                return BaseMessage::System { id, content };
            }
            BaseMessage::System {
                id,
                content: project_content(&content, &block_actions, caps),
            }
        }

        BaseMessage::Ai {
            id,
            content,
            tool_calls,
        } => {
            // ToolCall 与 ToolUse 是 canonical execution data。无论 plan 中包含何种
            // legacy/非法组合，renderer 都只允许投影独立的非 ToolUse content block。
            // 无 block action 时内容不变，直接移动（不改写正文与 tool_calls）。
            if block_actions.is_empty() {
                return BaseMessage::Ai {
                    id,
                    content,
                    tool_calls,
                };
            }
            BaseMessage::Ai {
                id,
                content: project_content(&content, &block_actions, caps),
                tool_calls,
            }
        }

        BaseMessage::Tool {
            id,
            tool_call_id,
            content,
            is_error,
            execution,
            subagent_failure,
        } => {
            // 消息级 action 只作用于成功结果；错误结果保持不变。
            let content = if is_error {
                content
            } else {
                // Message 与 block 0 是同一逻辑 ToolResult 文本；重复或冲突时
                // fail-closed，不执行任何投影。estimator 复用同一选择函数。
                let content_action =
                    effective_tool_result_action(entries).unwrap_or(&ProjectionAction::Keep);
                project_tool_result_content(content, content_action)
            };

            BaseMessage::Tool {
                id,
                tool_call_id,
                content,
                is_error,
                execution,
                subagent_failure,
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
///
/// 消费 owned 内容：不需要截断时原样返回（不复制正文）。
fn project_tool_result_content(content: MessageContent, action: &ProjectionAction) -> MessageContent {
    let ProjectionAction::CompactToolResult {
        keep_head,
        keep_tail,
        ..
    } = action
    else {
        return content;
    };

    match content {
        // 纯文本是常见情形：直接切片截断，避免拼接后再复制。
        MessageContent::Text(text) => match apply_head_tail(&text, *keep_head, *keep_tail) {
            Some(projected) => MessageContent::text(projected),
            None => MessageContent::Text(text),
        },
        other => {
            let text = other.text_content();
            match apply_head_tail(&text, *keep_head, *keep_tail) {
                Some(projected) => MessageContent::text(projected),
                None => other,
            }
        }
    }
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
    let (_, projected) = head_tail_geometry(total, head_chars, tail_chars)?;

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
    let projected_text = format!("{head}\n{sentinel}\n{tail}");
    debug_assert_eq!(
        projected_text.chars().count(),
        projected,
        "head/tail 几何与实际渲染必须一致"
    );

    Some(projected_text)
}

/// head/tail 投影的几何：`(原始字符数, 投影后字符数)`；`None` 表示不缩短。
///
/// 与 [`apply_head_tail`] 共用同一几何规则（sentinel 由同一格式化函数给出），
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

/// 统计消息正文中参与 tool result 投影的字符数（不复制正文）。
///
/// 与 [`apply_head_tail`] 的输入口径一致：`Text` 直接计数，`Blocks` / `Raw`
/// 按 text block 拼接后计数（拼接结果只用于计数，不驻留）。
fn projected_text_char_count(content: &MessageContent) -> usize {
    match content {
        MessageContent::Text(text) => text.chars().count(),
        other => other.text_content().chars().count(),
    }
}

/// 单条 tool result 的投影字符数 `(before, after)`；`None` 表示投影不缩短。
fn projection_char_counts(
    content: &MessageContent,
    head_chars: usize,
    tail_chars: usize,
) -> Option<(usize, usize)> {
    head_tail_geometry(projected_text_char_count(content), head_chars, tail_chars)
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
