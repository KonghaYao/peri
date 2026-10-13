//! 摘要响应分类：把「可提交的完整摘要」「可回填的草稿片段」「本次无可用正文」
//! 与不可恢复的协议错误分开，供 Full Compact 的有界恢复循环决策。
//!
//! 分类只做结构与协议校验，不判断摘要语义是否完整；未闭合的输出一律不是
//! 可提交摘要，也不能用尾部省略标识伪装成完成。

use peri_model::{ContentBlock, ModelMessage, ModelResponse, StopReason};

use crate::error::{AgentError, AgentResult};
pub use peri_acp_types::error::SummaryRecoveryReason;

/// 一次摘要生成响应相对「可提交摘要」的分类。
#[derive(Debug)]
pub(super) enum SummaryStep {
    /// 已得到完整、通过校验的摘要正文。
    Complete(String),
    /// 有可用正文但未完成：`chunk` 按原顺序回填，不插入分隔符。
    Draft {
        chunk: String,
        reason: SummaryRecoveryReason,
    },
    /// 本次响应没有可用正文（空响应或仅思考内容）。
    Empty { reason: SummaryRecoveryReason },
}

/// 分类一次摘要响应。
///
/// `accumulated` 是此前已回填片段的原样拼接结果（不含本次响应），用于识别
/// 跨响应拆开的控制标签。工具调用、异常 stop 类型与控制标签不成对都是
/// 不可恢复的协议错误，不产生草稿。
pub(super) fn classify_summary_step(
    response: &ModelResponse,
    accumulated: &str,
) -> AgentResult<SummaryStep> {
    let stop_reason = response.stop_reason();
    let raw = summary_text(response)?;
    match stop_reason {
        StopReason::EndTurn | StopReason::MaxTokens => {}
        stop_reason => return Err(incomplete(stop_reason)),
    }
    let Some(extracted) = extract(&raw) else {
        // 控制标签不成对：协议不合法，不猜测正文，也不重新生成。
        return Err(incomplete(stop_reason));
    };
    // 本次响应自身给出的完整摘要优先：模型重写全文时不再拼接先前片段。
    if let Extracted::Closed(text) = &extracted {
        if let Some(text) = format_summary(text.clone()) {
            return Ok(SummaryStep::Complete(text));
        }
    }
    let body = extracted.into_text();
    let combined = format!("{accumulated}{body}");
    if let Some(text) = extract_closed_summary(&combined).and_then(format_summary) {
        return Ok(SummaryStep::Complete(text));
    }
    if body.trim().is_empty() {
        return Ok(SummaryStep::Empty {
            reason: SummaryRecoveryReason::EmptyResponse,
        });
    }
    if *stop_reason == StopReason::EndTurn
        && !has_summary_tag(accumulated)
        && !has_summary_tag(&body)
    {
        // 纯文本摘要兼容：模型完全不使用控制标签且自称结束。
        return format_summary(combined).map(SummaryStep::Complete).ok_or(
            AgentError::CompactIncompleteResponse {
                stop_reason: StopReason::EndTurn,
            },
        );
    }
    let reason = if *stop_reason == StopReason::MaxTokens {
        SummaryRecoveryReason::OutputLimitTruncated
    } else {
        SummaryRecoveryReason::UnclosedSummary
    };
    Ok(SummaryStep::Draft {
        chunk: body,
        reason,
    })
}

/// 是否出现摘要控制标签（用于区分「模型不用标签」与「标签未闭合」）。
fn has_summary_tag(text: &str) -> bool {
    text.contains("<summary>") || text.contains("</summary>")
}

fn summary_text(response: &ModelResponse) -> AgentResult<String> {
    if matches!(response.message(), ModelMessage::Assistant { content, tool_calls }
        if !tool_calls.is_empty() || content.iter().any(|block| matches!(block, ContentBlock::ToolUse { .. })))
    {
        return Err(incomplete(&StopReason::ToolUse));
    }
    Ok(response.assistant_text().unwrap_or_default())
}

fn incomplete(stop_reason: &StopReason) -> AgentError {
    AgentError::CompactIncompleteResponse {
        stop_reason: stop_reason.clone(),
    }
}

// ─── 思考块过滤与提取（单一权威）────────────────────────────────────────────────

/// 思考块过滤结果。
enum Extracted {
    /// 找到闭合 `<summary>...</summary>`：其内部正文（已剥离控制标签）。
    Closed(String),
    /// 没有闭合 summary：过滤思考块后的正文（保留未闭合控制标签）。
    Unclosed(String),
}

impl Extracted {
    /// 可见正文：闭合时取 summary 内部正文，否则取可拼接正文。
    fn into_text(self) -> String {
        match self {
            Self::Closed(text) | Self::Unclosed(text) => text,
        }
    }

    /// 仅闭合 summary 才有正文。
    fn into_closed(self) -> Option<String> {
        match self {
            Self::Closed(text) => Some(text),
            Self::Unclosed(_) => None,
        }
    }
}

fn format_summary(mut text: String) -> Option<String> {
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

fn extract_closed_summary(raw: &str) -> Option<String> {
    extract(raw).and_then(Extracted::into_closed)
}

/// 思考/分析控制标签：`(标签, 名称, 是否为开标签)`。
const TAGS: [(&str, &str, bool); 7] = [
    ("<summary>", "summary", true),
    ("<analysis>", "analysis", true),
    ("</analysis>", "analysis", false),
    ("<thinking>", "thinking", true),
    ("</thinking>", "thinking", false),
    ("<think>", "think", true),
    ("</think>", "think", false),
];

/// 扫描控制标签：闭合 summary 直接返回其正文；否则返回过滤思考块后的正文。
///
/// `None` = 出现未配对的思考/分析结束标签（协议不合法）。标签均为纯 ASCII，
/// `find()` 返回的字节索引即字符边界，切片不会 panic。
fn extract(raw: &str) -> Option<Extracted> {
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
                    return Some(Extracted::Closed(
                        remaining[body_start..body_start + body_len].to_owned(),
                    ));
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
    Some(Extracted::Unclosed(result))
}

#[cfg(test)]
#[path = "summary_test.rs"]
mod tests;
