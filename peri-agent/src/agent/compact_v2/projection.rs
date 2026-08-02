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

pub const PROJECTION_POLICY_VERSION: u32 = 2;

const fn default_compact_tool_input_keep_head() -> usize {
    350
}

const fn default_compact_tool_input_keep_tail() -> usize {
    100
}

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
    /// 按字典序持久化的 root JSON object 顶层 key。
    ///
    /// 未知字段、非 string 值及非 object 根类型均安全地不执行任何操作；不支持嵌套路径、
    /// JSON Pointer 或数组索引。
    CompactToolInput {
        fields: Vec<String>,
        #[serde(default = "default_compact_tool_input_keep_head")]
        keep_head: usize,
        #[serde(default = "default_compact_tool_input_keep_tail")]
        keep_tail: usize,
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
                            entry.message_id,
                            id
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

    Ok(MicroCompactPlan {
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

    for action in actions {
        let Some(message) = transcript
            .entries()
            .iter()
            .find(|entry| entry.message.id() == action.message_id)
            .map(|entry| &entry.message)
        else {
            continue;
        };

        match (&action.target, &action.action, message) {
            (
                ProjectionTarget::ToolCall { tool_call_id },
                ProjectionAction::CompactToolInput {
                    fields,
                    keep_head,
                    keep_tail,
                },
                BaseMessage::Ai { tool_calls, .. },
            ) => {
                let Some(tool_call) = tool_calls.iter().find(|tc| tc.id == *tool_call_id) else {
                    continue;
                };
                let Some(arguments) = tool_call.arguments.as_object() else {
                    continue;
                };

                // fields 空 = 整条压缩兜底：按整条 arguments 序列化长度估算，
                // after 计入占位 JSON 的实际渲染长度（与 project_tool_input 共用同一占位）
                if fields.is_empty() {
                    let json = serde_json::to_string(arguments).unwrap_or_default();
                    before += json.chars().count() as u64;
                    after += serde_json::to_string(&compact_note_placeholder())
                        .unwrap_or_default()
                        .chars()
                        .count() as u64;
                    continue;
                }

                let mut seen_fields = HashSet::new();
                for field in fields {
                    if !seen_fields.insert(field) {
                        continue;
                    }
                    let Some(text) = arguments.get(field).and_then(serde_json::Value::as_str)
                    else {
                        continue;
                    };
                    if let Some(projected) = apply_head_tail(text, *keep_head, *keep_tail) {
                        before += text.chars().count() as u64;
                        after += projected.chars().count() as u64;
                    }
                }
            }
            (
                ProjectionTarget::Message | ProjectionTarget::ContentBlock { index: 0 },
                ProjectionAction::CompactToolResult {
                    keep_head,
                    keep_tail,
                    ..
                },
                BaseMessage::Tool {
                    content, is_error, ..
                },
            ) if !is_error => {
                let text = content.text_content();
                if let Some(projected) = apply_head_tail(&text, *keep_head, *keep_tail) {
                    before += text.chars().count() as u64;
                    after += projected.chars().count() as u64;
                }
            }
            _ => {}
        }
    }

    (before, after)
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
            ContentBlock::ToolUse { id, .. } => {
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

// ─── project_tool_input ───────────────────────────────────────────────────────

/// 空 fields 整条压缩兜底的占位 arguments（`{"_compact_note":"tool input compacted"}`）。
///
/// 估算（`estimate_projection_chars`）与渲染共用此构造，保证占位长度与内容不漂移。
fn compact_note_placeholder() -> serde_json::Value {
    let mut minimal = serde_json::Map::new();
    minimal.insert(
        "_compact_note".to_string(),
        serde_json::Value::String("tool input compacted".to_string()),
    );
    serde_json::Value::Object(minimal)
}

/// 投影 tool input
fn project_tool_input(tc: &ToolCallRequest, action: &ProjectionActionEntry) -> ToolCallRequest {
    match &action.action {
        ProjectionAction::CompactToolInput {
            fields,
            keep_head,
            keep_tail,
        } => {
            let Some(arguments) = tc.arguments.as_object() else {
                return tc.clone();
            };
            // fields 空 = 整条压缩兜底：替换为 minimal 占位（旧版 preserve_shape 语义）。
            // 由 planner 在无超长字段时生成，保证普通工具调用也会被压缩。
            if fields.is_empty() {
                return ToolCallRequest {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    arguments: compact_note_placeholder(),
                };
            }
            let mut projected_arguments = arguments.clone();
            let mut changed = false;

            for field in fields {
                let Some(text) = arguments.get(field).and_then(serde_json::Value::as_str) else {
                    continue;
                };
                let Some(truncated) = apply_head_tail(text, *keep_head, *keep_tail) else {
                    continue;
                };
                projected_arguments.insert(field.clone(), serde_json::Value::String(truncated));
                changed = true;
            }

            if changed {
                ToolCallRequest {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    arguments: serde_json::Value::Object(projected_arguments),
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
    let projected = format!("{}\n... [{} 字符已省略] ...\n{}", head, skipped, tail);

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
