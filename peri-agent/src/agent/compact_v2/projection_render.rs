//! Projection render — 借用式消息/内容构造
//!
//! 从 `projection.rs` 拆出的消息构造管线：`project_message_ref` 与 `MessageDraft`
//! 只在确定输出时复制正文，`RenderAction` 承载已解析的归档 payload。模块私有于
//! `projection`，由 `projection_archive` 调用；编排与对外入口留在父模块。

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use crate::error::AgentResult;
use crate::messages::{BaseMessage, ContentBlock, MessageContent, MessageId, ToolCallRequest};

use super::{
    effective_tool_result_action, head_tail_geometry, invalid, ProjectionAction,
    ProjectionActionEntry, ProjectionTarget, ProviderCapabilities, ProviderProtocol,
};

// ─── project_message_ref ──────────────────────────────────────────────────────

/// One archive action whose catalog/identity payload is already resolved.
///
/// Catalog lookups and reference validation stay in `projection_archive`; the
/// borrowed message pipeline only applies validated payloads. Keeping them
/// ordered preserves the renderer's Preview/Receipt → Reference sequence.
pub(super) enum RenderAction {
    /// `PreviewArchiveRead`: whole-result head/tail preview plus recovery suffix.
    Preview {
        action: ProjectionAction,
        suffix: String,
    },
    /// `ResultReceipt`: replace a whole result or one inline result block.
    Receipt {
        target: ProjectionTarget,
        text: String,
        receipt_id: MessageId,
    },
    /// Recovery handle appended to an already projected block/message.
    Reference {
        target: ProjectionTarget,
        suffix: String,
    },
}

/// Borrowed analogue of the former owned `project_message` pipeline.
///
/// 正文只在确定要输出时复制一次：整条省略的独立工具结果不构造消息；混合消息先按
/// 原始 block index 应用 ordinary/Preview/Receipt，再仅复制存活兄弟块。
/// `None` 表示整条消息被省略。
pub(super) fn project_message_ref(
    message: &BaseMessage,
    ordinary: &[&ProjectionActionEntry],
    legacy: bool,
    actions: &[RenderAction],
    removed: &HashSet<String>,
    caps: &ProviderCapabilities,
) -> AgentResult<Option<BaseMessage>> {
    // Replay state and its visible siblings form one assistant output unit.
    // The provider validates the same invariant at the wire boundary, but a
    // stale/persisted projection must fail here before a request is attempted.
    let atomic_replay = message.message_content().has_provider_state();
    // A standalone archived result carries one logical body. Drop it before any
    // projection work copies that body; special actions still take the ordered
    // path so their validation errors stay unchanged.
    if actions.is_empty()
        && matches!(message, BaseMessage::Tool { tool_call_id, .. } if removed.contains(tool_call_id))
    {
        return Ok(None);
    }

    let mut draft = MessageDraft::new(message);
    if legacy {
        if let Some(content) = legacy_result_content(message, ordinary) {
            draft.set_content(content);
        }
    } else {
        draft.apply_ordinary(ordinary, caps);
    }
    for action in actions {
        match action {
            RenderAction::Preview { action, suffix } => {
                if !matches!(message, BaseMessage::Tool { .. }) {
                    return Err(invalid("archive read preview requires standalone result"));
                }
                let content = project_tool_result_content_ref(draft.content(), action);
                draft.set_content(content);
                draft.append_reference(&ProjectionTarget::Message, suffix)?;
            }
            RenderAction::Receipt {
                target,
                text,
                receipt_id,
            } => {
                draft.replace_result(target, text.clone(), *receipt_id)?;
            }
            RenderAction::Reference { target, suffix } => {
                draft.append_reference(target, suffix)?;
            }
        }
    }
    if atomic_replay
        && message
            .tool_calls()
            .iter()
            .any(|call| removed.contains(&call.id))
    {
        return Err(invalid("provider replay unit cannot omit tool calls"));
    }
    if !atomic_replay && !removed.is_empty() && draft.omit(removed, caps)? {
        return Ok(None);
    }
    let projected = draft.finish();
    if atomic_replay
        && (projected.message_content() != message.message_content()
            || projected.tool_calls() != message.tool_calls())
    {
        return Err(invalid(
            "provider replay unit cannot be partially projected",
        ));
    }
    Ok(Some(projected))
}

/// Copy-on-write message body: fields stay borrowed until an action changes them.
struct MessageDraft<'a> {
    source: &'a BaseMessage,
    id: Option<MessageId>,
    content: Option<MessageContent>,
    tool_calls: Option<Vec<ToolCallRequest>>,
}

impl<'a> MessageDraft<'a> {
    fn new(source: &'a BaseMessage) -> Self {
        Self {
            source,
            id: None,
            content: None,
            tool_calls: None,
        }
    }

    fn content(&self) -> &MessageContent {
        self.content
            .as_ref()
            .unwrap_or_else(|| self.source.message_content())
    }

    fn set_content(&mut self, content: MessageContent) {
        self.content = Some(content);
    }

    fn content_mut(&mut self) -> &mut MessageContent {
        if self.content.is_none() {
            self.content = Some(self.source.message_content().clone());
        }
        self.content.as_mut().expect("content initialized")
    }

    fn tool_calls(&self) -> &[ToolCallRequest] {
        match (&self.tool_calls, self.source) {
            (Some(calls), _) => calls,
            (None, BaseMessage::Ai { tool_calls, .. }) => tool_calls,
            _ => &[],
        }
    }

    fn apply_ordinary(&mut self, entries: &[&ProjectionActionEntry], caps: &ProviderCapabilities) {
        let mut block_actions: HashMap<usize, &ProjectionActionEntry> = HashMap::new();
        for entry in entries {
            if let ProjectionTarget::ContentBlock { index } = entry.target {
                block_actions.insert(index, *entry);
            }
        }

        match self.source {
            BaseMessage::Human { .. } | BaseMessage::System { .. } | BaseMessage::Ai { .. } => {
                if block_actions.is_empty() {
                    return;
                }
                if let Cow::Owned(projected) =
                    project_content_ref(self.content(), &block_actions, caps)
                {
                    self.set_content(projected);
                }
            }
            BaseMessage::Tool { .. } => {
                // Message 与 block 0 是同一逻辑 ToolResult 文本；重复或冲突时
                // fail-closed，不执行任何投影。estimator 复用同一选择函数。
                let content_action =
                    effective_tool_result_action(entries).unwrap_or(&ProjectionAction::Keep);
                if matches!(content_action, ProjectionAction::CompactToolResult { .. }) {
                    let projected = project_tool_result_content_ref(self.content(), content_action);
                    self.set_content(projected);
                }
                // Message/block 0 的 CompactToolResult 已统一处理，不能在冲突时
                // 又通过 block 路径执行，更不能对正常动作重复裁剪。
                block_actions.retain(|index, entry| {
                    !(*index == 0
                        && matches!(entry.action, ProjectionAction::CompactToolResult { .. }))
                });
                if block_actions.is_empty() {
                    return;
                }
                if let Cow::Owned(projected) =
                    project_content_ref(self.content(), &block_actions, caps)
                {
                    self.set_content(projected);
                }
            }
        }
    }

    /// Replace a whole result or one inline result block with receipt facts.
    fn replace_result(
        &mut self,
        target: &ProjectionTarget,
        text: String,
        receipt_id: MessageId,
    ) -> AgentResult<()> {
        match (self.source, target) {
            (BaseMessage::Tool { .. }, ProjectionTarget::Message) => {
                self.id = Some(receipt_id);
                self.content = Some(MessageContent::text(text));
            }
            (
                BaseMessage::Human { .. } | BaseMessage::Ai { .. },
                ProjectionTarget::ContentBlock { index },
            ) => match self.content_mut() {
                MessageContent::Blocks(blocks) => match blocks.get_mut(*index) {
                    Some(ContentBlock::ToolResult { id, content, .. }) => {
                        *id = Some(receipt_id.as_uuid().to_string());
                        *content = vec![ContentBlock::Text { text }];
                    }
                    _ => return Err(invalid("receipt target is not a tool result")),
                },
                _ => return Err(invalid("unsupported receipt target")),
            },
            _ => return Err(invalid("unsupported receipt target")),
        }
        Ok(())
    }

    /// Append a recovery handle without rewriting the canonical block index.
    fn append_reference(&mut self, target: &ProjectionTarget, suffix: &str) -> AgentResult<()> {
        match (self.source, target) {
            (
                BaseMessage::Tool { .. },
                ProjectionTarget::Message | ProjectionTarget::ContentBlock { index: 0 },
            ) => match self.content_mut() {
                MessageContent::Text(text) => text.push_str(suffix),
                MessageContent::Blocks(blocks) => blocks.push(ContentBlock::Text {
                    text: suffix.into(),
                }),
                MessageContent::Raw(_) => {
                    return Err(invalid(
                        "cannot attach recovery handle to unknown raw content",
                    ))
                }
            },
            (
                BaseMessage::Human { .. } | BaseMessage::Ai { .. },
                ProjectionTarget::ContentBlock { index },
            ) => match self.content_mut() {
                MessageContent::Blocks(blocks) => match blocks.get_mut(*index) {
                    Some(ContentBlock::ToolResult { content, .. }) => {
                        content.push(ContentBlock::Text {
                            text: suffix.into(),
                        })
                    }
                    _ => return Err(invalid("recovery handle target is not a tool result")),
                },
                _ => return Err(invalid("unsupported recovery handle target")),
            },
            _ => return Err(invalid("unsupported recovery handle target")),
        }
        Ok(())
    }

    /// Remove archived exchanges from this message; `true` means drop the message.
    fn omit(
        &mut self,
        removed: &HashSet<String>,
        caps: &ProviderCapabilities,
    ) -> AgentResult<bool> {
        let remaining_calls = match self.source {
            BaseMessage::Tool { tool_call_id, .. } => {
                return Ok(removed.contains(tool_call_id));
            }
            BaseMessage::Ai { .. } => {
                let calls = self.tool_calls();
                if calls.iter().any(|call| removed.contains(&call.id)) {
                    let retained: Vec<_> = calls
                        .iter()
                        .filter(|call| !removed.contains(&call.id))
                        .cloned()
                        .collect();
                    let remaining = retained.len();
                    self.tool_calls = Some(retained);
                    remaining
                } else {
                    calls.len()
                }
            }
            BaseMessage::Human { .. } => 0,
            BaseMessage::System { .. } => return Err(invalid("cannot omit system data")),
        };

        self.retain_content(removed, remaining_calls, caps)?;
        Ok(remaining_calls == 0 && self.content().is_empty())
    }

    /// Retain only surviving sibling blocks at their original relative order.
    fn retain_content(
        &mut self,
        removed: &HashSet<String>,
        remaining_calls: usize,
        caps: &ProviderCapabilities,
    ) -> AgentResult<()> {
        if let Some(content) = self.content.as_mut() {
            match content {
                MessageContent::Blocks(blocks) => {
                    validate_block_omission(blocks, remaining_calls, caps)?;
                    blocks.retain(|block| !omitted_block(block, removed, remaining_calls));
                }
                MessageContent::Raw(blocks) => {
                    validate_raw_omission(blocks)?;
                    blocks.retain(|block| !omitted_raw_block(block, removed));
                }
                MessageContent::Text(_) => {}
            }
            return Ok(());
        }

        // Borrowed content: copy only the surviving blocks.
        match self.source.message_content() {
            MessageContent::Blocks(blocks) => {
                validate_block_omission(blocks, remaining_calls, caps)?;
                let retained = blocks
                    .iter()
                    .filter(|block| !omitted_block(block, removed, remaining_calls))
                    .cloned()
                    .collect();
                self.set_content(MessageContent::Blocks(retained));
            }
            MessageContent::Raw(blocks) => {
                validate_raw_omission(blocks)?;
                let retained = blocks
                    .iter()
                    .filter(|block| !omitted_raw_block(block, removed))
                    .cloned()
                    .collect();
                self.set_content(MessageContent::Raw(retained));
            }
            MessageContent::Text(_) => {}
        }
        Ok(())
    }

    fn finish(self) -> BaseMessage {
        let Self {
            source,
            id,
            content,
            tool_calls,
        } = self;
        let id = id.unwrap_or_else(|| source.id());
        let content = content.unwrap_or_else(|| source.message_content().clone());
        match source {
            BaseMessage::Human { .. } => BaseMessage::Human { id, content },
            BaseMessage::System { .. } => BaseMessage::System { id, content },
            BaseMessage::Ai { .. } => BaseMessage::Ai {
                id,
                content,
                tool_calls: tool_calls.unwrap_or_else(|| source.tool_calls().to_vec()),
            },
            BaseMessage::Tool {
                tool_call_id,
                is_error,
                execution,
                subagent_failure,
                ..
            } => BaseMessage::Tool {
                id,
                tool_call_id: tool_call_id.clone(),
                content,
                is_error: *is_error,
                execution: execution.clone(),
                subagent_failure: subagent_failure.clone(),
            },
        }
    }
}

fn validate_block_omission(
    blocks: &[ContentBlock],
    remaining_calls: usize,
    caps: &ProviderCapabilities,
) -> AgentResult<()> {
    if blocks
        .iter()
        .any(|block| matches!(block, ContentBlock::Unknown(_)))
    {
        return Err(invalid("unknown protocol dependency"));
    }
    let signed = blocks.iter().any(|block| {
        matches!(
            block,
            ContentBlock::Reasoning {
                signature: Some(_),
                ..
            }
        )
    });
    if signed && (remaining_calls > 0 || caps.protocol == ProviderProtocol::Generic) {
        return Err(invalid("signed reasoning closure cannot be split"));
    }
    Ok(())
}

fn omitted_block(block: &ContentBlock, removed: &HashSet<String>, remaining_calls: usize) -> bool {
    match block {
        ContentBlock::ToolUse { id, .. } => removed.contains(id),
        ContentBlock::ToolResult { tool_use_id, .. } => removed.contains(tool_use_id),
        // 与基线 retain 语义一致：仅当交换内已无剩余调用时省略思考块；
        // 仍有调用时保留，以维持 provider 的思考/调用闭包。
        ContentBlock::Reasoning { .. } => remaining_calls == 0,
        _ => false,
    }
}

fn validate_raw_omission(blocks: &[serde_json::Value]) -> AgentResult<()> {
    if blocks.iter().any(|block| {
        !matches!(
            block["type"].as_str(),
            Some("text" | "tool_use" | "tool_result")
        )
    }) {
        return Err(invalid("unrecognized raw protocol dependency"));
    }
    Ok(())
}

fn omitted_raw_block(block: &serde_json::Value, removed: &HashSet<String>) -> bool {
    match block["type"].as_str() {
        Some("tool_use") => block["id"].as_str().is_some_and(|id| removed.contains(id)),
        Some("tool_result") => block["tool_use_id"]
            .as_str()
            .is_some_and(|id| removed.contains(id)),
        _ => false,
    }
}

/// v2 legacy directive semantics: only a successful standalone tool result is
/// truncated; block actions do not apply.
fn legacy_result_content(
    message: &BaseMessage,
    entries: &[&ProjectionActionEntry],
) -> Option<MessageContent> {
    let BaseMessage::Tool {
        content,
        is_error: false,
        ..
    } = message
    else {
        return None;
    };
    let ProjectionAction::CompactToolResult {
        keep_head,
        keep_tail,
        ..
    } = effective_tool_result_action(entries)?
    else {
        return None;
    };
    match content {
        MessageContent::Text(text) => {
            apply_head_tail(text, *keep_head, *keep_tail).map(MessageContent::text)
        }
        MessageContent::Blocks(_) | MessageContent::Raw(_) => {
            apply_head_tail(&content.text_content(), *keep_head, *keep_tail)
                .map(MessageContent::text)
        }
    }
}

// ─── project_content ──────────────────────────────────────────────────────────

/// 对 MessageContent 中的每个 ContentBlock 应用对应 action。
///
/// 借用入口：没有命中 action 时返回 `Cow::Borrowed`，正文只在需要输出时复制一次。
fn project_content_ref<'a>(
    content: &'a MessageContent,
    block_actions: &HashMap<usize, &ProjectionActionEntry>,
    caps: &ProviderCapabilities,
) -> Cow<'a, MessageContent> {
    if block_actions.is_empty() {
        return Cow::Borrowed(content);
    }

    match content {
        MessageContent::Text(text) => {
            if text.is_empty() {
                return Cow::Borrowed(content);
            }
            let Some(entry) = block_actions.get(&0) else {
                return Cow::Borrowed(content);
            };
            match project_text(text, &entry.action, caps) {
                Some(projected) => Cow::Owned(MessageContent::text(projected)),
                None => Cow::Borrowed(content),
            }
        }
        MessageContent::Blocks(blocks) => {
            if blocks.is_empty() {
                return Cow::Borrowed(content);
            }
            let projected = blocks
                .iter()
                .enumerate()
                .map(|(index, block)| {
                    project_block(
                        block,
                        block_actions.get(&index).map(|entry| &entry.action),
                        caps,
                    )
                })
                .collect();
            Cow::Owned(MessageContent::Blocks(projected))
        }
        // Raw 内容无法逐块投影——原样保留
        MessageContent::Raw(_) => Cow::Borrowed(content),
    }
}

/// Text block 投影：`None` 表示保持原文（不复制）。
fn project_text(
    text: &str,
    action: &ProjectionAction,
    _caps: &ProviderCapabilities,
) -> Option<String> {
    match action {
        ProjectionAction::Exclude => Some("[已排除]".to_string()),
        ProjectionAction::CompactToolResult {
            keep_head,
            keep_tail,
            ..
        } => apply_head_tail(text, *keep_head, *keep_tail),
        ProjectionAction::CompactText { max_chars } => {
            let chars: Vec<char> = text.chars().collect();
            if chars.len() <= *max_chars {
                return None;
            }
            let truncated: String = chars[..*max_chars].iter().collect();
            Some(format!("{}\n[内容已压缩]", truncated))
        }
        _ => None,
    }
}

/// 对 tool result 的完整文本流应用 CompactToolResult action。
///
/// 借用入口：只复制截断后的片段；纯文本不先物化整份正文。
fn project_tool_result_content_ref(
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

    match content {
        // 纯文本是常见情形：直接切片截断，避免拼接后再复制。
        MessageContent::Text(text) => match apply_head_tail(text, *keep_head, *keep_tail) {
            Some(projected) => MessageContent::text(projected),
            None => content.clone(),
        },
        MessageContent::Blocks(blocks) => {
            MessageContent::Blocks(project_tool_result_blocks(blocks, *keep_head, *keep_tail))
        }
        raw @ MessageContent::Raw(_) => raw.clone(),
    }
}

/// 逐块截断嵌套 tool result；只复制文本片段与存活块。
fn project_tool_result_blocks(
    blocks: &[ContentBlock],
    keep_head: usize,
    keep_tail: usize,
) -> Vec<ContentBlock> {
    blocks
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => ContentBlock::Text {
                text: apply_head_tail(text, keep_head, keep_tail).unwrap_or_else(|| text.clone()),
            },
            ContentBlock::ToolResult {
                id,
                tool_use_id,
                content,
                is_error,
            } => ContentBlock::ToolResult {
                id: id.clone(),
                tool_use_id: tool_use_id.clone(),
                content: project_tool_result_blocks(content, keep_head, keep_tail),
                is_error: *is_error,
            },
            other => other.clone(),
        })
        .collect()
}

// ─── project_block ────────────────────────────────────────────────────────────

/// 投影单个 ContentBlock
fn project_block(
    block: &ContentBlock,
    action: Option<&ProjectionAction>,
    _caps: &ProviderCapabilities,
) -> ContentBlock {
    if matches!(
        block,
        ContentBlock::ToolUse { .. } | ContentBlock::ProviderState { .. }
    ) {
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
            ContentBlock::ToolResult {
                id,
                tool_use_id,
                content,
                is_error,
            } => ContentBlock::ToolResult {
                id: id.clone(),
                tool_use_id: tool_use_id.clone(),
                content: project_tool_result_blocks(content, *keep_head, *keep_tail),
                is_error: *is_error,
            },
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
pub(super) fn apply_head_tail(text: &str, head_chars: usize, tail_chars: usize) -> Option<String> {
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
