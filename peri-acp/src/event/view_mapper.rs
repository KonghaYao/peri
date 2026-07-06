//! BaseMessage -> ViewModel conversion with incremental caching.
//!
//! This is the ACP-layer implementation of the `ViewMapper` trait defined in
//! `router.rs`. It converts a `Vec<BaseMessage>` into `Vec<ViewModel>` (the
//! pure-DTO contract type from `peri-acp-types`), using an internal cache so
//! that only newly appended messages are converted on each call.
//!
//! ## Mapping rules (migrated from `peri-tui/ui/message_view/build.rs`)
//!
//! | BaseMessage variant   | ViewModel variant      | Notes                                     |
//! |-----------------------|------------------------|--------------------------------------------|
//! | Human                 | UserBubble             | Compact `<system-reminder>` detected       |
//! | Ai (text blocks)      | AssistantBubble        | ToolUse blocks extracted as sibling cards   |
//! | Ai (reasoning blocks) | AssistantBubble        | Reasoning preserved with collapsed flag    |
//! | Ai (image/document)   | AssistantBubble        | Placeholder text                           |
//! | Tool (Agent)          | SubAgentGroup          | Task preview + result                      |
//! | Tool (other)          | ToolCard               | Paired via `tool_call_id` → prev AI's `tool_calls` |
//! | System                | SystemNote             | Info level                                 |

use peri_acp_types::view_model::{
    hash_str, AskUserBlockData, AskUserItem, AssistantBubbleData, DiffBlock, Hunk, HunkLine,
    HunkLineKind, NoteLevel, ReasoningBlock, SubAgentGroupData, SystemNoteData, ToolCardData,
    UserBubbleData, ViewModel,
};
use peri_agent::agent::compact::CONTINUATION_HINT;
use peri_agent::messages::{BaseMessage, ContentBlock};

use super::router::ViewMapper;

// ---------------------------------------------------------------------------
// ViewMapper impl
// ---------------------------------------------------------------------------

/// Caching converter from `BaseMessage` slices to `ViewModel` lists.
///
/// On each `convert()` call, only the suffix of messages beyond the cached
/// prefix is converted. If the input shrinks (e.g. after rewind), the entire
/// cache is invalidated.
pub struct ViewMapperImpl {
    /// Number of messages already converted and cached.
    cached_count: usize,
    /// Cached view models from prior conversions.
    cached: Vec<ViewModel>,
}

impl ViewMapperImpl {
    pub fn new() -> Self {
        Self {
            cached_count: 0,
            cached: Vec::new(),
        }
    }

    /// Discard all cached state (e.g. after a rewind or new session).
    pub fn reset(&mut self) {
        self.cached_count = 0;
        self.cached.clear();
    }
}

impl Default for ViewMapperImpl {
    fn default() -> Self {
        Self::new()
    }
}

impl ViewMapper for ViewMapperImpl {
    /// Convert a full message list. Uses cache for the prefix, only converts
    /// the new suffix. Returns the complete `Vec<ViewModel>` for "view-commit"
    /// data.
    fn convert(&mut self, messages: &[BaseMessage]) -> Vec<ViewModel> {
        // If messages shrunk (e.g. after rewind), invalidate cache.
        if messages.len() < self.cached_count {
            self.cached_count = 0;
            self.cached.clear();
        }

        // Build a lookup from tool_call_id → (tool_name, input) for all Ai
        // messages seen so far. This is needed for Tool messages that only
        // store `tool_call_id` but not the tool name or arguments.
        let prev_ai_tool_calls = collect_tool_calls(messages);

        // Convert only new messages beyond the cached prefix.
        for msg in &messages[self.cached_count..] {
            let vm = convert_one(msg, &prev_ai_tool_calls);
            self.cached.push(vm);
        }
        self.cached_count = messages.len();
        self.cached.clone()
    }
}

// ---------------------------------------------------------------------------
// Helper: collect all (id, name, input) from every Ai message's tool_calls
// ---------------------------------------------------------------------------

type ToolCallEntry = (String, String, serde_json::Value);

fn collect_tool_calls(messages: &[BaseMessage]) -> Vec<ToolCallEntry> {
    let mut out = Vec::new();
    for msg in messages {
        if let BaseMessage::Ai { tool_calls, .. } = msg {
            for tc in tool_calls {
                out.push((tc.id.clone(), tc.name.clone(), tc.arguments.clone()));
            }
        }
        // Also scan ContentBlock::ToolUse inside content blocks (some providers
        // store tool_use info only in blocks, not in the top-level tool_calls vec).
        for block in msg.content_blocks() {
            if let ContentBlock::ToolUse {
                id, name, input, ..
            } = block
            {
                // Avoid duplicate if already captured from tool_calls.
                if !out.iter().any(|(eid, _, _)| eid == &id) {
                    out.push((id, name, input));
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Core conversion: single BaseMessage → ViewModel
// ---------------------------------------------------------------------------

/// Convert a single `BaseMessage` into a `ViewModel`.
///
/// `prev_ai_tool_calls` is the full list of `(tool_call_id, tool_name, input)`
/// tuples collected from all Ai messages in the transcript. This is needed
/// because `BaseMessage::Tool` only stores `tool_call_id`, not the tool name
/// or arguments.
fn convert_one(msg: &BaseMessage, prev_ai_tool_calls: &[ToolCallEntry]) -> ViewModel {
    match msg {
        BaseMessage::Human { content, .. } => convert_human(content),

        BaseMessage::Ai {
            content,
            tool_calls,
            ..
        } => convert_ai(content, tool_calls),

        BaseMessage::Tool {
            tool_call_id,
            content,
            is_error,
            ..
        } => convert_tool(tool_call_id, content, *is_error, prev_ai_tool_calls),

        BaseMessage::System { content, .. } => convert_system(content),
    }
}

// ---------------------------------------------------------------------------
// Human message
// ---------------------------------------------------------------------------

fn convert_human(content: &peri_agent::messages::MessageContent) -> ViewModel {
    let raw = content.text_content();

    // Detect compact summary: <system-reminder> tag + CONTINUATION_HINT marker.
    // Bare <system-reminder> tags are also used for goal steering,
    // tool_dispatch consecutive-failure warnings, hooks stop_hook_feedback,
    // etc. (see CLAUDE.md TRAP). Only treat as compact when the hint is present.
    let is_system_reminder = raw.contains("<system-reminder>");
    let (display_text, is_compact) = if is_system_reminder && raw.contains(CONTINUATION_HINT) {
        let cleaned = raw
            .replacen("<system-reminder>\n", "", 1)
            .replacen("\n</system-reminder>", "", 1)
            .trim()
            .to_string();
        (cleaned, true)
    } else {
        (raw, false)
    };

    if is_compact {
        // Compact summary → SystemNote with Info level (matches build.rs behavior
        // where system_reminder=true is set on UserBubble; in the DTO layer we
        // use SystemNote to communicate this distinction without adding a field).
        ViewModel::SystemNote(SystemNoteData {
            text: display_text.clone(),
            level: NoteLevel::Info,
            content_hash: hash_str(&format!("{}|Info", display_text)),
        })
    } else {
        ViewModel::UserBubble(UserBubbleData {
            text: display_text.clone(),
            content_hash: hash_str(&display_text),
            is_system_reminder,
        })
    }
}

// ---------------------------------------------------------------------------
// AI message
// ---------------------------------------------------------------------------

fn convert_ai(
    content: &peri_agent::messages::MessageContent,
    tool_calls: &[peri_agent::messages::ToolCallRequest],
) -> ViewModel {
    let blocks = content.content_blocks();

    // Collect text fragments, reasoning blocks, and tool-use IDs.
    let mut text_parts: Vec<String> = Vec::new();
    let mut reasoning: Option<ReasoningBlock> = None;
    let mut tool_card_ids: Vec<String> = Vec::new();

    for block in &blocks {
        match block {
            ContentBlock::Text { text } => {
                text_parts.push(text.clone());
            }
            ContentBlock::Reasoning { text, .. } => {
                reasoning = Some(ReasoningBlock {
                    text: text.clone(),
                    collapsed: true,
                });
            }
            ContentBlock::ToolUse { id, .. } => {
                tool_card_ids.push(id.clone());
            }
            ContentBlock::Image { .. } => {
                text_parts.push("[Image]".to_string());
            }
            ContentBlock::Document { title, .. } => {
                let label = title.as_deref().unwrap_or("Document");
                text_parts.push(format!("[Document: {}]", label));
            }
            ContentBlock::Unknown(v) => {
                let type_name = v.get("type").and_then(|t| t.as_str()).unwrap_or("unknown");
                text_parts.push(format!("[{}]", type_name));
            }
            // ToolResult inside Ai message is unusual; skip silently (matches build.rs).
            ContentBlock::ToolResult { .. } => {}
        }
    }

    // Also capture tool_call_ids from the top-level `tool_calls` field that
    // might not have corresponding ContentBlock::ToolUse entries.
    let block_ids: std::collections::HashSet<String> = tool_card_ids.iter().cloned().collect();
    for tc in tool_calls {
        if !block_ids.contains(&tc.id) {
            tool_card_ids.push(tc.id.clone());
        }
    }

    let text = text_parts.join("");

    let reasoning_hash_text = reasoning
        .as_ref()
        .map(|r| r.text.clone())
        .unwrap_or_default();
    let content_hash = hash_str(&format!("{}|{}", text, reasoning_hash_text));
    ViewModel::AssistantBubble(AssistantBubbleData {
        text: text.clone(),
        reasoning,
        tool_card_ids,
        content_hash,
    })
}

// ---------------------------------------------------------------------------
// Tool result message
// ---------------------------------------------------------------------------

fn convert_tool(
    tool_call_id: &str,
    content: &peri_agent::messages::MessageContent,
    is_error: bool,
    prev_ai_tool_calls: &[ToolCallEntry],
) -> ViewModel {
    // Look up tool name and input from the preceding Ai message's tool_calls.
    let (tool_name, input) = prev_ai_tool_calls
        .iter()
        .find(|(id, _, _)| id == tool_call_id)
        .map(|(_, name, input)| (name.clone(), input.clone()))
        .unwrap_or_else(|| (tool_call_id.to_string(), serde_json::Value::Null));

    let raw_content = content.text_content();

    // Agent tool → SubAgentGroup (matches build.rs logic).
    if tool_name == "Agent" {
        return convert_agent_tool(&tool_name, &input, &raw_content, is_error, tool_call_id);
    }

    // AskUserQuestion → AskUserBlock
    if tool_name == "AskUserQuestion" {
        return convert_ask_user_tool(&input, &raw_content, is_error);
    }

    // Build summaries (replicates tool_display helpers from TUI).
    let tool_name_str = tool_name.as_str();
    let input_summary = summarize_input(tool_name_str, &input);
    let output_summary = summarize_output(tool_name_str, &raw_content);

    // Diff for Write/Edit tools (successful only).
    let mut diff = if is_error {
        None
    } else {
        build_diff_block(tool_name_str, &input)
    };

    // Annotate diff with output-bound metadata (binary / too-large detection).
    if let Some(ref mut d) = diff {
        d.is_binary = raw_content.contains("Binary");
        d.is_too_large = raw_content.contains("too large");
    }

    let diff_path = diff.as_ref().map(|d| d.path.as_str()).unwrap_or("");
    let content_hash_input = format!(
        "{}|{}|{}|{}|{}|{}|{}",
        tool_call_id, tool_name, input_summary, output_summary, is_error, false, diff_path,
    );

    ViewModel::ToolCard(ToolCardData {
        tool_id: tool_call_id.to_string(),
        tool_name,
        input_summary,
        output_summary,
        is_error,
        is_running: false,
        diff,
        content_hash: hash_str(&content_hash_input),
    })
}

// ---------------------------------------------------------------------------
// System message
// ---------------------------------------------------------------------------

fn convert_system(content: &peri_agent::messages::MessageContent) -> ViewModel {
    let text = content.text_content();
    ViewModel::SystemNote(SystemNoteData {
        text: text.clone(),
        level: NoteLevel::Info,
        content_hash: hash_str(&format!("{}|Info", text)),
    })
}

// ---------------------------------------------------------------------------
// Agent (SubAgent) tool result
// ---------------------------------------------------------------------------

fn convert_agent_tool(
    tool_name: &str,
    input: &serde_json::Value,
    _raw_content: &str,
    is_error: bool,
    _tool_call_id: &str,
) -> ViewModel {
    let agent_id = input
        .get("subagent_type")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("fork")
        .to_string();

    let _task_preview = input["prompt"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(40)
        .collect::<String>();

    let _is_background = _raw_content.starts_with("Background task");

    // SubAgentGroup in the DTO layer has minimal fields compared to TUI's
    // MessageViewModel::SubAgentGroup. The TUI-side SubAgentGroup is built
    // from streaming events (SubagentStarted/SubagentStopped) rather than
    // from the Tool message. Here we emit a SubAgentGroup placeholder so the
    // view-commit has a slot for it.
    ViewModel::SubAgentGroup(SubAgentGroupData {
        agent_id: agent_id.clone(),
        agent_name: tool_name.to_string(),
        view_models: Vec::new(),
        collapsed: false,
        is_running: false,
        content_hash: hash_str(&format!("{}|{}|0|{}|false", agent_id, tool_name, !is_error)),
    })
}

// ---------------------------------------------------------------------------
// AskUserQuestion → AskUserBlock conversion
// ---------------------------------------------------------------------------

fn convert_ask_user_tool(
    input: &serde_json::Value,
    raw_content: &str,
    is_error: bool,
) -> ViewModel {
    let mut items = Vec::new();

    // Extract questions from tool input: {"questions": [{"header": "...", "id": "..."}, ...]}
    let questions: Vec<String> = input
        .get("questions")
        .and_then(|q| q.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|q| {
                    q.get("header")
                        .and_then(|h| h.as_str())
                        .map(|s| s.to_string())
                })
                .collect()
        })
        .unwrap_or_default();

    // Try to pair answers from raw_content (user response text).
    // Format is typically question-per-line with answers following.
    let answers: Vec<&str> = raw_content.lines().filter(|l| !l.is_empty()).collect();

    // Pair questions with answers by index match.
    for (i, header) in questions.into_iter().enumerate() {
        let answer = answers.get(i).map(|s| s.to_string()).unwrap_or_default();
        items.push(AskUserItem { header, answer });
    }

    // If no structured questions found, emit a single item with raw content as answer.
    if items.is_empty() {
        items.push(AskUserItem {
            header: "Questions".to_string(),
            answer: raw_content.to_string(),
        });
    }

    let content_hash = hash_str(&format!(
        "askuser|{}|{}",
        if is_error { "err" } else { "ok" },
        raw_content.len(),
    ));

    ViewModel::AskUserBlock(AskUserBlockData {
        items,
        is_error,
        content_hash,
    })
}

// ---------------------------------------------------------------------------
// Input / Output summary helpers
// ---------------------------------------------------------------------------
//
// These replicate the logic from `peri-tui/app/tool_display.rs` (format_tool_name,
// format_tool_args, summarize_output) without depending on the TUI crate.

/// Produce a one-line summary of a tool's JSON input.
fn summarize_input(name: &str, input: &serde_json::Value) -> String {
    let obj = match input {
        serde_json::Value::Object(map) => map,
        other => return truncate_chars(&other.to_string(), 120),
    };
    let str_val = |key: &str| -> String {
        obj.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    match name {
        // ── 无前缀，文件路径不截断 ──
        "Read" | "Write" | "Edit" => {
            let p = str_val("file_path");
            if p.is_empty() {
                str_val("path")
            } else {
                p
            }
        }
        // ── 无前缀，命令截断 400 ──
        "Bash" => truncate_chars(&str_val("command"), 400),
        // ── 有前缀 pattern:，pattern 截断 200 ──
        "Glob" | "Grep" => {
            let p = str_val("pattern");
            let p = if p.is_empty() { str_val("query") } else { p };
            format!("pattern: {}", truncate_chars(&p, 200))
        }
        // ── "operation folder_path"，不截断 ──
        "folder_operations" => {
            let op = str_val("operation");
            let fp = str_val("folder_path");
            format!("{} {}", op, fp)
        }
        // ── query: 截断 60 ──
        "WebSearch" => {
            format!("query: {}", truncate_chars(&str_val("query"), 60))
        }
        // ── url: 不截断 ──
        "WebFetch" => {
            format!("url: {}", str_val("url"))
        }
        // ── 空字符串（文档无参数）──
        "TodoWrite" => String::new(),
        // ── task_id 截断 12 ──
        "AgentResult" => truncate_chars(&str_val("task_id"), 12),
        // ── file_path 不截断 ──
        "artifact" => str_val("file_path"),
        // ── operation 截断 40 ──
        "LSP" => truncate_chars(&str_val("operation"), 40),
        // ── tool_name 截断 40 ──
        "ExecuteExtraTool" => truncate_chars(&str_val("tool_name"), 40),
        // ── query 截断 40 ──
        "SearchExtraTools" => truncate_chars(&str_val("query"), 40),
        // ── 兜底：第一个非空字段，截断 100 ──
        _ => {
            if let Some((k, v)) = obj.iter().next() {
                let raw = v.as_str().unwrap_or("");
                format!("{}: {}", k, truncate_chars(raw, 100))
            } else {
                "(empty input)".to_string()
            }
        }
    }
}

/// Produce a one-line summary of a tool's output.
fn summarize_output(name: &str, output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    match name {
        "Edit" | "Write" => {
            let lines = trimmed.lines().count();
            if lines <= 3 {
                return truncate_text(trimmed, 200);
            }
            format!("{} lines changed", lines)
        }
        "WebFetch" => {
            let lines = trimmed.lines().count();
            let bytes = output.len();
            format!(
                "{} lines · {} bytes\n{}",
                lines,
                bytes,
                truncate_text(trimmed, 400)
            )
        }
        // TodoWrite 返回全量内容（显示完整 todo 列表）
        "TodoWrite" => trimmed.to_string(),
        // Read / Glob / Grep — 折叠态显示行数
        "Read" | "Glob" | "Grep" => {
            let lines = trimmed.lines().count();
            format!("{} lines", lines)
        }
        _ => truncate_text(trimmed, 200),
    }
}

/// Truncate text to `max_chars` Unicode code points (CJK-safe).
fn truncate_text(s: &str, max_chars: usize) -> String {
    let len = s.chars().count();
    if len <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}...", truncated)
    }
}

/// Truncate text to `max_chars` Unicode code points (CJK-safe).
/// Same logic as `truncate_text`, distinct name for tool-input truncation.
fn truncate_chars(s: &str, max_chars: usize) -> String {
    let len = s.chars().count();
    if len <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}...", truncated)
    }
}

// ---------------------------------------------------------------------------
// Diff block builder (Write / Edit tools)
// ---------------------------------------------------------------------------
//
// The TUI version uses `peri_widgets::diff::render_diff` to produce terminal
// `Line<'static>` output. In the DTO layer we produce a structured
// `DiffBlock` instead, so the consumer (TUI or IDE) can render it in its own
// style. The diff content is a simple unified-diff string extracted from the
// tool input; full semantic parsing is left to the consumer.

fn build_diff_block(name: &str, input: &serde_json::Value) -> Option<DiffBlock> {
    let (file_path, old_content, new_content) = match name {
        "Edit" => {
            let old_string = input
                .get("old_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let new_string = input
                .get("new_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let file_path = input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if old_string.is_empty() || file_path.is_empty() {
                return None;
            }
            (
                file_path.to_string(),
                old_string.to_string(),
                new_string.to_string(),
            )
        }
        "Write" => {
            let content = input.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let file_path = input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if content.is_empty() || file_path.is_empty() {
                return None;
            }
            (file_path.to_string(), String::new(), content.to_string())
        }
        _ => return None,
    };

    // Build a minimal structured diff from old/new content.
    // This is a simplified unified-diff representation. The consumer can
    // re-render with full diff algorithm if needed.
    let hunks = build_simple_diff(&old_content, &new_content);
    if hunks.is_empty() {
        return None;
    }

    let is_new_file = name == "Write" || old_content.is_empty();

    Some(DiffBlock {
        path: file_path,
        hunks,
        is_binary: false,
        is_too_large: false,
        is_new_file,
    })
}

/// Build a simple line-by-line diff between old and new content.
///
/// This produces a single hunk with add/del/context lines. It is intentionally
/// simple -- the TUI has its own `peri_widgets::diff::render_diff` for
/// production-quality rendering. The DTO diff is a structured fallback for
/// non-TUI consumers.
fn build_simple_diff(old: &str, new: &str) -> Vec<Hunk> {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    // Simple LCS-based line diff to classify lines as context/add/del.
    let diff_lines = simple_line_diff(&old_lines, &new_lines);

    if diff_lines.is_empty() {
        return Vec::new();
    }

    let old_count = old_lines.len() as u32;
    let new_count = new_lines.len() as u32;

    // Count context/add/del lines for the range headers.
    let old_start = 1u32;
    let new_start = 1u32;
    let mut old_line_no = 1u32;
    let mut new_line_no = 1u32;

    let mut hunk_lines: Vec<HunkLine> = Vec::new();

    for (kind, text) in diff_lines {
        match kind {
            HunkLineKind::Context => {
                hunk_lines.push(HunkLine {
                    kind,
                    text: text.to_string(),
                    old_no: Some(old_line_no),
                    new_no: Some(new_line_no),
                });
                old_line_no += 1;
                new_line_no += 1;
            }
            HunkLineKind::Del => {
                hunk_lines.push(HunkLine {
                    kind,
                    text: text.to_string(),
                    old_no: Some(old_line_no),
                    new_no: None,
                });
                old_line_no += 1;
            }
            HunkLineKind::Add => {
                hunk_lines.push(HunkLine {
                    kind,
                    text: text.to_string(),
                    old_no: None,
                    new_no: Some(new_line_no),
                });
                new_line_no += 1;
            }
        }
    }

    vec![Hunk {
        old_range: format!("{}-{}", old_start, old_count),
        new_range: format!("{}-{}", new_start, new_count),
        lines: hunk_lines,
    }]
}

/// Very simple line diff: classifies each line of the shorter input, then
/// appends the remainder of the longer input as add/del. Not a real LCS --
/// just enough to produce structured diff data for the DTO.
fn simple_line_diff<'a>(old: &'a [&'a str], new: &'a [&'a str]) -> Vec<(HunkLineKind, &'a str)> {
    let mut result = Vec::new();
    let min_len = old.len().min(new.len());

    // Compare line by line up to the shorter length.
    for i in 0..min_len {
        if old[i] == new[i] {
            result.push((HunkLineKind::Context, old[i]));
        } else {
            result.push((HunkLineKind::Del, old[i]));
            result.push((HunkLineKind::Add, new[i]));
        }
    }

    // Remaining old lines (deleted).
    for i in min_len..old.len() {
        result.push((HunkLineKind::Del, old[i]));
    }

    // Remaining new lines (added).
    for i in min_len..new.len() {
        result.push((HunkLineKind::Add, new[i]));
    }

    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use peri_agent::messages::{BaseMessage, MessageContent};

    // ── ViewMapperImpl cache behavior ──────────────────────────────────────

    #[test]
    fn test_empty_messages() {
        let mut mapper = ViewMapperImpl::new();
        let vms = mapper.convert(&[]);
        assert!(vms.is_empty());
    }

    #[test]
    fn test_single_human_message() {
        let mut mapper = ViewMapperImpl::new();
        let msgs = vec![BaseMessage::human("hello")];
        let vms = mapper.convert(&msgs);
        assert_eq!(vms.len(), 1);
        match &vms[0] {
            ViewModel::UserBubble(d) => assert_eq!(d.text, "hello"),
            other => panic!("expected UserBubble, got {:?}", other),
        }
    }

    #[test]
    fn test_cache_reuses_prefix() {
        let mut mapper = ViewMapperImpl::new();

        let msgs1: Vec<BaseMessage> =
            vec![BaseMessage::human("first"), BaseMessage::ai("response")];
        let vms1 = mapper.convert(&msgs1);
        assert_eq!(vms1.len(), 2);

        // Append one more message — only the new one should be converted.
        let msgs2: Vec<BaseMessage> = vec![
            BaseMessage::human("first"),
            BaseMessage::ai("response"),
            BaseMessage::human("second"),
        ];
        let vms2 = mapper.convert(&msgs2);
        assert_eq!(vms2.len(), 3);
    }

    #[test]
    fn test_shrink_invalidates_cache() {
        let mut mapper = ViewMapperImpl::new();

        let msgs1: Vec<BaseMessage> = vec![
            BaseMessage::human("a"),
            BaseMessage::human("b"),
            BaseMessage::human("c"),
        ];
        let _ = mapper.convert(&msgs1);

        // Shrink — cache should be invalidated.
        let msgs2: Vec<BaseMessage> = vec![BaseMessage::human("a")];
        let vms2 = mapper.convert(&msgs2);
        assert_eq!(vms2.len(), 1);
    }

    #[test]
    fn test_reset_clears_cache() {
        let mut mapper = ViewMapperImpl::new();
        let msgs = vec![BaseMessage::human("hello")];
        let _ = mapper.convert(&msgs);
        mapper.reset();
        let vms = mapper.convert(&[]);
        assert!(vms.is_empty());
    }

    // ── Human message ──────────────────────────────────────────────────────

    #[test]
    fn test_human_plain_text() {
        let msg = BaseMessage::human("hello world");
        let vm = convert_one(&msg, &[]);
        match vm {
            ViewModel::UserBubble(d) => assert_eq!(d.text, "hello world"),
            other => panic!("expected UserBubble, got {:?}", other),
        }
    }

    #[test]
    fn test_human_compact_reminder_becomes_system_note() {
        let hint = CONTINUATION_HINT;
        let content = format!(
            "<system-reminder>\nCompact summary here. {}\n</system-reminder>",
            hint
        );
        let msg = BaseMessage::human(MessageContent::text(content));
        let vm = convert_one(&msg, &[]);
        match vm {
            ViewModel::SystemNote(d) => {
                assert!(d.text.contains("Compact summary here"));
                assert_eq!(d.level, NoteLevel::Info);
            }
            other => panic!("expected SystemNote, got {:?}", other),
        }
    }

    #[test]
    fn test_human_bare_system_reminder_stays_user_bubble() {
        // A bare <system-reminder> without CONTINUATION_HINT should remain
        // UserBubble (used for goal steering, hook feedback, etc.).
        let content = "<system-reminder>\nGoal: fix the bug\n</system-reminder>".to_string();
        let msg = BaseMessage::human(MessageContent::text(content));
        let vm = convert_one(&msg, &[]);
        assert!(matches!(vm, ViewModel::UserBubble(_)));
    }

    // ── AI message ────────────────────────────────────────────────────────

    #[test]
    fn test_ai_text_only() {
        let msg = BaseMessage::ai("thinking...");
        let vm = convert_one(&msg, &[]);
        match vm {
            ViewModel::AssistantBubble(d) => {
                assert_eq!(d.text, "thinking...");
                assert!(d.reasoning.is_none());
                assert!(d.tool_card_ids.is_empty());
            }
            other => panic!("expected AssistantBubble, got {:?}", other),
        }
    }

    #[test]
    fn test_ai_with_reasoning() {
        let blocks = vec![
            ContentBlock::Reasoning {
                text: "deep thought".into(),
                signature: None,
            },
            ContentBlock::text("answer"),
        ];
        let msg = BaseMessage::ai(MessageContent::blocks(blocks));
        let vm = convert_one(&msg, &[]);
        match vm {
            ViewModel::AssistantBubble(d) => {
                assert_eq!(d.text, "answer");
                assert!(d.reasoning.is_some());
                let r = d.reasoning.unwrap();
                assert_eq!(r.text, "deep thought");
                assert!(r.collapsed);
                assert!(d.tool_card_ids.is_empty());
            }
            other => panic!("expected AssistantBubble, got {:?}", other),
        }
    }

    #[test]
    fn test_ai_with_tool_use_emits_tool_card_ids() {
        let blocks = vec![
            ContentBlock::text("I'll edit the file."),
            ContentBlock::ToolUse {
                id: "tc-42".into(),
                name: "Edit".into(),
                input: serde_json::json!({"file_path": "foo.rs"}),
            },
        ];
        let msg = BaseMessage::ai(MessageContent::blocks(blocks));
        let vm = convert_one(&msg, &[]);
        match vm {
            ViewModel::AssistantBubble(d) => {
                assert_eq!(d.tool_card_ids, vec!["tc-42".to_string()]);
            }
            other => panic!("expected AssistantBubble, got {:?}", other),
        }
    }

    #[test]
    fn test_ai_tool_calls_field_also_captured() {
        // Some providers put tool info in tool_calls but not in content blocks.
        let msg_with_tc = BaseMessage::ai_with_tool_calls(
            MessageContent::text("doing work"),
            vec![peri_agent::messages::ToolCallRequest::new(
                "tc-99",
                "Bash",
                serde_json::json!({"command": "cargo build"}),
            )],
        );
        let vm = convert_one(&msg_with_tc, &[]);
        match vm {
            ViewModel::AssistantBubble(d) => {
                assert!(d.tool_card_ids.contains(&"tc-99".to_string()));
            }
            other => panic!("expected AssistantBubble, got {:?}", other),
        }
    }

    #[test]
    fn test_ai_image_becomes_placeholder() {
        let blocks = vec![ContentBlock::Image {
            source: peri_agent::messages::ImageSource::Url {
                url: "https://example.com/img.png".into(),
            },
        }];
        let msg = BaseMessage::ai(MessageContent::blocks(blocks));
        let vm = convert_one(&msg, &[]);
        match vm {
            ViewModel::AssistantBubble(d) => {
                assert_eq!(d.text, "[Image]");
            }
            other => panic!("expected AssistantBubble, got {:?}", other),
        }
    }

    #[test]
    fn test_ai_document_becomes_placeholder() {
        let blocks = vec![ContentBlock::Document {
            source: peri_agent::messages::DocumentSource::Text {
                text: "doc content".into(),
            },
            title: Some("Spec".into()),
        }];
        let msg = BaseMessage::ai(MessageContent::blocks(blocks));
        let vm = convert_one(&msg, &[]);
        match vm {
            ViewModel::AssistantBubble(d) => {
                assert_eq!(d.text, "[Document: Spec]");
            }
            other => panic!("expected AssistantBubble, got {:?}", other),
        }
    }

    // ── Tool result message ───────────────────────────────────────────────

    #[test]
    fn test_tool_result_basic() {
        let prev_tc = vec![(
            "tc-1".to_string(),
            "Read".to_string(),
            serde_json::json!({"path": "/tmp/foo.rs"}),
        )];
        let msg = BaseMessage::tool_result("tc-1", "file contents here");
        let vm = convert_one(&msg, &prev_tc);
        match vm {
            ViewModel::ToolCard(d) => {
                assert_eq!(d.tool_id, "tc-1");
                assert_eq!(d.tool_name, "Read");
                assert_eq!(d.input_summary, "/tmp/foo.rs");
                assert_eq!(d.output_summary, "1 lines");
                assert!(!d.is_error);
                assert!(d.diff.is_none());
            }
            other => panic!("expected ToolCard, got {:?}", other),
        }
    }

    #[test]
    fn test_tool_result_error() {
        let prev_tc = vec![(
            "tc-2".to_string(),
            "Bash".to_string(),
            serde_json::json!({"command": "false"}),
        )];
        let msg = BaseMessage::tool_error("tc-2", "exit code 1");
        let vm = convert_one(&msg, &prev_tc);
        match vm {
            ViewModel::ToolCard(d) => assert!(d.is_error),
            other => panic!("expected ToolCard, got {:?}", other),
        }
    }

    #[test]
    fn test_tool_result_edit_with_diff() {
        let prev_tc = vec![(
            "tc-3".to_string(),
            "Edit".to_string(),
            serde_json::json!({
                "file_path": "foo.rs",
                "old_string": "fn old()",
                "new_string": "fn new()"
            }),
        )];
        let msg = BaseMessage::tool_result("tc-3", "updated successfully");
        let vm = convert_one(&msg, &prev_tc);
        match vm {
            ViewModel::ToolCard(d) => {
                let diff = d.diff.unwrap();
                assert_eq!(diff.path, "foo.rs");
                assert!(!diff.hunks.is_empty());
            }
            other => panic!("expected ToolCard, got {:?}", other),
        }
    }

    #[test]
    fn test_tool_result_write_with_diff() {
        let prev_tc = vec![(
            "tc-4".to_string(),
            "Write".to_string(),
            serde_json::json!({
                "file_path": "bar.rs",
                "content": "fn main() {}\n"
            }),
        )];
        let msg = BaseMessage::tool_result("tc-4", "file written");
        let vm = convert_one(&msg, &prev_tc);
        match vm {
            ViewModel::ToolCard(d) => {
                let diff = d.diff.unwrap();
                assert_eq!(diff.path, "bar.rs");
                assert!(!diff.hunks.is_empty());
            }
            other => panic!("expected ToolCard, got {:?}", other),
        }
    }

    #[test]
    fn test_tool_result_error_no_diff() {
        let prev_tc = vec![(
            "tc-5".to_string(),
            "Edit".to_string(),
            serde_json::json!({
                "file_path": "baz.rs",
                "old_string": "old",
                "new_string": "new"
            }),
        )];
        let msg = BaseMessage::tool_error("tc-5", "permission denied");
        let vm = convert_one(&msg, &prev_tc);
        match vm {
            ViewModel::ToolCard(d) => {
                assert!(d.is_error);
                assert!(d.diff.is_none()); // No diff on error
            }
            other => panic!("expected ToolCard, got {:?}", other),
        }
    }

    #[test]
    fn test_tool_result_unknown_id_fallback() {
        // No matching tool_call_id in prev_ai_tool_calls.
        let msg = BaseMessage::tool_result("unknown-id", "some result");
        let vm = convert_one(&msg, &[]);
        match vm {
            ViewModel::ToolCard(d) => {
                assert_eq!(d.tool_name, "unknown-id");
                assert_eq!(d.input_summary, "null");
            }
            other => panic!("expected ToolCard, got {:?}", other),
        }
    }

    #[test]
    fn test_tool_result_agent_becomes_subagent_group() {
        let prev_tc = vec![(
            "tc-agent".to_string(),
            "Agent".to_string(),
            serde_json::json!({
                "subagent_type": "fork",
                "prompt": "search for TODO items in the codebase"
            }),
        )];
        let msg = BaseMessage::tool_result("tc-agent", "Agent completed successfully");
        let vm = convert_one(&msg, &prev_tc);
        match vm {
            ViewModel::SubAgentGroup(d) => {
                assert_eq!(d.agent_id, "fork");
                assert_eq!(d.agent_name, "Agent");
            }
            other => panic!("expected SubAgentGroup, got {:?}", other),
        }
    }

    // ── System message ─────────────────────────────────────────────────────

    #[test]
    fn test_system_message() {
        let msg = BaseMessage::system("system prompt text");
        let vm = convert_one(&msg, &[]);
        match vm {
            ViewModel::SystemNote(d) => {
                assert_eq!(d.text, "system prompt text");
                assert_eq!(d.level, NoteLevel::Info);
            }
            other => panic!("expected SystemNote, got {:?}", other),
        }
    }

    // ── Helper function tests ─────────────────────────────────────────────

    #[test]
    fn test_truncate_text_short() {
        assert_eq!(truncate_text("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_text_exact() {
        assert_eq!(truncate_text("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_text_long() {
        assert_eq!(truncate_text("abcdefghij", 5), "abcde...");
    }

    #[test]
    fn test_truncate_text_cjk() {
        assert_eq!(truncate_text("你好世界", 2), "你好...");
    }

    #[test]
    fn test_summarize_input_read() {
        let input = serde_json::json!({"file_path": "/tmp/foo.rs", "offset": 10});
        assert_eq!(summarize_input("Read", &input), "/tmp/foo.rs");
    }

    #[test]
    fn test_summarize_input_read_fallback_path() {
        let input = serde_json::json!({"path": "/tmp/bar.rs"});
        assert_eq!(summarize_input("Read", &input), "/tmp/bar.rs");
    }

    #[test]
    fn test_summarize_input_write() {
        let input = serde_json::json!({"file_path": "src/main.rs"});
        assert_eq!(summarize_input("Write", &input), "src/main.rs");
    }

    #[test]
    fn test_summarize_input_edit() {
        let input = serde_json::json!({"file_path": "lib.rs", "old_string": "x"});
        assert_eq!(summarize_input("Edit", &input), "lib.rs");
    }

    #[test]
    fn test_summarize_input_bash() {
        let input = serde_json::json!({"command": "cargo build --release"});
        assert_eq!(summarize_input("Bash", &input), "cargo build --release");
    }

    #[test]
    fn test_summarize_input_grep() {
        let input = serde_json::json!({"pattern": "TODO"});
        assert_eq!(summarize_input("Grep", &input), "pattern: TODO");
    }

    #[test]
    fn test_summarize_input_glob() {
        let input = serde_json::json!({"pattern": "**/*.rs"});
        assert_eq!(summarize_input("Glob", &input), "pattern: **/*.rs");
    }

    #[test]
    fn test_summarize_input_folder_operations() {
        let input = serde_json::json!({"operation": "list", "folder_path": "/tmp/workdir", "pattern": "*.rs"});
        assert_eq!(
            summarize_input("folder_operations", &input),
            "list /tmp/workdir"
        );
    }

    #[test]
    fn test_summarize_input_web_search() {
        let input = serde_json::json!({"query": "rust async best practices"});
        assert_eq!(
            summarize_input("WebSearch", &input),
            "query: rust async best practices"
        );
    }

    #[test]
    fn test_summarize_input_web_fetch() {
        let input = serde_json::json!({"url": "https://docs.rs/tokio/latest/tokio/"});
        assert_eq!(
            summarize_input("WebFetch", &input),
            "url: https://docs.rs/tokio/latest/tokio/"
        );
    }

    #[test]
    fn test_summarize_input_todo_write() {
        let input = serde_json::json!({"todos": [{"content": "do stuff", "status": "pending"}]});
        assert_eq!(summarize_input("TodoWrite", &input), "");
    }

    #[test]
    fn test_summarize_input_agent_result() {
        let input = serde_json::json!({"task_id": "abc123def456ghi789"});
        assert_eq!(summarize_input("AgentResult", &input), "abc123def456...");
    }

    #[test]
    fn test_summarize_input_lsp() {
        let input = serde_json::json!({"operation": "completion", "file_path": "foo.rs"});
        assert_eq!(summarize_input("LSP", &input), "completion");
    }

    #[test]
    fn test_summarize_input_execute_extra_tool() {
        let input = serde_json::json!({"tool_name": "mcp__server__some_tool", "arguments": "{}"});
        assert_eq!(
            summarize_input("ExecuteExtraTool", &input),
            "mcp__server__some_tool"
        );
    }

    #[test]
    fn test_summarize_input_search_extra_tools() {
        let input = serde_json::json!({"query": "mcp"});
        assert_eq!(summarize_input("SearchExtraTools", &input), "mcp");
    }

    #[test]
    fn test_summarize_input_empty_object() {
        let input = serde_json::json!({});
        assert_eq!(summarize_input("Unknown", &input), "(empty input)");
    }

    #[test]
    fn test_summarize_output_empty() {
        assert_eq!(summarize_output("Bash", ""), "");
    }

    #[test]
    fn test_summarize_output_edit_long() {
        let output = "line1\nline2\nline3\nline4\nline5";
        assert_eq!(summarize_output("Edit", output), "5 lines changed");
    }

    #[test]
    fn test_summarize_output_edit_short() {
        let output = "done";
        assert_eq!(summarize_output("Edit", output), "done");
    }

    #[test]
    fn test_build_diff_block_edit() {
        let input = serde_json::json!({
            "file_path": "foo.rs",
            "old_string": "old line",
            "new_string": "new line"
        });
        let diff = build_diff_block("Edit", &input);
        assert!(diff.is_some());
        let d = diff.unwrap();
        assert_eq!(d.path, "foo.rs");
        assert_eq!(d.hunks.len(), 1);
        let hunk = &d.hunks[0];
        // Should have one del + one add
        let del_count = hunk
            .lines
            .iter()
            .filter(|l| l.kind == HunkLineKind::Del)
            .count();
        let add_count = hunk
            .lines
            .iter()
            .filter(|l| l.kind == HunkLineKind::Add)
            .count();
        assert_eq!(del_count, 1);
        assert_eq!(add_count, 1);
    }

    #[test]
    fn test_build_diff_block_write() {
        let input = serde_json::json!({
            "file_path": "bar.rs",
            "content": "fn main() {}\n"
        });
        let diff = build_diff_block("Write", &input);
        assert!(diff.is_some());
        let d = diff.unwrap();
        assert_eq!(d.path, "bar.rs");
    }

    #[test]
    fn test_build_diff_block_non_diff_tool() {
        let input = serde_json::json!({"path": "foo.rs"});
        assert!(build_diff_block("Read", &input).is_none());
    }

    #[test]
    fn test_build_diff_block_empty_old_string() {
        let input = serde_json::json!({
            "file_path": "foo.rs",
            "old_string": "",
            "new_string": "new"
        });
        assert!(build_diff_block("Edit", &input).is_none());
    }

    // ── Full pipeline test ──────────────────────────────────────────────────

    #[test]
    fn test_full_conversation_pipeline() {
        let mut mapper = ViewMapperImpl::new();

        let msgs = vec![
            BaseMessage::human("fix the bug in parser.rs"),
            BaseMessage::ai(MessageContent::blocks(vec![
                ContentBlock::text("I'll read the file first."),
                ContentBlock::ToolUse {
                    id: "tc-1".into(),
                    name: "Read".into(),
                    input: serde_json::json!({"path": "parser.rs"}),
                },
            ])),
            BaseMessage::tool_result("tc-1", "fn parse() { todo!() }"),
            BaseMessage::ai(MessageContent::blocks(vec![
                ContentBlock::text("Now I'll fix it."),
                ContentBlock::ToolUse {
                    id: "tc-2".into(),
                    name: "Edit".into(),
                    input: serde_json::json!({
                        "file_path": "parser.rs",
                        "old_string": "todo!()",
                        "new_string": "println!(\"parsed\")"
                    }),
                },
            ])),
            BaseMessage::tool_result("tc-2", "updated successfully"),
            BaseMessage::ai("Done! The parser now prints a message."),
        ];

        let vms = mapper.convert(&msgs);

        // Expected: UserBubble, AssistantBubble, ToolCard, AssistantBubble, ToolCard, AssistantBubble
        assert_eq!(vms.len(), 6);
        assert!(matches!(&vms[0], ViewModel::UserBubble(_)));
        assert!(matches!(&vms[1], ViewModel::AssistantBubble(_)));
        assert!(matches!(&vms[2], ViewModel::ToolCard(_)));
        assert!(matches!(&vms[3], ViewModel::AssistantBubble(_)));
        assert!(matches!(&vms[4], ViewModel::ToolCard(_)));
        assert!(matches!(&vms[5], ViewModel::AssistantBubble(_)));

        // Verify tool cards have correct tool names.
        if let ViewModel::ToolCard(d) = &vms[2] {
            assert_eq!(d.tool_name, "Read");
        }
        if let ViewModel::ToolCard(d) = &vms[4] {
            assert_eq!(d.tool_name, "Edit");
            assert!(d.diff.is_some());
        }

        // Verify assistant bubbles have correct tool_card_ids.
        if let ViewModel::AssistantBubble(d) = &vms[1] {
            assert_eq!(d.tool_card_ids, vec!["tc-1".to_string()]);
        }
        if let ViewModel::AssistantBubble(d) = &vms[3] {
            assert_eq!(d.tool_card_ids, vec!["tc-2".to_string()]);
        }
    }

    // ── Interleaved text + tool_use scenario ──────────────────────────────

    /// 单条 AI 消息中文本和工具调用交错出现——所有文本段必须拼接保留。
    #[test]
    fn test_ai_interleaved_text_and_tooluse_all_text_preserved() {
        let msg = BaseMessage::ai(MessageContent::blocks(vec![
            ContentBlock::text("Let me start by searching."),
            ContentBlock::ToolUse {
                id: "tc-grep".into(),
                name: "Grep".into(),
                input: serde_json::json!({"pattern": "foo"}),
            },
            ContentBlock::text("I found it, now editing."),
            ContentBlock::ToolUse {
                id: "tc-edit".into(),
                name: "Edit".into(),
                input: serde_json::json!({"file_path": "bar.rs"}),
            },
        ]));
        let vm = convert_one(&msg, &[]);
        match vm {
            ViewModel::AssistantBubble(d) => {
                assert!(
                    d.text.contains("Let me start by searching"),
                    "第一段文本应保留，实际 text='{}'",
                    d.text
                );
                assert!(
                    d.text.contains("I found it, now editing"),
                    "中间文本（两工具之间）应保留，实际 text='{}'",
                    d.text
                );
                assert_eq!(d.tool_card_ids.len(), 2);
                assert!(d.tool_card_ids.contains(&"tc-grep".to_string()));
                assert!(d.tool_card_ids.contains(&"tc-edit".to_string()));
            }
            other => panic!("期望 AssistantBubble，得到 {:?}", other),
        }
    }

    /// 完整消息序列——多段 AI 文本+工具调用+最终纯文本 AI 回复——所有文本都保留。
    #[test]
    fn test_full_pipeline_interleaved_text_all_preserved() {
        let mut mapper = ViewMapperImpl::new();

        // 模拟真实 LLM 交互：用户提问 → AI 查文件+改文件 → AI 总结
        let msgs = vec![
            BaseMessage::human("帮我在 parser.rs 中修复 foo 函数"),
            // AI 回复包含多段文字+多个工具调用交错
            BaseMessage::ai(MessageContent::blocks(vec![
                ContentBlock::text("我先搜索一下 foo 的定义。"),
                ContentBlock::ToolUse {
                    id: "tc-grep".into(),
                    name: "Grep".into(),
                    input: serde_json::json!({"pattern": "fn foo"}),
                },
                ContentBlock::text("找到了，让我读取 parser.rs。"),
                ContentBlock::ToolUse {
                    id: "tc-read".into(),
                    name: "Read".into(),
                    input: serde_json::json!({"file_path": "parser.rs"}),
                },
            ])),
            BaseMessage::tool_result("tc-grep", "parser.rs:42 fn foo()"),
            BaseMessage::tool_result("tc-read", "fn foo() { returns_none(); }"),
            // 第二段 AI 回复——根据工具结果修改
            BaseMessage::ai(MessageContent::blocks(vec![
                ContentBlock::text("我看到问题了，现在修复它。"),
                ContentBlock::ToolUse {
                    id: "tc-edit".into(),
                    name: "Edit".into(),
                    input: serde_json::json!({
                        "file_path": "parser.rs",
                        "old_string": "returns_none()",
                        "new_string": "returns_some()",
                    }),
                },
            ])),
            BaseMessage::tool_result("tc-edit", "updated successfully"),
            // 最终总结——纯文本，无工具调用（症状 2：应被渲染但实际可能丢失）
            BaseMessage::ai("修复完成！foo 函数现在返回 Some 而不是 None。"),
        ];

        let vms = mapper.convert(&msgs);

        // 期望顺序：UserBubble, AssistantBubble(2段文字), ToolCard(Grep), ToolCard(Read),
        //           AssistantBubble(文字), ToolCard(Edit), AssistantBubble(最终总结)
        // = 7 条 ViewModel
        assert_eq!(vms.len(), 7, "期望 7 条 ViewModel，实际 {} 条", vms.len());

        // 验证类型序列
        assert!(
            matches!(&vms[0], ViewModel::UserBubble(_)),
            "vms[0] 应为 UserBubble"
        );
        assert!(
            matches!(&vms[1], ViewModel::AssistantBubble(_)),
            "vms[1] 应为 AssistantBubble"
        );
        assert!(
            matches!(&vms[2], ViewModel::ToolCard(_)),
            "vms[2] 应为 ToolCard(Grep)"
        );
        assert!(
            matches!(&vms[3], ViewModel::ToolCard(_)),
            "vms[3] 应为 ToolCard(Read)"
        );
        assert!(
            matches!(&vms[4], ViewModel::AssistantBubble(_)),
            "vms[4] 应为 AssistantBubble"
        );
        assert!(
            matches!(&vms[5], ViewModel::ToolCard(_)),
            "vms[5] 应为 ToolCard(Edit)"
        );
        assert!(
            matches!(&vms[6], ViewModel::AssistantBubble(_)),
            "vms[6] 应为 AssistantBubble(总结)"
        );

        // 症状 1 验证：第一条 AssistantBubble 包含两段工具调用之间的文字
        if let ViewModel::AssistantBubble(d) = &vms[1] {
            assert!(
                d.text.contains("我先搜索一下"),
                "第一条 AB 应包含开头文字，text='{}'",
                d.text
            );
            assert!(
                d.text.contains("找到了，让我读取"),
                "第一条 AB 应包含工具间的中间文字，text='{}'",
                d.text
            );
        }

        // 第二条 AssistantBubble
        if let ViewModel::AssistantBubble(d) = &vms[4] {
            assert!(
                d.text.contains("我看到问题了"),
                "第二条 AB 应包含文字，text='{}'",
                d.text
            );
        }

        // 症状 2 验证：最终纯文本 AI 回复必须存在且有内容
        if let ViewModel::AssistantBubble(d) = &vms[6] {
            assert!(
                d.text.contains("修复完成"),
                "最终 AI 总结应保留，text='{}'",
                d.text
            );
            assert!(d.tool_card_ids.is_empty(), "最终总结不应关联工具调用");
        }
    }
}
