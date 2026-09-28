//! Full Compact + Re-inject 实现
//!
//! 完整流程：
//! 1. 从包含 canonical reminder 的可见模型上下文派生摘要请求
//! 2. LLM 生成结构化摘要
//! 3. 后处理摘要
//! 4. 快照内自有的普通历史和 reminder 标 excluded（保留 System / ancestor）
//! 5. 追加 Human 摘要消息（带 CONTINUATION_HINT，wrap 在 system-reminder 标签中）
//! 6. Re-inject 关键文件 + Skills（如果 cwd 提供）

use std::path::Path;

use peri_model::{ModelMessage, ModelRequest};
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

// ─── Full Compact ───────────────────────────────────────────────────────────────

/// Full Compact 内部实现
///
/// 步骤：
/// 1. 从包含 canonical reminder 的可见模型上下文派生摘要请求
/// 2. LLM 生成结构化摘要
/// 3. 后处理摘要
/// 4. 快照内自有的普通历史和 reminder 标 excluded（保留 System / ancestor）
/// 5. 追加 Human 摘要消息（带 CONTINUATION_HINT，wrap 在 system-reminder 标签中）
/// 6. Re-inject 关键文件（如果 cwd 提供）
pub(super) async fn full_compact_inner(
    transcript: &mut MessageTranscript,
    llm: Option<&dyn peri_model::Model>,
    config: &CompactConfig,
    cwd: &str,
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
        messages.push(ModelMessage::user_text(SUMMARY_USER_PROMPT));
        let request = ModelRequest::new(messages).with_max_tokens(config.summary_max_tokens);
        let response = llm
            .complete(request, CancellationToken::new())
            .await
            .map_err(map_model_error)?;
        if !matches!(response.stop_reason(), peri_model::StopReason::EndTurn) {
            return Err(crate::error::AgentError::CompactIncompleteResponse {
                stop_reason: response.stop_reason().clone(),
            });
        }
        let text = response.assistant_text().unwrap_or_default();
        postprocess_summary(&text).ok_or_else(|| {
            // 仅记录形状与 usage，区分无正文和 analysis-only，不输出历史或摘要正文。
            warn!(
                response_chars = text.chars().count(),
                output_tokens = response.usage().map(|usage| usage.output_tokens),
                "Full Compact response has no usable summary"
            );
            crate::error::AgentError::CompactEmptyResponse
        })?
    } else {
        // 全 System / 空历史仍保持命令输出 Human-first 的既有契约。
        "No conversation history to compact.".to_owned()
    };

    // 6. 先收集 re-inject 消息，随后和摘要一次性提交。
    let re_inject_result = if has_history {
        collect_reinject_v2(transcript, config, cwd).await
    } else {
        ReInjectResult::default()
    };
    debug!(
        files_injected = re_inject_result.files_injected,
        skills_injected = re_inject_result.skills_injected,
        "Full Compact: re-inject 完成"
    );

    let mut appended_messages = vec![build_summary_message(&summary)];
    appended_messages.extend(re_inject_result.messages);
    transcript
        .commit_compaction_lifecycle(CompactionChange {
            flag_updates,
            appended_messages,
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
        after_visible, "Full Compact: excluded 旧消息 + 追加摘要 + re-inject"
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
    let mut text = strip_reasoning_blocks(raw)?;

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

    if text.is_empty() {
        None
    } else {
        Some(format!("{}\n\n{}", prefix, text))
    }
}

// 精确识别协议思考标签并按栈配对，不能把嵌套块残留的 closing tag 当作摘要。
// 普通文字与相近的标签名保持原样；未闭合思考块的尾部不属于可提交正文。
fn strip_reasoning_blocks(raw: &str) -> Option<String> {
    const TAGS: [(&str, &str, bool); 6] = [
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
    Some(result)
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
/// 文件候选仅来自当前可见消息；Skills 保持从全部历史 entries 收集。
async fn collect_reinject_v2(
    transcript: &MessageTranscript,
    config: &CompactConfig,
    cwd: &str,
) -> ReInjectResult {
    let visible_messages: Vec<BaseMessage> = transcript
        .entries()
        .iter()
        .filter(|entry| !transcript.flags(entry.id()).excluded)
        .filter_map(|entry| entry.as_message().cloned())
        .collect();
    let all_messages: Vec<BaseMessage> = transcript
        .entries()
        .iter()
        .filter_map(|entry| entry.as_message().cloned())
        .collect();

    let mut result_messages: Vec<BaseMessage> = Vec::new();

    // 1. 提取并注入最近读取的文件
    let file_paths = extract_recent_files(&visible_messages, config.re_inject_max_files);
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
            let human_content = format!("[最近读取的文件: {}]\n{}", path, content);
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
            let human_content = format!("[激活的 Skill 指令: {}]\n{}", path, content);
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

/// 从 re_inject 消息提取文件/Skill 信息（事实源 peri-acp-types::compact）
pub use peri_acp_types::compact::{extract_file_info, extract_skill_names};

#[cfg(test)]
#[path = "full_test.rs"]
mod tests;

#[cfg(test)]
#[path = "full_report_test.rs"]
mod report_tests;
