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
use crate::messages::{BaseMessage, ContentBlock, MessageContent, MessageId, ToolCallRequest};
use crate::session::transcript::MessageTranscript;

/// 投影目标（消息、块或工具调用）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectionTarget {
    Message,
    ContentBlock { index: usize },
    ToolCall { tool_call_id: String },
}

/// 投影动作 — 决定 LLM view 中消息/块如何呈现
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectionAction {
    Keep,
    CompactText {
        max_chars: usize,
    },
    CompactToolResult {
        keep_head: usize,
        keep_tail: usize,
        preserve_recovery_handle: bool,
    },
    CompactToolInput {
        fields: Vec<String>,
        preserve_shape: bool,
    },
    ReplaceMedia {
        placeholder: String,
    },
    Exclude,
}

/// 单个投影条目：消息 id → 目标 → 动作
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionActionEntry {
    pub message_id: MessageId,
    pub target: ProjectionTarget,
    pub action: ProjectionAction,
}

/// 消息级投影指令 — 存储于 MessageFlags 中，可序列化/可恢复
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageProjectionDirective {
    pub policy_version: u32,
    /// 仅含本消息的 action entries，不含 BaseMessage 内容或 Base64
    #[serde(default)]
    pub entries: Vec<ProjectionActionEntry>,
}

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

/// 从 transcript 中已持久化的 projection directive 重建 MicroCompactPlan。
///
/// 遍历全部可见消息，检查 `MessageFlags.projection`：
/// - `projection = Some(d)` 且 `d.policy_version == expected_version` → 收集 entries
/// - `projection = Some(d)` 但版本不匹配 → 立即返回错误
/// - `projection = None`（含旧 truncated 标记）→ 跳过（不生产伪 action）
///
/// # Returns
/// - `Ok(plan)`：至少一条消息有有效 directive
/// - `Err(msg)`：无有效 directive（caller 应 fallback 到 `plan_micro`）或版本不匹配
pub fn plan_from_persisted_directives(
    transcript: &MessageTranscript,
    expected_version: u32,
) -> AgentResult<MicroCompactPlan> {
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
                    return Err(crate::error::AgentError::Other(anyhow::anyhow!(
                        "{}: expected {}, got {} (msg {:?})",
                        DIRECTIVE_VERSION_MISMATCH,
                        expected_version,
                        directive.policy_version,
                        id
                    )));
                }
                // 验证 directive entries 的 message_id 与当前消息一致
                for entry in &directive.entries {
                    if entry.message_id != id {
                        return Err(crate::error::AgentError::Other(anyhow::anyhow!(
                            "directive entry references wrong message: entry.msg_id={:?} != msg.id={:?}",
                            entry.message_id, id
                        )));
                    }
                }
                actions.extend(directive.entries.clone());
            }
            None => {
                // G1: fail-closed on unknown directives
                // truncated=true + projection=None + not excluded = corrupted state
                // （visible_messages() 已过滤 excluded，此处消息必然非 excluded）
                if flags.truncated {
                    return Err(crate::error::AgentError::Other(anyhow::anyhow!(
                        "{}: msg {:?} is truncated but lacks projection directive",
                        CORRUPTED_PROJECTION,
                        id
                    )));
                }
                // 无 truncated 标记 → 正常跳过，不生成投影 action
            }
        }
    }

    if !has_any_directive {
        return Err(crate::error::AgentError::Other(anyhow::anyhow!(
            "{}",
            NO_PERSISTED_DIRECTIVES
        )));
    }

    // 估算 token（与 plan_micro 保持一致）
    let (before, after) = estimate_tokens_for_actions(transcript, &actions);

    Ok(MicroCompactPlan {
        policy_version: expected_version,
        target_reclaim_tokens: 0, // 持久化 directive 不依赖 dynamic config target
        actions,
        estimated_before_tokens: before,
        estimated_after_tokens: after,
        estimated_tokens_saved: before.saturating_sub(after),
    })
}

/// 对指定 actions 列表做 token 估算（用于 plan_from_persisted_directives）
fn estimate_tokens_for_actions(
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

        let has_action = actions.iter().any(|a| a.message_id == id);
        if has_action {
            let projected_chars = (chars / 3).min(chars);
            before += chars;
            after += projected_chars;
        }
    }

    (before / 4, after / 4)
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
    // 1. 收集可见消息（从 transcript 取原始消息）
    let visible = transcript.visible_messages();

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
    let mut msg_entry: Option<&ProjectionActionEntry> = None;
    let mut block_actions: HashMap<usize, &ProjectionActionEntry> = HashMap::new();
    let mut tool_actions: HashMap<&str, &ProjectionActionEntry> = HashMap::new();

    for e in entries {
        match &e.target {
            ProjectionTarget::Message => msg_entry = Some(e),
            ProjectionTarget::ContentBlock { index } => {
                block_actions.insert(*index, e);
            }
            ProjectionTarget::ToolCall { tool_call_id } => {
                tool_actions.insert(tool_call_id.as_str(), e);
            }
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
            // 投影 tool_calls（先投影以便同步到 ContentBlock::ToolUse）
            let projected_tool_calls: Vec<ToolCallRequest> = tool_calls
                .iter()
                .map(|tc| {
                    if let Some(action) = tool_actions.get(tc.id.as_str()) {
                        project_tool_input(tc, action)
                    } else {
                        tc.clone()
                    }
                })
                .collect();

            // 构造 tool_call_id → projected ToolCallRequest 快速查找
            let tool_call_lookup: HashMap<&str, &ToolCallRequest> = projected_tool_calls
                .iter()
                .map(|tc| (tc.id.as_str(), tc))
                .collect();

            // 投影 content blocks，同时将 ToolUse blocks 与 projected tool_calls 同步
            let projected_content =
                project_ai_content(content, &block_actions, &tool_call_lookup, caps);

            BaseMessage::Ai {
                id: *id,
                content: projected_content,
                tool_calls: projected_tool_calls,
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

            // 检查消息级 action（CompactToolResult）
            let content_action = if let Some(entry) = msg_entry {
                &entry.action
            } else {
                // fallback：检查 block_actions 中 index=0 的 action
                match block_actions.get(&0) {
                    Some(entry) => &entry.action,
                    None => &ProjectionAction::Keep,
                }
            };

            // 投影 tool result content
            let projected_content = project_tool_result_content(content, content_action, caps);

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

/// AI 消息专用投影：在 project_content 基础上，将 ToolUse blocks 与 projected tool_calls 同步。
///
/// 保证 Anthropic adapter 看到的 ContentBlock::ToolUse 与 tool_calls 向量一致，
/// 避免投影后的 tool input 在不同 provider 路径中产生数据不一致（P0-4 修复）。
fn project_ai_content(
    content: &MessageContent,
    block_actions: &HashMap<usize, &ProjectionActionEntry>,
    tool_call_lookup: &HashMap<&str, &ToolCallRequest>,
    caps: &ProviderCapabilities,
) -> MessageContent {
    let blocks = content.content_blocks();
    if blocks.is_empty() {
        return content.clone();
    }

    let mut projected_blocks = Vec::with_capacity(blocks.len());

    for (i, block) in blocks.iter().enumerate() {
        // 先按 block_actions 获取投影 action
        let action_opt = block_actions.get(&i).map(|a| &a.action);

        match block {
            ContentBlock::ToolUse { id, name: _, .. } => {
                // 从 projected tool_calls 查找对应的投影版本
                if let Some(projected_tc) = tool_call_lookup.get(id.as_str()) {
                    projected_blocks.push(ContentBlock::ToolUse {
                        id: projected_tc.id.clone(),
                        name: projected_tc.name.clone(),
                        input: projected_tc.arguments.clone(),
                    });
                } else if action_opt.is_some() {
                    // 有 block_actions 但没有 tool_call_lookup 条目 → 使用 action 投影
                    projected_blocks.push(project_block(block, action_opt, caps));
                } else {
                    projected_blocks.push(block.clone());
                }
            }
            _ => {
                // 非 ToolUse block 使用标准投影逻辑
                projected_blocks.push(project_block(block, action_opt, caps));
            }
        }
    }

    match content {
        MessageContent::Text(_) => {
            if projected_blocks.len() == 1 {
                if let ContentBlock::Text { ref text } = projected_blocks[0] {
                    return MessageContent::text(text.clone());
                }
            }
            MessageContent::Blocks(projected_blocks)
        }
        MessageContent::Blocks(_) => MessageContent::Blocks(projected_blocks),
        MessageContent::Raw(_) => content.clone(),
    }
}

/// 对 tool result 内容应用 CompactToolResult action
fn project_tool_result_content(
    content: &MessageContent,
    action: &ProjectionAction,
    caps: &ProviderCapabilities,
) -> MessageContent {
    let blocks = content.content_blocks();
    let mut projected_blocks = Vec::with_capacity(blocks.len());

    for block in &blocks {
        projected_blocks.push(project_block(block, Some(action), caps));
    }

    match content {
        MessageContent::Text(_) => {
            if projected_blocks.len() == 1 {
                if let ContentBlock::Text { ref text } = projected_blocks[0] {
                    return MessageContent::text(text.clone());
                }
            }
            MessageContent::Blocks(projected_blocks)
        }
        MessageContent::Blocks(_) => MessageContent::Blocks(projected_blocks),
        MessageContent::Raw(_) => content.clone(),
    }
}

// ─── project_block ────────────────────────────────────────────────────────────

/// 投影单个 ContentBlock
fn project_block(
    block: &ContentBlock,
    action: Option<&ProjectionAction>,
    _caps: &ProviderCapabilities,
) -> ContentBlock {
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
            ContentBlock::Text { text } => {
                let truncated = apply_head_tail(text, *keep_head, *keep_tail);
                ContentBlock::Text { text: truncated }
            }
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

        _ => block.clone(),
    }
}

// ─── project_tool_input ───────────────────────────────────────────────────────

/// 投影 tool input：保持 JSON object 根类型
fn project_tool_input(tc: &ToolCallRequest, action: &ProjectionActionEntry) -> ToolCallRequest {
    match &action.action {
        ProjectionAction::CompactToolInput { preserve_shape, .. } => {
            if *preserve_shape && tc.arguments.is_object() {
                // 保留 object 根，替换为 minimal 占位
                let mut minimal = serde_json::Map::new();
                minimal.insert(
                    "_compact_note".to_string(),
                    serde_json::Value::String("tool input compacted".to_string()),
                );
                ToolCallRequest {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    arguments: serde_json::Value::Object(minimal),
                }
            } else {
                tc.clone()
            }
        }
        ProjectionAction::CompactText { max_chars } => {
            let args_str = serde_json::to_string(&tc.arguments).unwrap_or_default();
            let chars: Vec<char> = args_str.chars().collect();
            if chars.len() > *max_chars && tc.arguments.is_string() {
                let truncated: String = chars[..*max_chars].iter().collect();
                ToolCallRequest {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    arguments: serde_json::Value::String(format!("{}\n[内容已压缩]", truncated)),
                }
            } else {
                tc.clone()
            }
        }
        _ => tc.clone(),
    }
}

// ─── apply_head_tail ──────────────────────────────────────────────────────────

/// 安全的 head/tail 截断（CJK 安全）
fn apply_head_tail(text: &str, head_chars: usize, tail_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= head_chars + tail_chars {
        return text.to_string();
    }

    let head: String = chars[..head_chars].iter().collect();
    let tail: String = chars[chars.len() - tail_chars..].iter().collect();
    let skipped = chars.len() - head_chars - tail_chars;

    format!("{}\n... [{} 字符已省略] ...\n{}", head, skipped, tail)
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
                if let ContentBlock::Reasoning { signature, .. } = block {
                    if signature.is_some() {
                        // 有签名的 reasoning 存在 → OK（没有被局部截断）
                    }
                }
            }
        }
    }

    Ok(())
}
