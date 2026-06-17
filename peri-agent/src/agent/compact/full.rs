use tracing::warn;

use crate::{
    agent::compact::{
        config::CompactConfig,
        invariant::{group_messages_by_round, MessageRound},
    },
    error::{AgentError, AgentResult},
    llm::{types::LlmRequest, BaseModel},
    messages::{BaseMessage, ContentBlock, MessageContent},
};

/// 结构化摘要 system prompt
const SYSTEM_PROMPT: &str =
    "You are a conversation context compression tool. You excel at compressing long conversations into structured summaries.";

/// 结构化摘要 user prompt 模板
const USER_PROMPT_TEMPLATE: &str = r#"Analyze the following conversation history and produce a structured summary covering these areas:

<analysis>
1. **Primary Request and Intent** — The user's core request and intent
2. **Key Technical Concepts** — Technical concepts and frameworks involved
3. **Files and Code Sections** — File paths operated on and key code snippets (preserve exact absolute paths from the working directory above)
4. **Errors and Fixes** — Errors encountered and how they were fixed
5. **Problem Solving** — Problem-solving approach and process
6. **All User Messages** — Summary of all user messages
7. **Pending Tasks** — Tasks that remain incomplete
8. **Current Work** — What is currently being worked on
9. **Optional Next Step** — Suggested next action
</analysis>

<summary>
Based on the analysis above, generate a concise structured summary. Preserve all file paths (always use absolute paths), error messages, and key decisions. Use Markdown format.
</summary>"#;

/// Full Compact 执行结果
#[derive(Debug, Clone)]
pub struct FullCompactResult {
    pub summary: String,
    pub messages_used: usize,
}

/// 按字符数截断，超出时添加 "...(已截断)" 后缀
fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let end: String = s.chars().take(max).collect();
        format!("{}...(truncated)", end)
    } else {
        s.to_string()
    }
}

/// 将 content 中的 Image block 替换为 [image] 文本，然后截断到 max_chars 字符
fn replace_images_and_truncate(content: &MessageContent, max_chars: usize) -> String {
    let blocks = content.content_blocks();
    let parts: Vec<String> = blocks
        .iter()
        .map(|b| match b {
            ContentBlock::Image { .. } => "[image]".to_string(),
            _ => match b {
                ContentBlock::Text { text } => text.clone(),
                ContentBlock::ToolUse { name, input, .. } => {
                    format!("调用 {}({})", name, input)
                }
                ContentBlock::Reasoning { text, .. } => text.clone(),
                _ => format!("{:?}", b),
            },
        })
        .collect();
    let full = parts.join("\n");
    truncate_str(&full, max_chars)
}

/// 将工具调用格式化为包含关键参数的摘要。
///
/// 提取路径相关字段（file_path/path/folder_path）和命令字段（command/pattern），
/// 让摘要 LLM 能保留精确的文件路径，避免 compact 后 agent 丢失路径上下文。
fn format_tool_call_summary(tc: &crate::messages::ToolCallRequest) -> String {
    let args = &tc.arguments;
    let key_fields = ["file_path", "path", "folder_path", "command", "pattern"];
    let mut parts = Vec::new();
    for field in &key_fields {
        if let Some(val) = args.get(*field).and_then(|v| v.as_str()) {
            // 字符级截断，避免 CJK 字符 panic
            let truncated: String = val.chars().take(200).collect();
            let display = if truncated.chars().count() < val.chars().count() {
                format!("{}...", truncated)
            } else {
                truncated
            };
            parts.push(format!("{}=\"{}\"", field, display));
        }
    }
    if parts.is_empty() {
        tc.name.clone()
    } else {
        format!("{}({})", tc.name, parts.join(", "))
    }
}

/// 工具结果摘要：保留首行 + 关键路径/错误信息，丢弃冗长堆栈/调试输出。
///
/// 策略：
/// - 标记 `[error]`/`[ok]` 状态（从 is_error 字段读取）
/// - 保留前 `first_lines` 行（首行通常是错误摘要/路径，价值密度最高）
/// - 启发式提取文件路径（绝对路径或 Unix 相对路径），避免摘要 LLM 丢失操作目标
/// - 全部输出做最终 `max_chars` 截断
fn format_tool_result_summary(
    tool_call_id: &str,
    content: &MessageContent,
    is_error: bool,
    first_lines: usize,
    max_chars: usize,
) -> String {
    let status = if is_error { "error" } else { "ok" };
    let raw = match content
        .content_blocks()
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => text.clone(),
            ContentBlock::ToolUse { name, input, .. } => {
                format!("调用 {}({})", name, input)
            }
            ContentBlock::Reasoning { text, .. } => text.clone(),
            _ => format!("{:?}", b),
        })
        .collect::<Vec<_>>()
        .join("\n")
    {
        s if !s.is_empty() => s,
        _ => return format!("[ToolResult:{}][{}]", tool_call_id, status),
    };

    // 取前 N 行（首行价值密度最高：错误摘要/路径/状态码）
    let head: String = raw
        .lines()
        .take(first_lines)
        .collect::<Vec<&str>>()
        .join(" | ");

    // 启发式提取路径：匹配常见路径模式（绝对路径、./相对路径、~/家目录）
    // 让摘要 LLM 能保留操作目标，避免 P1-4 描述的"操作哪个文件"丢失问题
    let mut paths: Vec<String> = Vec::new();
    for line in raw.lines() {
        for token in line.split_whitespace() {
            let trimmed = token.trim_matches(|c: char| {
                c == '"' || c == '\'' || c == '`' || c == ',' || c == ';' || c == ':'
            });
            // 匹配 /abs/path, ./rel/path, ~/home/path 三种模式
            let is_path_like = trimmed.starts_with('/')
                || trimmed.starts_with("./")
                || trimmed.starts_with("~/")
                || (trimmed.starts_with('.') && trimmed.contains('/'));
            if is_path_like && trimmed.len() > 2 && !paths.contains(&trimmed.to_string()) {
                paths.push(trimmed.to_string());
                if paths.len() >= 3 {
                    break;
                }
            }
        }
        if paths.len() >= 3 {
            break;
        }
    }

    let mut out = format!("[ToolResult:{}][{}]", tool_call_id, status);
    out.push_str(&format!(" {}", head));
    if !paths.is_empty() {
        out.push_str(&format!(" paths=[{}]", paths.join(", ")));
    }
    truncate_str(&out, max_chars)
}

/// 预处理消息：跳过 System、替换 Image block 为 [image]、截断每条消息
fn preprocess_messages(messages: &[BaseMessage], truncate_chars: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for msg in messages {
        match msg {
            BaseMessage::System { .. } => {}
            BaseMessage::Human { .. } => {
                let content = replace_images_and_truncate(msg.message_content(), truncate_chars);
                lines.push(format!("[User] {}", content));
            }
            BaseMessage::Ai { tool_calls, .. } => {
                let text = replace_images_and_truncate(msg.message_content(), truncate_chars);
                let line = if tool_calls.is_empty() {
                    format!("[Assistant] {}", text)
                } else {
                    let tool_summaries: Vec<String> =
                        tool_calls.iter().map(format_tool_call_summary).collect();
                    format!(
                        "[Assistant] {}（tools: {}）",
                        text,
                        tool_summaries.join(", ")
                    )
                };
                lines.push(line);
            }
            BaseMessage::Tool {
                tool_call_id,
                is_error,
                ..
            } => {
                let content = msg.message_content();
                lines.push(format_tool_result_summary(
                    tool_call_id,
                    content,
                    *is_error,
                    3,
                    truncate_chars,
                ));
            }
        }
    }
    lines
}

/// 移除 <analysis>...</analysis> 块
fn remove_analysis_blocks(text: &str) -> String {
    let mut result = text.to_string();
    loop {
        let start_tag = "<analysis>";
        let end_tag = "</analysis>";
        if let Some(start) = result.find(start_tag) {
            if let Some(end) = result[start..].find(end_tag) {
                let remove_end = start + end + end_tag.len();
                result = format!("{}{}", &result[..start], &result[remove_end..]);
            } else {
                result = result[..start].to_string();
                break;
            }
        } else {
            break;
        }
    }
    result
}

/// 提取 <summary>...</summary> 标签内的内容
fn extract_summary_content(text: &str) -> Option<String> {
    let start_tag = "<summary>";
    let end_tag = "</summary>";
    let start = text.find(start_tag)?;
    let content_start = start + start_tag.len();
    if let Some(end) = text[content_start..].find(end_tag) {
        Some(text[content_start..content_start + end].trim().to_string())
    } else {
        Some(text[content_start..].trim().to_string())
    }
}

/// 后处理 LLM 输出
fn postprocess_summary(raw: &str) -> String {
    let mut text = raw.to_string();

    text = remove_analysis_blocks(&text);

    if let Some(summary_content) = extract_summary_content(&text) {
        text = summary_content;
    }

    let prefix = "This session continues from a previous conversation. Below is a summary of the prior dialogue.";

    text = text.trim().to_string();
    while text.contains("\n\n\n") {
        text = text.replace("\n\n\n", "\n\n");
    }

    format!("{}\n\n{}", prefix, text)
}

/// 判断错误是否为 PTL（Prompt Too Long）错误
fn is_ptl_error(error: &crate::error::AgentError) -> bool {
    let msg = error.to_string().to_lowercase();
    msg.contains("prompt_too_long")
        || msg.contains("context_length_exceeded")
        || msg.contains("max_context_window")
        || msg.contains("token limit")
        || msg.contains("too many tokens")
}

/// PTL 降级：从最旧的 round 开始删除指定数量的消息组
///
/// 始终保留开头的所有 System 消息（包含旧摘要等关键上下文），
/// 只从第一个非 System 消息开始截断。
fn truncate_for_ptl(
    messages: &[BaseMessage],
    rounds: &[MessageRound],
    drop_count: usize,
) -> Vec<BaseMessage> {
    if rounds.len() <= 1 || drop_count == 0 {
        return messages.to_vec();
    }

    // 找到第一条非 System 消息的位置
    let first_non_system = messages
        .iter()
        .position(|m| !matches!(m, BaseMessage::System { .. }))
        .unwrap_or(messages.len());

    // 在 rounds 中找到第一个包含非 System 消息的 round 的起始索引
    let non_system_round_start = rounds
        .iter()
        .position(|r| r.start >= first_non_system)
        .unwrap_or(0);

    let droppable_rounds = rounds.len().saturating_sub(non_system_round_start);
    if droppable_rounds <= 1 {
        return messages.to_vec();
    }

    let actual_drop = drop_count.min(droppable_rounds - 1);
    let drop_end = rounds[non_system_round_start + actual_drop - 1].end;

    // 拼接：保留所有 System 消息 + 截断后的非 System 消息
    let mut result = messages[..first_non_system].to_vec();
    result.extend_from_slice(&messages[drop_end..]);
    result
}

/// 执行 Full Compact：预处理 -> LLM 摘要 -> 后处理，支持 PTL 降级重试
pub async fn full_compact(
    messages: &[BaseMessage],
    model: &dyn BaseModel,
    config: &CompactConfig,
    instructions: &str,
    cwd: &str,
) -> AgentResult<FullCompactResult> {
    let non_system_count = messages
        .iter()
        .filter(|m| !matches!(m, BaseMessage::System { .. }))
        .count();

    if non_system_count == 0 {
        return Ok(FullCompactResult {
            summary: postprocess_summary("## Summary\n(No valid conversation history)"),
            messages_used: messages.len(),
        });
    }

    let mut current_messages: Vec<BaseMessage> = messages.to_vec();
    let max_retries = config.ptl_max_retries as usize;

    for attempt in 0..=max_retries {
        let truncated = preprocess_messages(&current_messages, 2000);
        let conversation_text = truncated.join("\n");

        let mut user_content = format!(
            "Compress the following conversation history:\n<conversation>\n{}\n</conversation>\n\n\
             Current working directory: {}\n\n{}",
            conversation_text, cwd, USER_PROMPT_TEMPLATE
        );

        if !instructions.trim().is_empty() {
            user_content.push_str(&format!(
                "\n\nPay special attention to: {}",
                instructions.trim()
            ));
        }

        let request = LlmRequest::new(vec![BaseMessage::human(user_content)])
            .with_system(SYSTEM_PROMPT.to_string())
            .with_max_tokens(config.summary_max_tokens);

        match model.invoke(request).await {
            Ok(response) => {
                let raw_summary = response.message.content();
                if raw_summary.trim().is_empty() {
                    tracing::warn!("Full Compact: LLM 返回空摘要，跳过压缩");
                    return Err(AgentError::Other(anyhow::anyhow!(
                        "Full Compact 失败：LLM 返回空摘要"
                    )));
                }
                let summary = postprocess_summary(&raw_summary);
                return Ok(FullCompactResult {
                    summary,
                    messages_used: current_messages.len(),
                });
            }
            Err(e) if is_ptl_error(&e) && attempt < max_retries => {
                warn!(
                    attempt = attempt + 1,
                    max_retries, "Full Compact PTL 降级：prompt 过长，删除最旧消息组后重试"
                );

                let rounds = group_messages_by_round(&current_messages);
                let truncated_messages = truncate_for_ptl(&current_messages, &rounds, 1);
                // 截断无变化 → 无法继续降级，立即返回错误
                if truncated_messages.len() == current_messages.len() {
                    return Err(AgentError::Other(anyhow::anyhow!(
                        "Full Compact PTL 降级失败：消息已无法进一步缩减 ({})",
                        e
                    )));
                }
                current_messages = truncated_messages;
            }
            Err(e) => {
                return Err(AgentError::Other(anyhow::anyhow!(
                    "Full Compact 失败（PTL 降级重试 {} 次后仍失败）: {}",
                    attempt,
                    e
                )));
            }
        }
    }

    // 所有 attempt 在循环内均有 return（Ok 或 Err），此处不可达
    unreachable!("full_compact loop should always return within the loop body")
}

#[cfg(test)]
#[path = "full_test.rs"]
mod tests;
