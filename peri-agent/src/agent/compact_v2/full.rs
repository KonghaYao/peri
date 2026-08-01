//! Full Compact + Re-inject 实现
//!
//! 完整流程：
//! 1. 预处理可见消息为文本
//! 2. LLM 生成结构化摘要
//! 3. 后处理摘要
//! 4. 所有旧消息标 excluded
//! 5. 追加 Human 摘要消息（带 CONTINUATION_HINT，wrap 在 system-reminder 标签中）
//! 6. Re-inject 关键文件 + Skills（如果 cwd 提供）

use std::path::Path;

use peri_model::{ModelMessage, ModelRequest};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::agent::{
    compact_v2::{config::CompactConfig, CompactOutcome},
    events::{CompactFileInfo, CompactStrategy},
    model_bridge::map_model_error,
};
use crate::error::AgentResult;
use crate::messages::{BaseMessage, ContentBlock, MessageContent};
use crate::session::transcript::{MessageFlags, MessageTranscript};
use crate::thread::CompactionLifecycle;

// ─── 公共常量 ──────────────────────────────────────────────────────────────────

/// Full Compact 摘要 system prompt
const SUMMARY_SYSTEM_PROMPT: &str = include_str!("descriptions/summary_system_prompt.md");

/// Full Compact user prompt 模板
const SUMMARY_USER_PROMPT: &str = include_str!("descriptions/summary_user_prompt.md");

// ─── Full Compact ───────────────────────────────────────────────────────────────

/// Full Compact 内部实现
///
/// 步骤：
/// 1. 预处理可见消息为文本
/// 2. LLM 生成结构化摘要
/// 3. 后处理摘要
/// 4. 所有旧消息标 excluded
/// 5. 追加 Human 摘要消息（带 CONTINUATION_HINT，wrap 在 system-reminder 标签中）
/// 6. Re-inject 关键文件（如果 cwd 提供）
pub(super) async fn full_compact_inner(
    transcript: &mut MessageTranscript,
    llm: Option<&dyn peri_model::Model>,
    config: &CompactConfig,
    cwd: &str,
) -> AgentResult<super::CompactResult> {
    let before_len = transcript.len();

    // 无 LLM 时降级为 Micro
    let llm = llm.ok_or(crate::error::AgentError::CompactNoLlm)?;

    // 收集可见消息用于预处理
    let visible: Vec<&BaseMessage> = transcript.visible_messages();
    let non_system_count = visible
        .iter()
        .filter(|m| !matches!(m, BaseMessage::System { .. }))
        .count();

    if non_system_count == 0 {
        // 无有效对话历史——生成 fallback 摘要（保证 compact 后首条仍为 Human）
        // 这是 v1 build_summary_human_message + re_inject 的不变量：
        //   即使全 System history，compact 输出仍以 Human(fallback 摘要) 开头
        // 详见 peri-acp/src/session/command/compact_test.rs::test_contract_all_system_history_still_human_first
        let fallback_summary = "No conversation history to compact.".to_string();
        let summary_message = build_summary_message(&fallback_summary);
        transcript
            .commit_compaction_lifecycle(CompactionLifecycle {
                flag_updates: Vec::new(),
                appended_messages: vec![summary_message],
            })
            .await?;
        transcript.mark_full_compaction_committed();
        let after_visible = transcript.visible_messages().len();

        return Ok(super::CompactResult {
            strategy: CompactStrategy::Full,
            affected_count: before_len,
            estimated_tokens_saved: 0,
            before_visible_len: before_len,
            after_visible_len: after_visible,
            summary: Some(fallback_summary),
            full_escalation_reason: None,
            outcome: CompactOutcome::FullApplied,
            changed_messages: 0,
            changed_fields: 0,
            no_op_candidates: 0,
        });
    }

    // 1. 预处理消息为文本序列
    let lines = preprocess_messages_for_summary(&visible, 2000);
    let conversation_text = lines.join("\n");

    // 2. 构造 LLM 请求
    let user_content = format!(
        "Compress the following conversation history:\n<conversation>\n{}\n</conversation>\n\n{}",
        conversation_text, SUMMARY_USER_PROMPT
    );

    let request = ModelRequest::new(vec![
        ModelMessage::system_text(SUMMARY_SYSTEM_PROMPT),
        ModelMessage::user_text(user_content),
    ])
    .with_max_tokens(config.summary_max_tokens);

    // 3. 调用 LLM（走标准链路）
    let response = llm
        .complete(request, CancellationToken::new())
        .await
        .map_err(map_model_error)?;
    let raw_summary = response.assistant_text().unwrap_or_default();

    if raw_summary.trim().is_empty() {
        return Err(crate::error::AgentError::CompactEmptyResponse);
    }

    // 4. 后处理摘要
    let summary = postprocess_summary(&raw_summary);

    // 5. 构造原子生命周期：仅排除 own region 中的非 System 原消息。
    //    不根据 visible filter 丢失已被排除的原文。
    let flag_updates = transcript
        .entries()
        .iter()
        .skip(transcript.ancestor_len())
        .filter(|entry| !matches!(entry.message, BaseMessage::System { .. }))
        .map(|entry| {
            (
                entry.message.id(),
                MessageFlags {
                    excluded: true,
                    ..Default::default()
                },
            )
        })
        .collect();

    // 6. 先收集 re-inject 消息，随后和摘要一次性提交。
    let re_inject_result = collect_reinject_v2(transcript, config, cwd).await;
    debug!(
        files_injected = re_inject_result.files_injected,
        skills_injected = re_inject_result.skills_injected,
        "Full Compact: re-inject 完成"
    );

    let mut appended_messages = vec![build_summary_message(&summary)];
    appended_messages.extend(re_inject_result.messages);
    transcript
        .commit_compaction_lifecycle(CompactionLifecycle {
            flag_updates,
            appended_messages,
        })
        .await?;
    transcript.mark_full_compaction_committed();

    let after_visible = transcript.visible_messages().len();

    debug!(
        before_len,
        after_visible, "Full Compact: excluded 旧消息 + 追加摘要 + re-inject"
    );

    Ok(super::CompactResult {
        strategy: CompactStrategy::Full,
        affected_count: before_len,
        estimated_tokens_saved: 0,
        before_visible_len: before_len,
        after_visible_len: after_visible,
        summary: Some(summary),
        full_escalation_reason: None,
        outcome: CompactOutcome::FullApplied,
        changed_messages: 0,
        changed_fields: 0,
        no_op_candidates: 0,
    })
}

/// 构造 Full Compact 的 Human 摘要消息。
fn build_summary_message(summary: &str) -> BaseMessage {
    let hint_text = format!(
        "<system-reminder>\n{}\n\n{}\n</system-reminder>",
        crate::agent::compact_v2::CONTINUATION_HINT,
        summary
    );
    BaseMessage::human(hint_text)
}

/// 预处理消息为文本行（供 LLM 摘要使用）
///
/// 跳过 System 消息；Image/Document 替换为占位符；按字符级截断。
fn preprocess_messages_for_summary(messages: &[&BaseMessage], max_chars: usize) -> Vec<String> {
    let mut lines = Vec::new();

    for msg in messages {
        match msg {
            BaseMessage::System { .. } => continue,
            BaseMessage::Human { .. } => {
                let content = replace_images_and_truncate(msg.message_content(), max_chars);
                lines.push(format!("[User] {}", content));
            }
            BaseMessage::Ai { tool_calls, .. } => {
                let text = replace_images_and_truncate(msg.message_content(), max_chars);
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
                    max_chars,
                ));
            }
        }
    }

    lines
}

/// 将 content 中的 Image/Document 替换为占位符文本，并按字符级截断
fn replace_images_and_truncate(content: &MessageContent, max_chars: usize) -> String {
    let blocks = content.content_blocks();
    let parts: Vec<String> = blocks
        .iter()
        .map(|b| match b {
            ContentBlock::Image { .. } => "[image]".to_string(),
            ContentBlock::Document { .. } => "[document]".to_string(),
            ContentBlock::Text { text } => text.clone(),
            ContentBlock::ToolUse { name, input, .. } => {
                format!("调用 {}({})", name, input)
            }
            ContentBlock::Reasoning { text, .. } => text.clone(),
            _ => format!("{:?}", b),
        })
        .collect();
    let full = parts.join("\n");
    truncate_str(&full, max_chars)
}

/// 工具调用摘要：保留名称和关键参数
fn format_tool_call_summary(tc: &crate::messages::ToolCallRequest) -> String {
    let args = &tc.arguments;
    let key_fields = ["file_path", "path", "folder_path", "command", "pattern"];
    let mut parts = Vec::new();
    for field in &key_fields {
        if let Some(val) = args.get(*field).and_then(|v| v.as_str()) {
            // 字符级截断，避免 CJK panic
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

/// 工具结果摘要：保留状态 + 首行 + 关键路径
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

    // 取前 N 行
    let head: String = raw
        .lines()
        .take(first_lines)
        .collect::<Vec<&str>>()
        .join(" | ");

    let mut out = format!("[ToolResult:{}][{}]", tool_call_id, status);
    out.push_str(&format!(" {}", head));
    truncate_str(&out, max_chars)
}

/// 按字符数截断，超出时添加 "...(truncated)" 后缀
fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let end: String = s.chars().take(max).collect();
        format!("{}...(truncated)", end)
    } else {
        s.to_string()
    }
}

/// 后处理 LLM 摘要输出：移除 analysis 块，提取 summary 块，添加前缀
///
/// # Safety
///
/// 本函数内部使用 `str::find` 返回的字节索引进行切片（`&text[..start]` 等）。
/// `<analysis>`、`</analysis>`、`<summary>`、`</summary>` 均为纯 ASCII 标签，
/// `find()` 返回的字节索引即字符边界，不会导致 panic。
fn postprocess_summary(raw: &str) -> String {
    let mut text = raw.to_string();

    // 移除 <analysis>...</analysis> 块
    loop {
        let start_tag = "<analysis>";
        let end_tag = "</analysis>";
        if let Some(start) = text.find(start_tag) {
            if let Some(end) = text[start..].find(end_tag) {
                let remove_end = start + end + end_tag.len();
                // Safety: <analysis> 为纯 ASCII 标签，字节索引即字符边界
                text = format!("{}{}", &text[..start], &text[remove_end..]);
            } else {
                // Safety: <analysis> 为纯 ASCII 标签，字节索引即字符边界
                text = text[..start].to_string();
                break;
            }
        } else {
            break;
        }
    }

    // 提取 <summary>...</summary> 内容
    if let Some(start) = text.find("<summary>") {
        let content_start = start + "<summary>".len();
        if let Some(end) = text[content_start..].find("</summary>") {
            // Safety: <summary>/</summary> 为纯 ASCII 标签，字节索引即字符边界
            text = text[content_start..content_start + end].trim().to_string();
        } else {
            // Safety: <summary> 为纯 ASCII 标签，字节索引即字符边界
            text = text[content_start..].trim().to_string();
        }
    }

    let prefix = "This session continues from a previous conversation. Below is a summary of the prior dialogue.";

    text = text.trim().to_string();
    while text.contains("\n\n\n") {
        text = text.replace("\n\n\n", "\n\n");
    }

    format!("{}\n\n{}", prefix, text)
}

// ─── Re-inject ──────────────────────────────────────────────────────────────────

/// Full Compact 后重新注入的关键信息结果
#[derive(Debug, Clone, Default)]
pub struct ReInjectResult {
    /// 注入的消息列表（文件 + Skills，已按顺序排列）
    pub messages: Vec<BaseMessage>,
    /// 成功注入的文件数量
    pub files_injected: usize,
    /// 成功注入的 Skills 数量
    pub skills_injected: usize,
}

/// 判断路径是否为 Skills 目录下的 SKILL.md 文件
fn is_skills_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    normalized.contains("/.claude/skills/")
        || (normalized.contains("/skills/") && normalized.ends_with("SKILL.md"))
}

/// 从消息历史中提取最近通过 Read 工具读取的文件路径（去重，保留最新）
fn extract_recent_files(messages: &[BaseMessage], max_files: usize) -> Vec<String> {
    let mut seen = std::collections::HashSet::<String>::new();
    let mut paths = Vec::new();

    for msg in messages.iter().rev() {
        for tc in msg.tool_calls() {
            if tc.name == "Read" {
                let path = tc
                    .arguments
                    .get("file_path")
                    .and_then(|v| v.as_str())
                    .or_else(|| tc.arguments.get("path").and_then(|v| v.as_str()));
                if let Some(path) = path {
                    if is_skills_path(path) {
                        continue;
                    }
                    if seen.insert(path.to_string()) {
                        paths.push(path.to_string());
                        if paths.len() >= max_files {
                            return paths;
                        }
                    }
                }
            }
        }
    }

    paths
}

/// 从消息历史中提取 SkillPreloadMiddleware 注入的 Skills 路径（去重，保留出现顺序）
fn extract_skills_paths(messages: &[BaseMessage]) -> Vec<String> {
    let mut seen = std::collections::HashSet::<String>::new();
    let mut paths = Vec::new();

    for msg in messages.iter() {
        for tc in msg.tool_calls() {
            if tc.name == "Read" {
                let path = tc
                    .arguments
                    .get("file_path")
                    .and_then(|v| v.as_str())
                    .or_else(|| tc.arguments.get("path").and_then(|v| v.as_str()));
                if let Some(path) = path {
                    if is_skills_path(path) && seen.insert(path.to_string()) {
                        paths.push(path.to_string());
                    }
                }
            }
        }

        let text = match msg {
            BaseMessage::System { content, .. } | BaseMessage::Human { content, .. } => {
                content.text_content()
            }
            _ => continue,
        };
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("[Skill: ") {
                if let Some(path) = rest.strip_suffix(']') {
                    let trimmed = path.trim();
                    if is_skills_path(trimmed) && seen.insert(trimmed.to_string()) {
                        paths.push(trimmed.to_string());
                    }
                }
            }
        }
    }

    paths
}

/// 异步读取文件并截断到指定 token 预算（字符数 / 4 估算）
async fn read_file_with_budget(path: &str, max_tokens: u32) -> Option<String> {
    let path_owned = path.to_string();
    let content = tokio::task::spawn_blocking(move || std::fs::read_to_string(&path_owned))
        .await
        .ok()?
        .ok()?;

    let max_chars = max_tokens as usize * 4;
    if content.chars().count() > max_chars {
        let truncated: String = content.chars().take(max_chars).collect();
        debug!(path, max_tokens, "文件内容截断到 {} 字符", max_chars);
        Some(format!("{}...(已截断)", truncated))
    } else {
        Some(content)
    }
}

/// 按总 token 预算截断内容列表，返回保留的条目数
fn truncate_to_budget(contents: &mut Vec<(String, String)>, budget: u32) -> usize {
    let budget_chars = budget as usize * 4;
    let mut used_chars = 0;
    let mut keep_count = 0;

    for (_, content) in contents.iter() {
        let chars = content.chars().count();
        if used_chars + chars > budget_chars {
            break;
        }
        used_chars += chars;
        keep_count += 1;
    }

    contents.truncate(keep_count);
    keep_count
}

/// 解析相对路径为绝对路径（基于 cwd）
fn resolve_path(path: &str, cwd: &str) -> String {
    if Path::new(path).is_absolute() {
        path.to_string()
    } else {
        let abs = Path::new(cwd).join(path);
        abs.to_string_lossy().to_string()
    }
}

/// Full Compact 后重新注入关键信息（文件 + Skills）。
///
/// 保留既有公共行为：收集消息后普通追加到 transcript 末尾。
pub async fn re_inject_v2(
    transcript: &mut MessageTranscript,
    config: &CompactConfig,
    cwd: &str,
) -> ReInjectResult {
    let result = collect_reinject_v2(transcript, config, cwd).await;
    for message in &result.messages {
        transcript.append(message.clone());
    }
    result
}

/// 收集 Full Compact 后需要重新注入的关键信息（文件 + Skills）。
///
/// 从 transcript 全部 entries（含已标 excluded 的旧消息）中提取：
/// 1. 最近 Read 的非 Skills 文件 → 注入为消息
/// 2. SkillPreloadMiddleware 注入的 Skills 路径 → 注入为消息
async fn collect_reinject_v2(
    transcript: &MessageTranscript,
    config: &CompactConfig,
    cwd: &str,
) -> ReInjectResult {
    // 收集全部消息（含 excluded 的旧消息——它们是 compact 前的对话历史）
    let all_messages: Vec<BaseMessage> = transcript
        .entries()
        .iter()
        .map(|e| e.message.clone())
        .collect();

    let mut result_messages: Vec<BaseMessage> = Vec::new();

    // 1. 提取并注入最近读取的文件
    let file_paths = extract_recent_files(&all_messages, config.re_inject_max_files);
    let mut files_injected = 0;

    if !file_paths.is_empty() {
        let resolved_paths: Vec<String> = file_paths.iter().map(|p| resolve_path(p, cwd)).collect();

        let mut file_futures = Vec::new();
        for path in &resolved_paths {
            file_futures.push(read_file_with_budget(
                path,
                config.re_inject_max_tokens_per_file,
            ));
        }
        let file_contents: Vec<Option<String>> = futures::future::join_all(file_futures).await;

        let mut valid_files: Vec<(String, String)> = Vec::new();
        for (path, content) in file_paths.iter().zip(file_contents) {
            if let Some(content) = content {
                valid_files.push((path.clone(), content));
            } else {
                debug!(path, "文件读取失败或不存在，跳过重新注入");
            }
        }

        truncate_to_budget(&mut valid_files, config.re_inject_file_budget);

        for (path, content) in &valid_files {
            // 用 Human 消息（而非 System）避免 LLM invoke hoist 污染 frozen prompt
            let human_content = format!(
                "[最近读取的文件: {}]\n<system-reminder>\n{}\n</system-reminder>",
                path, content
            );
            result_messages.push(BaseMessage::human(human_content));
        }
        files_injected = valid_files.len();
    }

    // 2. 提取并注入激活的 Skills
    let skills_paths = extract_skills_paths(&all_messages);
    let mut skills_injected = 0;

    if !skills_paths.is_empty() {
        let resolved_skill_paths: Vec<String> =
            skills_paths.iter().map(|p| resolve_path(p, cwd)).collect();

        let mut skill_futures = Vec::new();
        for path in &resolved_skill_paths {
            skill_futures.push(read_file_with_budget(
                path,
                config.re_inject_max_tokens_per_file,
            ));
        }
        let skill_contents: Vec<Option<String>> = futures::future::join_all(skill_futures).await;

        let mut valid_skills: Vec<(String, String)> = Vec::new();
        for (path, content) in skills_paths.iter().zip(skill_contents) {
            if let Some(content) = content {
                valid_skills.push((path.clone(), content));
            } else {
                warn!(path, "Skill 文件读取失败，跳过重新注入");
            }
        }

        truncate_to_budget(&mut valid_skills, config.re_inject_skills_budget);

        for (path, content) in &valid_skills {
            let human_content = format!(
                "[激活的 Skill 指令: {}]\n<system-reminder>\n{}\n</system-reminder>",
                path, content
            );
            result_messages.push(BaseMessage::human(human_content));
        }
        skills_injected = valid_skills.len();
    }

    debug!(
        files_injected,
        skills_injected,
        total_messages = result_messages.len(),
        "v2 重新注入完成"
    );

    ReInjectResult {
        messages: result_messages,
        files_injected,
        skills_injected,
    }
}

/// 从 re_inject 消息中提取文件信息（CompactCompleted 事件用）
pub fn extract_file_info(messages: &[BaseMessage]) -> Vec<CompactFileInfo> {
    let mut files = Vec::new();
    for msg in messages {
        let content = msg.content();
        if let Some(rest) = content.strip_prefix("[最近读取的文件: ") {
            let path = rest.lines().next().unwrap_or("");
            let line_count = rest.lines().count().saturating_sub(1);
            if !path.is_empty() {
                files.push(CompactFileInfo {
                    path: path.to_string(),
                    lines: line_count,
                });
            }
        }
    }
    files
}

/// 从 re_inject 消息中提取 Skill 名称（CompactCompleted 事件用）
pub fn extract_skill_names(messages: &[BaseMessage]) -> Vec<String> {
    let mut skills = Vec::new();
    for msg in messages {
        let content = msg.content();
        if let Some(rest) = content.strip_prefix("[激活的 Skill 指令: ") {
            let name = rest.lines().next().unwrap_or("");
            if !name.is_empty() {
                skills.push(name.to_string());
            }
        }
    }
    skills
}

#[cfg(test)]
#[path = "full_test.rs"]
mod tests;
