use peri_model::{ContentBlock, ModelMessage, ModelResponse, StopReason};

use crate::error::{AgentError, AgentResult};

pub(super) enum SummaryAssembly {
    Complete { text: String, truncated: bool },
    Continue,
}

pub(super) fn assemble_summary(
    first: &ModelResponse,
    second: Option<&ModelResponse>,
) -> AgentResult<SummaryAssembly> {
    let first_text = summary_text(first)?;
    match first.stop_reason() {
        StopReason::EndTurn => {
            return postprocess_summary(&first_text)
                .map(|text| SummaryAssembly::Complete {
                    text,
                    truncated: false,
                })
                .ok_or(AgentError::CompactEmptyResponse);
        }
        StopReason::MaxTokens => {}
        stop_reason => return Err(incomplete(stop_reason)),
    }
    if let Some(text) = extract_closed_summary(&first_text).and_then(format_summary) {
        return Ok(SummaryAssembly::Complete {
            text,
            truncated: false,
        });
    }
    if first_text.trim().is_empty() {
        return Err(incomplete(&StopReason::MaxTokens));
    }
    let Some(second) = second else {
        return Ok(SummaryAssembly::Continue);
    };
    let second_text = summary_text(second)?;
    let combined = format!("{first_text}{second_text}");
    match second.stop_reason() {
        StopReason::EndTurn if !second_text.trim().is_empty() => extract_closed_summary(&combined)
            .and_then(format_summary)
            .map(|text| SummaryAssembly::Complete {
                text,
                truncated: false,
            })
            .ok_or_else(|| incomplete(&StopReason::MaxTokens)),
        StopReason::MaxTokens => {
            if let Some(text) = extract_closed_summary(&combined).and_then(format_summary) {
                return Ok(SummaryAssembly::Complete {
                    text,
                    truncated: false,
                });
            }
            let text =
                postprocess_summary(&combined).ok_or_else(|| incomplete(&StopReason::MaxTokens))?;
            Ok(SummaryAssembly::Complete {
                text: format!("{text}\n\n[Summary output limit reached after one continuation. The remaining tail was omitted; consult the original transcript for missing details.]"),
                truncated: true,
            })
        }
        StopReason::EndTurn => Err(incomplete(&StopReason::MaxTokens)),
        stop_reason => Err(incomplete(stop_reason)),
    }
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

/// 后处理 LLM 摘要输出：移除 analysis 块，提取 summary 块，添加前缀
///
/// # Safety
///
/// 本函数内部使用 `str::find` 返回的字节索引进行切片（`&text[..start]` 等）。
/// `<analysis>`、`</analysis>`、`<summary>`、`</summary>` 均为纯 ASCII 标签，
/// `find()` 返回的字节索引即字符边界，不会导致 panic。
fn postprocess_summary(raw: &str) -> Option<String> {
    format_summary(extract_summary_text(raw)?)
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

// 只提取思考块之外的闭合 summary，正文中的标签可能是任务讨论的字面量。
// 没有闭合 summary 时保留原有回退：剥除配对思考块及未闭合思考尾部。
fn extract_summary_text(raw: &str) -> Option<String> {
    extract_summary(raw, false)
}

fn extract_closed_summary(raw: &str) -> Option<String> {
    extract_summary(raw, true)
}

fn extract_summary(raw: &str, require_closed: bool) -> Option<String> {
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
    if require_closed {
        return None;
    }
    // 到此不存在可提取的闭合 summary；保留既有未闭合 summary 回退。
    if let Some(start) = result.find("<summary>") {
        result = result[start + "<summary>".len()..].to_owned();
    }
    Some(result)
}

#[cfg(test)]
#[path = "summary_test.rs"]
mod tests;
