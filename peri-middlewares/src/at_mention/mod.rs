mod parser;

use peri_agent::middleware::capabilities as hook_state;
use std::sync::Arc;

use crate::workspace_io::{WorkspaceFileReader, WorkspaceMentionContent};
use async_trait::async_trait;
use peri_agent::{
    error::AgentResult,
    messages::{BaseMessage, ContentBlock},
    middleware::r#trait::Middleware,
};

const WORKSPACE_MENTION_REQUEST: &str = "workspace/readMention";

/// 单项 mention 注入正文的 UTF-8 字节预算（宿主侧兜底界）。
///
/// provider（workspace 实例）另有同口径上限；宿主这一份保证远端/第三方
/// Workspace 实现不遵守预算时，模型可见正文仍不超界。数值是**界**，
/// 不是实测最优值。
const MAX_MENTION_CONTENT_BYTES: usize = 32 * 1024;
/// 一批输入（可含多条 Human）累计注入预算：多个合法文件不得无限累加。
const MAX_BATCH_MENTION_BYTES: usize = 128 * 1024;

/// AtMentionMiddleware — 解析用户消息中的 @path 提及，注入 Workspace 读取结果
///
/// 挂在 `before_input`：首批输入由 `run_before_agent` 的交错调用覆盖，同 loop
/// 的中途批次（steering / SDK 追加）由后续 `before_input` 覆盖。只消费
/// `input_message_ids`，不扫描历史最后一条 Human 充当新输入；空批次不注入。
///
/// 从会话 Workspace 读取对应内容，以 Ai[ToolUse{workspace/readMention}] →
/// Tool[ToolResult] 消息序列追加到 state。
///
/// 消息结构（与 SkillPreloadMiddleware 一致）：
/// ```text
/// [Human "用户消息（含 @path）"]
/// [Ai]    [ToolUse{workspace/readMention, call_{hex}}, ...]
/// [Tool]  ToolResult{call_{hex}, file_content}
/// ...
/// ```
///
/// # 预算（H7）
///
/// - 单项正文超过 [`MAX_MENTION_CONTENT_BYTES`]：按 UTF-8 边界截断，截断处
///   给出显式说明与可继续读取的行号（行内无法按行续读时明确说明）；
/// - 单批累计超过 [`MAX_BATCH_MENTION_BYTES`]：**不读**剩余提及，改为注入
///   显式「未载入（预算已用尽）」回执，不静默裁掉内容；
/// - 读取失败（Workspace 关闭 / 断连 / 路径不存在）不注入、不回落本机磁盘，
///   只记 debug。
pub struct AtMentionMiddleware {
    reader: Arc<dyn WorkspaceFileReader>,
}

impl AtMentionMiddleware {
    pub fn new(reader: Arc<dyn WorkspaceFileReader>) -> Self {
        Self { reader }
    }
}

#[async_trait]
impl Middleware for AtMentionMiddleware {
    fn name(&self) -> &str {
        "AtMentionMiddleware"
    }

    /// 输入准备（`before_input`）：首批由 `run_before_agent` 按链序交错调用本钩子
    /// 覆盖，同 loop 的中途批次（steering / SDK 追加）由 `run_before_input` 覆盖——
    /// 只实现一次即两条路径都生效，不会对首批重复注入。
    async fn before_input(&self, state: &mut dyn hook_state::BeforeInputState) -> AgentResult<()> {
        self.prepare_batch(state).await
    }
}

/// 本批待处理输入文本（仅本批 Human；没有批次身份就不猜历史）。
fn batch_inputs(state: &dyn hook_state::BeforeInputState) -> Vec<String> {
    let Some(ids) = state.input_message_ids() else {
        tracing::debug!(
            "AtMentionMiddleware: 本次输入没有批次身份（legacy 适配器），跳过附件注入且不扫描历史 Human"
        );
        return Vec::new();
    };
    if ids.is_empty() {
        return Vec::new();
    }
    state
        .messages()
        .iter()
        .filter(|message| {
            matches!(message, BaseMessage::Human { .. }) && ids.contains(&message.id())
        })
        .map(BaseMessage::content)
        .collect()
}

/// 一批 mention 的累计注入预算（批内所有输入消息共享，UTF-8 字节计）。
struct BatchBudget {
    remaining: usize,
}

impl BatchBudget {
    fn new() -> Self {
        Self {
            remaining: MAX_BATCH_MENTION_BYTES,
        }
    }

    /// 剩余预算；耗尽时调用方必须产出显式「未载入」回执而不是静默跳过。
    fn remaining(&self) -> usize {
        self.remaining
    }

    fn spend(&mut self, bytes: usize) {
        self.remaining = self.remaining.saturating_sub(bytes);
    }
}

/// 单项正文按字节预算裁剪的结果。
struct TrimmedContent {
    text: String,
    /// 裁剪是否真的发生（未发生时不加任何说明文字）。
    truncated: bool,
    /// 可继续读取的 1-based 行号；无法按行续读（单行超预算 / 目录）时为 None。
    resume_line: Option<usize>,
}

/// 按 UTF-8 字节预算裁剪正文，优先在行边界收束。
///
/// `first_line` = 本次读取的起始行号（1-based；未知为 None）：裁剪后据此给出
/// 「从哪一行继续读」的可继续位置。裁剪处一律附显式说明，不无标记裁掉内容。
fn trim_mention_content(content: &str, first_line: Option<usize>, budget: usize) -> TrimmedContent {
    if content.len() <= budget {
        return TrimmedContent {
            text: content.to_string(),
            truncated: false,
            resume_line: None,
        };
    }
    let cut = floor_char_boundary(content, budget);
    // 优先退到最后一个换行：保留的行都是完整行，续读行号可精确计算。
    match content[..cut].rfind('\n') {
        Some(newline) => {
            let kept_lines = content[..newline].matches('\n').count() + 1;
            let resume_line = first_line.map(|first| first + kept_lines);
            let note = match resume_line {
                Some(line) => format!(
                    "... (正文超过 {budget} 字节预算，已截断；从 L{line} 继续读取)"
                ),
                None => format!("... (正文超过 {budget} 字节预算，已截断)"),
            };
            TrimmedContent {
                text: format!("{}\n{note}", &content[..newline]),
                truncated: true,
                resume_line,
            }
        }
        // 单行就超过预算：只能按字符边界切，行级续读位置不可用——明确说明。
        None => TrimmedContent {
            text: format!(
                "{}\n... (单行超过 {budget} 字节预算，已按 UTF-8 边界截断；请用更小的行范围重新读取)",
                &content[..cut]
            ),
            truncated: true,
            resume_line: None,
        },
    }
}

/// 不大于 `max` 的最大 UTF-8 字符边界（不切断多字节字符）。
fn floor_char_boundary(text: &str, max: usize) -> usize {
    let mut end = max.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// 单项注入正文的渲染：provider 已按自身预算截断时不重复加说明（其正文已带
/// 截断与可继续读取位置），只在宿主这次裁剪时补说明。
fn render_mention_body(content: &WorkspaceMentionContent, budget: usize) -> TrimmedContent {
    if content.is_dir {
        // 目录列表没有行语义：超预算时只标记截断，不编造行号续读位置。
        return trim_directory_listing(&content.content, budget);
    }
    // 未显式给行号时读取自第 1 行起，续读行号据此计算。
    trim_mention_content(
        &content.content,
        Some(content.line_start.unwrap_or(1)),
        budget,
    )
}

/// 目录列表的字节裁剪（无行号语义）。
fn trim_directory_listing(content: &str, budget: usize) -> TrimmedContent {
    if content.len() <= budget {
        return TrimmedContent {
            text: content.to_string(),
            truncated: false,
            resume_line: None,
        };
    }
    let cut = floor_char_boundary(content, budget);
    let cut = content[..cut].rfind('\n').unwrap_or(cut);
    TrimmedContent {
        text: format!(
            "{}\n... (目录条目超过 {budget} 字节预算，已截断；请用更具体的路径缩小范围)",
            &content[..cut]
        ),
        truncated: true,
        resume_line: None,
    }
}

impl AtMentionMiddleware {
    async fn prepare_batch(&self, state: &mut dyn hook_state::BeforeInputState) -> AgentResult<()> {
        let inputs = batch_inputs(state);
        if inputs.is_empty() {
            return Ok(());
        }
        let mut budget = BatchBudget::new();
        for text in inputs {
            self.prepare_mentions(state, &text, &mut budget).await?;
        }
        Ok(())
    }

    async fn prepare_mentions(
        &self,
        state: &mut dyn hook_state::BeforeInputState,
        text: &str,
        budget: &mut BatchBudget,
    ) -> AgentResult<()> {
        let mentions = parser::extract_at_mentions(text);
        if mentions.is_empty() {
            return Ok(());
        }

        // 每项过预算闸门：预算耗尽的提及**不读**（限制实际读取量），改为
        // 显式「未载入」回执，与成功项一一配对。
        let mut items: Vec<(parser::AtMention, MentionOutcome)> =
            Vec::with_capacity(mentions.len());
        for mention in mentions {
            if budget.remaining() == 0 {
                tracing::warn!(
                    path = %mention.path,
                    "workspace mention 批次注入预算已用尽，未读取该提及（显式未载入回执）"
                );
                items.push((mention, MentionOutcome::BudgetExhausted));
                continue;
            }
            let result = self
                .reader
                .read_mention(&mention.path, mention.line_start, mention.line_end)
                .await;
            match result {
                Ok(content) => {
                    let per_item = budget.remaining().min(MAX_MENTION_CONTENT_BYTES);
                    let rendered = render_mention_body(&content, per_item);
                    budget.spend(rendered.text.len());
                    items.push((mention, MentionOutcome::Loaded(rendered)));
                }
                Err(error) => {
                    tracing::debug!(path = %mention.path, %error, "workspace mention read skipped");
                }
            }
        }

        if items.is_empty() {
            return Ok(());
        }

        // 生成 call_id
        let call_ids: Vec<String> = (0..items.len())
            .map(|_| format!("call_{}", uuid::Uuid::new_v4().simple()))
            .collect();

        // 构造 ToolUse blocks
        let tool_use_blocks: Vec<ContentBlock> = items
            .iter()
            .zip(call_ids.iter())
            .map(|((mention, _), id)| {
                let mut input = serde_json::json!({ "path": mention.path });
                if let Some(line_start) = mention.line_start {
                    input["lineStart"] = serde_json::json!(line_start);
                }
                if let Some(line_end) = mention.line_end {
                    input["lineEnd"] = serde_json::json!(line_end);
                }
                ContentBlock::tool_use(id.clone(), WORKSPACE_MENTION_REQUEST, input)
            })
            .collect();

        // 追加 Ai 消息
        state.append_input_message(BaseMessage::ai_from_blocks(tool_use_blocks));

        // 追加 ToolResult 消息
        for (id, (mention, outcome)) in call_ids.iter().zip(items.iter()) {
            state.append_input_message(BaseMessage::tool_result(
                id.clone(),
                outcome.render(mention),
            ));
        }

        Ok(())
    }
}

/// 单项提及的注入结果。
enum MentionOutcome {
    /// 已读取（可能按预算截断）。
    Loaded(TrimmedContent),
    /// 本批注入预算已用尽：不读、不假装成功，给显式未载入回执。
    BudgetExhausted,
}

impl MentionOutcome {
    fn render(&self, mention: &parser::AtMention) -> String {
        match self {
            Self::Loaded(trimmed) => {
                let prefix = match (mention.line_start, mention.line_end) {
                    (Some(s), Some(e)) => format!("→ {} (L{s}-L{e})", mention.path),
                    (Some(s), None) => format!("→ {} (L{s})", mention.path),
                    _ => format!("→ {}", mention.path),
                };
                format!("{prefix}\n{}", trimmed.text)
            }
            Self::BudgetExhausted => format!(
                "→ {}\n... (本批 @mention 注入预算已用尽，未载入该文件；请用 Read 工具按需读取)",
                mention.path
            ),
        }
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
pub(crate) mod tests;
