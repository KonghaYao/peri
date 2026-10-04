//! Full Compact 实现
//!
//! 完整流程：
//! 1. 从包含 canonical reminder 的可见模型上下文派生摘要请求
//! 2. LLM 生成结构化摘要
//! 3. 后处理摘要
//! 4. 快照内自有的普通历史和 reminder 标 excluded（保留 System / ancestor）
//! 5. 追加 Human 摘要消息（带 CONTINUATION_HINT，wrap 在 system-reminder 标签中）
//! 历史工具调用及结果留存在 transcript 中；不会从计算实例本机重新读取 Workspace 文件。

use peri_model::{ModelMessage, ModelRequest, StopReason};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::agent::{
    compact_v2::{config::CompactConfig, CompactOutcome},
    events::CompactStrategy,
    model_bridge::{map_model_error, AgentModelBridge},
};
use crate::error::AgentResult;
use crate::messages::BaseMessage;
use crate::session::transcript::MessageTranscript;
use crate::session::MessageFlags;
use crate::thread::CompactionChange;

// ─── 公共常量 ──────────────────────────────────────────────────────────────────

/// Full Compact 摘要 system prompt
const SUMMARY_SYSTEM_PROMPT: &str = include_str!("descriptions/summary_system_prompt.md");

/// Full Compact user prompt 模板
const SUMMARY_USER_PROMPT: &str = include_str!("descriptions/summary_user_prompt.md");

// 与普通输出恢复一样有界续写；不抬高 provider 的单次输出上限。
const MAX_SUMMARY_CONTINUATIONS: usize = 2;
const SUMMARY_CONTINUATION_PROMPT: &str = "Your summary was cut off by the output token limit. \
Continue exactly from the end of your previous text, including any unfinished word or tag. \
Output only the missing remainder; do not restart, repeat earlier sections, or add analysis. \
Finish the remaining essential facts concisely and close </summary>. Do not call tools.";

// ─── Full Compact ───────────────────────────────────────────────────────────────

/// Full Compact 内部实现
///
/// 步骤：
/// 1. 从包含 canonical reminder 的可见模型上下文派生摘要请求
/// 2. LLM 生成结构化摘要
/// 3. 后处理摘要
/// 4. 快照内自有的普通历史和 reminder 标 excluded（保留 System / ancestor）
/// 5. 追加 Human 摘要消息（带 CONTINUATION_HINT，wrap 在 system-reminder 标签中）
pub(super) async fn full_compact_inner(
    transcript: &mut MessageTranscript,
    llm: Option<&dyn peri_model::Model>,
    config: &CompactConfig,
    _cwd: &str,
) -> AgentResult<super::CompactResult> {
    let llm = llm.ok_or(crate::error::AgentError::CompactNoLlm)?;
    // Full 和 Reason 读取同一已提交视图。按摘要 provider 的协议保护 reasoning；恢复器只接受
    // 与工具输入/思考独立的 ToolResult 投影，不为摘要另做有损预览。
    let visible = super::projection::render_persisted_llm_view(
        transcript,
        &AgentModelBridge::projection_capabilities(llm),
    )?;
    let before_visible_len = visible.len();
    // 先固定本次快照的 own IDs；await 期间到达 inbox 的新结果不属于本次摘要。
    // ancestor 与 System 仍由各自 owner 管理；canonical reminder 不是豁免历史。
    let flag_updates: Vec<_> = transcript
        .entries()
        .iter()
        .skip(transcript.ancestor_len())
        .filter(|entry| !transcript.flags(entry.id()).excluded)
        .filter(|entry| !matches!(entry.as_message(), Some(BaseMessage::System { .. })))
        .map(|entry| {
            (
                entry.id(),
                MessageFlags {
                    excluded: true,
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
    let summary = if has_history {
        // 保留历史的角色、工具配对和完整正文；摘要指令只追加到派生请求，
        // 不回写原 transcript，也不提供可执行工具。
        let mut messages = AgentModelBridge::convert_messages(&visible)?;
        messages.insert(0, ModelMessage::system_text(SUMMARY_SYSTEM_PROMPT));
        messages.push(ModelMessage::user_text(SUMMARY_USER_PROMPT.replace(
            "{summary_target_tokens}",
            &(config.summary_max_tokens / 2).max(1).to_string(),
        )));
        let request = ModelRequest::new(messages).with_max_tokens(config.summary_max_tokens);
        complete_summary(llm, request).await?
    } else {
        // 全 System / 空历史仍保持命令输出 Human-first 的既有契约。
        "No conversation history to compact.".to_owned()
    };

    transcript
        .commit_compaction_lifecycle(CompactionChange {
            flag_updates,
            appended_messages: vec![build_summary_message(&summary)],
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

// 续写状态只属于本次摘要计算；完整成功前不向 canonical transcript 写半截摘要。
async fn complete_summary(
    llm: &dyn peri_model::Model,
    mut request: ModelRequest,
) -> AgentResult<String> {
    let mut text = String::new();
    for continuation in 0..=MAX_SUMMARY_CONTINUATIONS {
        let response = llm
            .complete(request.clone(), CancellationToken::new())
            .await
            .map_err(map_model_error)?;
        let part = response.assistant_text().unwrap_or_default();
        let has_tools = matches!(response.message(), ModelMessage::Assistant { content, tool_calls }
            if !tool_calls.is_empty() || content.iter().any(|block| matches!(block, peri_model::ContentBlock::ToolUse { .. })));
        if has_tools {
            return Err(crate::error::AgentError::CompactIncompleteResponse {
                stop_reason: StopReason::ToolUse,
            });
        }
        text.push_str(&part);
        match response.stop_reason() {
            StopReason::EndTurn => {
                // 空白尾响应不能证明已有半截正文完整，也不能触发外层空摘要重试。
                // 续写必须闭合 summary；跨响应拆开的标签在拼接之后才校验。
                if continuation > 0 && (part.trim().is_empty() || !text.contains("</summary>")) {
                    return Err(crate::error::AgentError::CompactIncompleteResponse {
                        stop_reason: StopReason::MaxTokens,
                    });
                }
                return postprocess_summary(&text).ok_or_else(|| {
                    warn!(
                        response_chars = text.chars().count(),
                        output_tokens = response.usage().map(|usage| usage.output_tokens),
                        "Full Compact response has no usable summary"
                    );
                    if continuation > 0 {
                        crate::error::AgentError::CompactIncompleteResponse {
                            stop_reason: StopReason::MaxTokens,
                        }
                    } else {
                        crate::error::AgentError::CompactEmptyResponse
                    }
                });
            }
            StopReason::MaxTokens
                if continuation < MAX_SUMMARY_CONTINUATIONS && !part.trim().is_empty() =>
            {
                warn!(
                    continuation = continuation + 1,
                    output_tokens = response.usage().map(|usage| usage.output_tokens),
                    "Full Compact output truncated; continuing the existing summary"
                );
                // 只续接可见正文；不重放未签名 reasoning 或任何工具调用。
                request.messages.push(ModelMessage::assistant_text(part));
                request
                    .messages
                    .push(ModelMessage::user_text(SUMMARY_CONTINUATION_PROMPT));
                // 给外层取消 select 明确的调度点，不在同步完成的模型替身上连跑请求。
                tokio::task::yield_now().await;
            }
            stop_reason => {
                return Err(crate::error::AgentError::CompactIncompleteResponse {
                    stop_reason: stop_reason.clone(),
                });
            }
        }
    }
    unreachable!("the final truncated response returns an error")
}

/// 构造 Full Compact 的 Human 摘要消息。
fn build_summary_message(summary: &str) -> BaseMessage {
    BaseMessage::human(format!(
        "{}\n\n{}",
        crate::agent::compact_v2::CONTINUATION_HINT,
        summary
    ))
}

/// 后处理 LLM 摘要输出：移除 analysis 块，提取 summary 块，添加前缀
///
/// # Safety
///
/// 本函数内部使用 `str::find` 返回的字节索引进行切片（`&text[..start]` 等）。
/// `<analysis>`、`</analysis>`、`<summary>`、`</summary>` 均为纯 ASCII 标签，
/// `find()` 返回的字节索引即字符边界，不会导致 panic。
fn postprocess_summary(raw: &str) -> Option<String> {
    let mut text = extract_summary_text(raw)?;

    let prefix = "This session continues from a previous conversation. Below is a summary of the prior dialogue.";

    text = text.trim().to_string();
    while text.contains("\n\n\n") {
        text = text.replace("\n\n\n", "\n\n");
    }

    if text.is_empty() {
        None
    } else {
        Some(format!("{}\n\n{}", prefix, text))
    }
}

// 只提取思考块之外的闭合 summary，正文中的标签可能是任务讨论的字面量。
// 没有闭合 summary 时保留原有回退：剥除配对思考块及未闭合思考尾部。
fn extract_summary_text(raw: &str) -> Option<String> {
    const TAGS: [(&str, &str, bool); 7] = [
        ("<summary>", "summary", true),
        ("<analysis>", "analysis", true),
        ("</analysis>", "analysis", false),
        ("<thinking>", "thinking", true),
        ("</thinking>", "thinking", false),
        ("<think>", "think", true),
        ("</think>", "think", false),
    ];
    let mut remaining = raw;
    let mut result = String::new();
    let mut stack = Vec::new();
    while let Some((position, tag, name, opening)) = TAGS
        .iter()
        .filter_map(|(tag, name, opening)| {
            remaining
                .find(tag)
                .map(|position| (position, *tag, *name, *opening))
        })
        .min_by_key(|(position, ..)| *position)
    {
        if stack.is_empty() {
            result.push_str(&remaining[..position]);
        }
        if name == "summary" {
            let body_start = position + tag.len();
            if stack.is_empty() {
                if let Some(body_len) = remaining[body_start..].find("</summary>") {
                    return Some(remaining[body_start..body_start + body_len].to_owned());
                }
                // 未闭合 summary 仍走后续思考块过滤，再沿用正文回退。
                result.push_str(tag);
            }
            remaining = &remaining[body_start..];
            continue;
        }
        if opening {
            stack.push(name);
        } else if stack.pop() != Some(name) {
            return None;
        }
        remaining = &remaining[position + tag.len()..];
    }
    if stack.is_empty() {
        result.push_str(remaining);
    }
    // 到此不存在可提取的闭合 summary；保留既有未闭合 summary 回退。
    if let Some(start) = result.find("<summary>") {
        result = result[start + "<summary>".len()..].to_owned();
    }
    Some(result)
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
