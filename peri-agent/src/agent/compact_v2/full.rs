//! Full Compact 实现
//!
//! 完整流程：
//! 1. 从包含 canonical reminder 的可见模型上下文派生摘要请求
//! 2. LLM 生成结构化摘要；未完成时在同一次 Full 内用**有界恢复循环**补齐
//! 3. 后处理摘要
//! 4. 快照内自有的普通历史和 reminder 标 excluded（保留 System / ancestor），同一批 flags
//!    里写下取代关系：被归档条目记下第 5 步那条摘要的 id
//! 5. 追加 Human 摘要消息（带 CONTINUATION_HINT，wrap 在 system-reminder 标签中），id 在
//!    提交前先分配，供第 4 步引用
//!
//! 历史工具调用及结果留存在 transcript 中；不会从计算实例本机重新读取 Workspace 文件。
//!
//! 恢复循环只把已生成的正文片段回填进临时请求；未完成摘要、错误字符串与
//! 隐藏思考都不进入正式 transcript，也不执行摘要响应里的工具调用。只有正常
//! 通过校验的非空摘要才经现有原子提交替换历史。

use peri_model::{ModelMessage, ModelRequest};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use self::summary::{classify_summary_step, SummaryRecoveryReason, SummaryStep};
use super::projection::{MessageProjectionDirective, PROJECTION_POLICY_VERSION};
use crate::agent::{
    compact_v2::{config::CompactConfig, CompactOutcome},
    events::CompactStrategy,
    model_bridge::{map_model_error, AgentModelBridge},
};
use crate::error::{AgentError, AgentResult};
use crate::messages::{BaseMessage, MessageId};
use crate::session::transcript::MessageTranscript;
use crate::session::MessageFlags;
use crate::thread::CompactionChange;

// ─── 公共常量 ──────────────────────────────────────────────────────────────────

/// Full Compact 摘要 system prompt
const SUMMARY_SYSTEM_PROMPT: &str = include_str!("descriptions/summary_system_prompt.md");

/// Full Compact user prompt 模板
const SUMMARY_USER_PROMPT: &str = include_str!("descriptions/summary_user_prompt.md");

#[path = "summary.rs"]
mod summary;

/// 摘要恢复提示：从已有正文末尾续写，只输出缺失部分。
const SUMMARY_RECOVERY_PROMPT: &str = "Your summary was cut off. Continue exactly from the end of your previous text, \
including any unfinished word or tag. Output only the missing remainder; do not restart, repeat earlier sections, or add \
analysis. Prioritize the essential facts that are still missing — the active request and its constraints, work status, \
and the next step — instead of only closing tags. Do not call tools.";

/// 最后一次生成的提示：补齐必要内容并在预算内简短收尾。
const SUMMARY_FINAL_PROMPT: &str = "Your summary is still incomplete. Continue exactly from the end of your previous text \
and output only the missing remainder; do not restart or repeat earlier sections. This is your final response: finish \
the remaining essential facts — the active request and constraints, work status, and the next step — concisely within \
the output budget and close </summary>. There will be no further continuation. Do not call tools.";

/// 从未取得正文时的重新生成提示（替换原摘要指令，目标已减半）。
const SUMMARY_REGENERATION_PROMPT: &str = "Your previous response contained no usable summary text. Respond now with a \
complete summary: output only <summary>...</summary>, with no analysis or preamble. Finish every priority section from \
the instructions above and close the summary within the reduced target. Do not call tools.";

// ─── Full Compact ───────────────────────────────────────────────────────────────

/// Full Compact 内部实现
///
/// 步骤：
/// 1. 从包含 canonical reminder 的可见模型上下文派生摘要请求
/// 2. LLM 生成结构化摘要（未完成时同一次 Full 内有界恢复）
/// 3. 后处理摘要
/// 4. 快照内自有的普通历史和 reminder 标 excluded（保留 System / ancestor），同一批 flags
///    里写下取代关系：每条被归档条目记下第 5 步那条摘要的 id（`superseded_by`）
/// 5. 追加 Human 摘要消息（带 CONTINUATION_HINT，wrap 在 system-reminder 标签中），其 id
///    在第 4 步之前分配，因此两处指向同一条消息
///
/// 排除与取代关系必须在**同一次原子提交**里落地：只写排除会让 rewind 失去逐条依据，
/// 只写取代关系则原文仍然可见。
#[cfg(test)]
pub(super) async fn full_compact_inner(
    transcript: &mut MessageTranscript,
    llm: Option<&dyn peri_model::Model>,
    config: &CompactConfig,
    cwd: &str,
) -> AgentResult<super::CompactResult> {
    full_compact_inner_with_protected(
        transcript,
        llm,
        config,
        cwd,
        &crate::session::async_result_mailbox::ProtectedResults::default(),
    )
    .await
}

/// 带后台结果保护集合的 Full Compact 实现（生产入口）。
///
/// 受保护结果不进入摘要请求，也不被归档；它们由派生模型视图在摘要之后按接收
/// 顺序重新投影。只有会话路径传入非空保护集合。
pub(super) async fn full_compact_inner_with_protected(
    transcript: &mut MessageTranscript,
    llm: Option<&dyn peri_model::Model>,
    config: &CompactConfig,
    _cwd: &str,
    protected: &crate::session::async_result_mailbox::ProtectedResults,
) -> AgentResult<super::CompactResult> {
    let llm = llm.ok_or(crate::error::AgentError::CompactNoLlm)?;
    // Full 和 Reason 读取同一已提交视图。按摘要 provider 的协议保护 reasoning；恢复器只接受
    // 与工具输入/思考独立的 ToolResult 投影，不为摘要另做有损预览。
    //
    // 尚未被主模型响应覆盖的终态通知即使已进入 canonical transcript，也不属于
    // 本次可归档历史：摘要模型不承担消费确认。这里先按保护集合移除（Reason 会在
    // 摘要之后按接收顺序重新投影一次），既不把它们交给摘要模型，也不归档。
    let protected_ids: std::collections::HashSet<MessageId> = protected
        .items
        .iter()
        .map(|item| item.delivery_id)
        .collect();
    let mut visible = super::projection::render_persisted_llm_view_with_protected(
        transcript,
        &AgentModelBridge::projection_capabilities(llm),
        protected,
    )?;
    if !protected_ids.is_empty() {
        visible.retain(|message| !protected_ids.contains(&message.id()));
    }
    let before_visible_len = visible.len();
    // 先固定本次快照的 own IDs；await 期间到达 inbox 的新结果不属于本次摘要。
    // ancestor 与 System 仍由各自 owner 管理；canonical reminder 不是豁免历史。
    // 追加的摘要 id 同样在提交前分配：同一批 flags 里每条被归档条目都指向它，
    // 追加时必须恰好用这个 id（见下方 `build_summary_message`）。
    let summary_id = MessageId::new();
    let flag_updates: Vec<_> = transcript
        .entries()
        .iter()
        .skip(transcript.ancestor_len())
        .filter(|entry| !transcript.flags(entry.id()).excluded)
        .filter(|entry| !matches!(entry.as_message(), Some(BaseMessage::System { .. })))
        .filter(|entry| !protected_ids.contains(&entry.id()))
        .map(|entry| {
            (
                entry.id(),
                MessageFlags {
                    excluded: true,
                    // 归档即「被本次摘要取代」：entries 为空（Full 不写渲染指令，被排除
                    // 条目也不渲染），只承载这条渲染之外的事实，供 rewind 精确判定。
                    projection: Some(MessageProjectionDirective {
                        policy_version: PROJECTION_POLICY_VERSION,
                        entries: Vec::new(),
                        superseded_by: Some(summary_id),
                    }),
                    ..Default::default()
                },
            )
        })
        .collect();
    let affected_count = flag_updates.len();
    // 只有继承上下文时没有可替换的 own 历史，沿用空历史 fallback，避免无效摘要调用。
    let has_history = !flag_updates.is_empty()
        && visible
            .iter()
            .any(|message| !matches!(message, BaseMessage::System { .. }));
    // 只剩 System 与受保护结果时，没有可归档历史：不调用摘要模型、不追加摘要、
    // 不写任何 flags。处理责任仍在 mailbox，下一次请求继续附带。
    if !protected.is_empty()
        && flag_updates.is_empty()
        && !visible
            .iter()
            .any(|message| !matches!(message, BaseMessage::System { .. }))
    {
        debug!(
            protected = protected.items.len(),
            "Full Compact: 仅 System 与受保护结果，跳过归档"
        );
        return Ok(super::CompactResult {
            strategy: CompactStrategy::Full,
            affected_count: 0,
            estimated_tokens_saved: 0,
            before_visible_len,
            after_visible_len: transcript.visible_messages().len(),
            summary: None,
            full_escalation_reason: None,
            outcome: CompactOutcome::Skipped,
            failure: None,
            changed_messages: 0,
            changed_fields: 0,
            no_op_candidates: 0,
        });
    }
    let summary = if has_history {
        let budget = SummaryBudget::resolve(llm, config)?;
        // 保留历史的角色、工具配对和完整正文；摘要指令只追加到派生请求，
        // 不回写原 transcript，也不提供可执行工具。
        let mut messages = vec![ModelMessage::system_text(SUMMARY_SYSTEM_PROMPT)];
        messages.extend(AgentModelBridge::convert_messages(&visible)?);
        let conversation = RecoveryConversation::default();
        complete_summary(llm, messages, &visible, budget, conversation).await?
    } else {
        // 全 System / 空历史仍保持命令输出 Human-first 的既有契约。
        "No conversation history to compact.".to_owned()
    };

    transcript
        .commit_compaction_lifecycle(CompactionChange {
            flag_updates,
            appended_messages: vec![build_summary_message(&summary, summary_id)],
        })
        .await?;
    transcript.mark_full_compaction_committed();

    let after_visible = transcript
        .entries()
        .iter()
        .filter(|entry| !transcript.flags(entry.id()).excluded)
        .count();

    debug!(
        before_visible_len,
        after_visible, "Full Compact: excluded 旧消息 + 追加摘要"
    );

    Ok(super::CompactResult {
        strategy: CompactStrategy::Full,
        affected_count,
        estimated_tokens_saved: 0,
        before_visible_len,
        after_visible_len: after_visible,
        summary: Some(summary),
        full_escalation_reason: None,
        outcome: CompactOutcome::FullApplied,
        failure: None,
        changed_messages: 0,
        changed_fields: 0,
        no_op_candidates: 0,
    })
}

/// Full Compact 派生请求的输出预算与恢复次数预算（H6）。
///
/// 输出预算的单一来源是**当前模型已解析的单次输出上限**（`Model::output_token_limit`），
/// 不是 `summary_max_tokens`：后者是长度目标上限，只影响提示词里的目标字数，
/// 并受有效输出预算约束。恢复次数由 `summary_max_requests` 限定，首轮与所有
/// 恢复共用同一个计数器。
struct SummaryBudget {
    /// 模型已解析的单次输出上限；`None` = provider 未声明，请求沿用 provider 默认。
    output_limit: Option<u32>,
    /// 提示词中声明的长度目标（受有效输出预算约束）。
    target_tokens: u32,
    /// 单次 Full 内允许的逻辑摘要生成次数（首轮 + 恢复），恒 ≥ 1。
    max_requests: u32,
}

impl SummaryBudget {
    fn resolve(llm: &dyn peri_model::Model, config: &CompactConfig) -> AgentResult<Self> {
        if config.summary_max_tokens == 0 {
            // 零值不是「不限」：既不能成为长度目标，也不允许被代填成任意常量。
            return Err(AgentError::CompactSummaryBudgetInvalid {
                reason: "summary_max_tokens must be greater than zero",
            });
        }
        if config.summary_max_requests == 0 {
            // 零值不是「不限」：没有逻辑生成次数的请求预算，就不是可执行的配置。
            return Err(AgentError::CompactSummaryBudgetInvalid {
                reason: "summary_max_requests must be at least 1",
            });
        }
        let output_limit = match llm.output_token_limit() {
            Some(0) => {
                return Err(AgentError::CompactSummaryBudgetInvalid {
                    reason: "resolved model output limit is zero",
                })
            }
            limit => limit,
        };
        Ok(Self {
            output_limit,
            target_tokens: output_limit.map_or(config.summary_max_tokens, |limit| {
                config.summary_max_tokens.min(limit)
            }),
            max_requests: config.summary_max_requests,
        })
    }

    /// 首轮提示词声明的长度目标：给模型输出留余量。
    fn initial_target_tokens(&self) -> u32 {
        (self.target_tokens / 2).max(1)
    }

    /// 无正文重新生成时声明的长度目标：在首轮目标上再减半，且不触碰 provider 上限。
    fn regeneration_target_tokens(&self) -> u32 {
        (self.initial_target_tokens() / 2).max(1)
    }

    /// 把有效预算应用到请求上：只沿用已解析上限，不写入 `summary_max_tokens`。
    fn apply(&self, request: ModelRequest) -> ModelRequest {
        match self.output_limit {
            Some(limit) => request.with_max_tokens(limit),
            None => request,
        }
    }
}

/// 一次 Full 的临时摘要恢复对话（原始请求 + 已生成片段 + 恢复提示）。
///
/// 恢复对话始终以 user 提示结尾；回填的片段只出现一次，避免同时加入
/// 「合并全文」和同一批历史片段而重复计算。连续的同角色消息合并到尾部
/// user 提示里，保持 provider 要求的 user/assistant 交替。
#[derive(Default)]
struct RecoveryConversation {
    /// 除原始请求外的恢复对话消息。
    messages: Vec<ModelMessage>,
    /// 按顺序累计的可回填片段；拼接时不插入分隔符。
    chunks: String,
}

impl RecoveryConversation {
    /// 追加本轮提示；尾部已是 user 时并入同一条消息。
    fn push_prompt(&mut self, prompt: &str) {
        if let Some(ModelMessage::User { content }) = self.messages.last_mut() {
            content.push(peri_model::ContentBlock::text(prompt));
            return;
        }
        self.messages.push(ModelMessage::user_text(prompt));
    }

    /// 用新提示替换尾部提示（无正文重新生成时改写目标，不注入空 assistant）。
    fn replace_prompt(&mut self, prompt: &str) {
        if matches!(self.messages.last(), Some(ModelMessage::User { .. })) {
            self.messages.pop();
        }
        self.messages.push(ModelMessage::user_text(prompt));
    }

    /// 回填已生成的 assistant 正文片段。
    fn push_chunk(&mut self, chunk: &str) {
        self.chunks.push_str(chunk);
        self.messages.push(ModelMessage::assistant_text(chunk));
    }

    /// 构造本次派生请求：原始请求 + 恢复对话。
    fn request(&self, prefix: &[ModelMessage]) -> ModelRequest {
        let mut messages = prefix.to_vec();
        messages.extend(self.messages.iter().cloned());
        ModelRequest::new(messages)
    }

    /// 已有草稿的字节长度；没有 tokenizer 估算时只作为诊断量，不冒充 tokens。
    fn draft_bytes(&self) -> usize {
        self.chunks.len()
    }
}

/// 有界恢复循环：每次 Full 只允许 `summary_max_requests` 次逻辑生成。
///
/// 取消、配置错误、provider 重试耗尽与持久化失败都不在这里重开预算。
/// 这里不做「请求装不下就拒绝发送」的前置拦截：摘要请求的输入是历史本身，
/// 装不下时正确的做法是发出请求并由 provider / 截断驱动恢复循环续写，而不是
/// 在 compaction 最需要的时刻拒绝压缩（拒绝会让高压 turn 直接终止）。
async fn complete_summary(
    llm: &dyn peri_model::Model,
    prefix: Vec<ModelMessage>,
    visible: &[BaseMessage],
    budget: SummaryBudget,
    mut conversation: RecoveryConversation,
) -> AgentResult<String> {
    // 历史长度沿用 Reason 侧的同一 chars/4 估算；恢复片段与提示逐次计入。
    let history_tokens = crate::agent::token::estimate_request_tokens(visible, &[]);
    let mut request_index = 0u32;
    let mut empty_regeneration_used = false;
    let mut last_reason: Option<SummaryRecoveryReason> = None;

    loop {
        if request_index >= budget.max_requests {
            // 绝不发起第 N+1 次逻辑生成。
            let error = AgentError::CompactRecoveryExhausted {
                attempts: request_index,
                last_reason: last_reason.unwrap_or(SummaryRecoveryReason::EmptyResponse),
            };
            warn!(
                attempts = request_index,
                max_requests = budget.max_requests,
                recovery_reason = ?last_reason,
                draft_bytes = conversation.draft_bytes(),
                "Full Compact summary recovery exhausted; the original history is unchanged"
            );
            return Err(error);
        }
        let is_final_request = request_index + 1 == budget.max_requests;
        if request_index == 0 {
            conversation.push_prompt(&summary_instruction(budget.initial_target_tokens()));
        } else if conversation.chunks.is_empty() {
            // 从未取得可用正文：只允许一次重新生成，目标减半，不注入空 assistant。
            conversation.replace_prompt(&regeneration_instruction(
                budget.regeneration_target_tokens(),
            ));
        } else if is_final_request {
            conversation.push_prompt(SUMMARY_FINAL_PROMPT);
        } else {
            conversation.push_prompt(SUMMARY_RECOVERY_PROMPT);
        }
        let next_index = request_index + 1;
        let estimated_input_tokens =
            history_tokens.saturating_add(estimate_dialog_tokens(&conversation.messages));
        let request = budget.apply(conversation.request(&prefix));
        tokio::task::yield_now().await;
        request_index = next_index;
        let response = llm
            .complete(request, CancellationToken::new())
            .await
            .map_err(map_model_error)?;
        debug!(
            request_index,
            max_requests = budget.max_requests,
            stop_reason = ?response.stop_reason(),
            input_tokens_estimate = estimated_input_tokens,
            output_tokens = response.usage().map(|usage| usage.output_tokens),
            draft_bytes = conversation.draft_bytes(),
            "Full Compact summary request completed"
        );
        match classify_summary_step(&response, &conversation.chunks)? {
            SummaryStep::Complete(text) => {
                info!(
                    request_index,
                    max_requests = budget.max_requests,
                    recovered = request_index > 1,
                    "Full Compact summary completed"
                );
                return Ok(text);
            }
            SummaryStep::Draft { chunk, reason } => {
                // 截断/未闭合是恢复分支，不是 session fatal error。
                warn!(
                    request_index,
                    max_requests = budget.max_requests,
                    recovery_reason = %reason,
                    stop_reason = ?response.stop_reason(),
                    output_tokens = response.usage().map(|usage| usage.output_tokens),
                    draft_bytes = conversation.draft_bytes(),
                    "Full Compact summary is incomplete; continuing within the same Full"
                );
                last_reason = Some(reason);
                conversation.push_chunk(&chunk);
            }
            SummaryStep::Empty { reason } => {
                warn!(
                    request_index,
                    max_requests = budget.max_requests,
                    recovery_reason = %reason,
                    stop_reason = ?response.stop_reason(),
                    previous_draft_bytes = conversation.draft_bytes(),
                    "Full Compact summary response has no usable text"
                );
                if conversation.chunks.trim().is_empty() {
                    if empty_regeneration_used {
                        // 重新生成后仍无正文：提前结束，避免用光预算空转。
                        return Err(AgentError::CompactRecoveryExhausted {
                            attempts: request_index,
                            last_reason: reason,
                        });
                    }
                    empty_regeneration_used = true;
                }
                last_reason = Some(reason);
            }
        }
    }
}

/// 摘要指令：模板 + 本次声明的长度目标。
fn summary_instruction(target_tokens: u32) -> String {
    SUMMARY_USER_PROMPT.replace("{summary_target_tokens}", &target_tokens.to_string())
}

/// 重新生成指令：原指令（目标减半）+ 明确要求只输出摘要正文。
fn regeneration_instruction(target_tokens: u32) -> String {
    format!(
        "{}\n\n{}",
        summary_instruction(target_tokens),
        SUMMARY_REGENERATION_PROMPT
    )
}

/// 恢复对话的输入估算：与 `crate::agent::token` 共用 chars/4 启发式。
///
/// 只统计会发给模型的正文（隐藏思考不进入派生请求）；这是近似值，不冒充 usage。
fn estimate_dialog_tokens(messages: &[ModelMessage]) -> u64 {
    let chars: u64 = messages
        .iter()
        .map(|message| match message {
            ModelMessage::System { content }
            | ModelMessage::User { content }
            | ModelMessage::Assistant { content, .. } => content
                .iter()
                .map(estimate_content_chars)
                .fold(0u64, u64::saturating_add),
            ModelMessage::ToolResult { result } => result
                .content
                .iter()
                .map(estimate_content_chars)
                .fold(0u64, u64::saturating_add),
        })
        .fold(0u64, u64::saturating_add);
    chars.div_ceil(4)
}

fn estimate_content_chars(block: &peri_model::ContentBlock) -> u64 {
    use peri_model::ContentBlock;
    match block {
        ContentBlock::Text { text } => text.chars().count() as u64,
        ContentBlock::ToolUse { tool_call } => {
            let arguments = serde_json::to_string(tool_call.arguments()).unwrap_or_default();
            tool_call
                .name()
                .chars()
                .count()
                .saturating_add(arguments.chars().count()) as u64
        }
        // 摘要请求不带工具定义；图片/文档只有占位成本，按有界常量估算。
        ContentBlock::Image { .. } | ContentBlock::Document { .. } => {
            crate::agent::token::BINARY_MEDIA_ESTIMATED_TOKENS
        }
        ContentBlock::Reasoning { .. } | ContentBlock::RedactedReasoning { .. } => 0,
        ContentBlock::ToolResult { result } => result
            .content
            .iter()
            .map(estimate_content_chars)
            .fold(0u64, u64::saturating_add),
    }
}

/// 构造 Full Compact 的 Human 摘要消息，并使用调用方预先分配的 `id`。
///
/// id 不是在这里现取的：同一批 `flag_updates` 里每条被归档条目都把 `id` 记为
/// [`MessageProjectionDirective::superseded_by`]，两处必须指向同一条消息。
fn build_summary_message(summary: &str, id: MessageId) -> BaseMessage {
    BaseMessage::human(format!(
        "{}\n\n{}",
        crate::agent::compact_v2::CONTINUATION_HINT,
        summary
    ))
    .with_message_id(id)
}

/// 提取旧格式压缩文件/Skill 元信息（事实源 peri-acp-types::compact）
pub use peri_acp_types::compact::{extract_file_info, extract_skill_names};

#[cfg(test)]
#[path = "full_test.rs"]
mod tests;

#[cfg(test)]
#[path = "full_report_test.rs"]
mod report_tests;

#[cfg(test)]
#[path = "full_continuation_test.rs"]
mod continuation_tests;
