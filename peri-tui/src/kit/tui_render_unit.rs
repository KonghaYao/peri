//! TuiRenderUnit —— TUI 内部渲染单元类型，不共享给 ACP 层。

use crate::i18n;
use std::hash::{Hash, Hasher};

// ---------------------------------------------------------------------------
// Hash 辅助函数
// ---------------------------------------------------------------------------

/// 内容哈希——rebuild 时用于检测是否需重新渲染。
pub fn tui_hash_str(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

// ---------------------------------------------------------------------------
// PartialEq 辅助宏——跳过 content_hash 字段
// ---------------------------------------------------------------------------

/// Implement `PartialEq` for a struct, comparing only the listed fields
/// (excluding `content_hash`).
macro_rules! tui_impl_partial_eq {
    ($ty:ty: $($field:ident),+ $(,)?) => {
        impl PartialEq for $ty {
            fn eq(&self, other: &Self) -> bool {
                $(self.$field == other.$field)&&+
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Top-level enum
// ---------------------------------------------------------------------------

/// Discriminated-union TuiRenderUnit consumed by the TUI renderer.
#[derive(Debug, Clone, PartialEq)]
pub enum TuiRenderUnit {
    TuiUserBubble(TuiUserBubble),
    TuiAssistantBubble(TuiAssistantBubble),
    TuiToolCard(TuiToolCard),
    TuiSystemNote(TuiSystemNote),
    TuiSubAgentGroup(TuiSubAgentGroup),
    TuiCollapsedGroup(TuiCollapsedGroup),
    TuiDivider(TuiDivider),
    TuiAskUserBlock(TuiAskUserBlock),
}

impl TuiRenderUnit {
    /// 返回该 VM 内部存储的 content_hash。
    /// 供按 VM 分片的渲染缓存作为 key 使用——hash 不变时直接 Arc::clone 复用渲染结果。
    pub fn content_hash(&self) -> u64 {
        match self {
            Self::TuiUserBubble(d) => d.content_hash,
            Self::TuiAssistantBubble(d) => d.content_hash,
            Self::TuiToolCard(d) => d.content_hash,
            Self::TuiSystemNote(d) => d.content_hash,
            Self::TuiSubAgentGroup(d) => d.content_hash,
            Self::TuiCollapsedGroup(d) => d.content_hash,
            Self::TuiDivider(d) => d.content_hash,
            Self::TuiAskUserBlock(d) => d.content_hash,
        }
    }
}

// ---------------------------------------------------------------------------
// Leaf data structures
// ---------------------------------------------------------------------------

/// System-reminder 分类——10 种从 `<system-reminder>` 标签检测到的类型。
#[derive(Debug, Clone, PartialEq)]
pub enum ReminderType {
    /// Channel（微信/Slack/飞书等）来源消息
    ChannelMessage(String),
    /// Cron 定时任务注入
    CronReminder,
    /// 后台任务完成通知
    BgTaskCompleted,
    /// Fork 模式背景 Agent 注入
    ForkMode,
    /// 上下文压缩摘要
    ContextCompacted,
    /// CONTINUATION_HINT 系统提示
    ContinuationHint,
    /// 信任边界声明
    TrustBoundary,
    /// 工具相关系统提醒
    ToolReminder,
    /// 子 Agent 结果摘要
    SubagentResult,
    /// 未匹配分类的兜底类型
    GenericReminder,
}

impl ReminderType {
    /// 中文标签，用于缩略渲染第一行。
    /// 返回 `String` 而非 `&'static str` 是为了 `ChannelMessage` 的动态 source。
    pub fn label(&self) -> String {
        match self {
            ReminderType::ChannelMessage(source) => format!("Channel ({})", source),
            ReminderType::CronReminder => i18n::tr("reminder-cron-task"),
            ReminderType::BgTaskCompleted => i18n::tr("reminder-bg-task"),
            ReminderType::ForkMode => i18n::tr("reminder-fork-mode"),
            ReminderType::ContextCompacted => i18n::tr("reminder-context-compaction"),
            ReminderType::ContinuationHint => i18n::tr("reminder-system-prompt"),
            ReminderType::TrustBoundary => i18n::tr("reminder-trust-boundary"),
            ReminderType::ToolReminder => i18n::tr("reminder-tool-reminder"),
            ReminderType::SubagentResult => i18n::tr("reminder-subagent-result"),
            ReminderType::GenericReminder => i18n::tr("reminder-system-reminder"),
        }
    }
}

/// 从 `<system-reminder>` 标签解析的信息——类型 + 摘要文本。
#[derive(Debug, Clone, PartialEq)]
pub struct ReminderInfo {
    pub reminder_type: ReminderType,
    /// 首非空行数据摘要，截断到 200 字符
    pub summary: String,
}

/// User message bubble -- right-aligned plain text.
#[derive(Debug, Clone)]
pub struct TuiUserBubble {
    pub text: String,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    pub content_hash: u64,
    /// 从文本中检测到的 system-reminder 信息。
    /// `None` 表示普通用户消息气泡。
    pub reminder: Option<ReminderInfo>,
}

impl TuiUserBubble {
    /// 构造函数——自动从文本中检测 `<system-reminder>` 标签并提取
    /// [`ReminderInfo`]。
    pub fn new(text: String) -> Self {
        let content_hash = tui_hash_str(&text);
        let reminder = detect_reminder(&text);
        TuiUserBubble {
            text,
            content_hash,
            reminder,
        }
    }
}

tui_impl_partial_eq!(TuiUserBubble: text, reminder);

/// Agent reply bubble -- left-aligned markdown with optional reasoning block.
///
/// Tool invocations are **siblings** (separate `TuiToolCard` entries), not
/// embedded inside the bubble.
#[derive(Debug, Clone)]
pub struct TuiAssistantBubble {
    /// Markdown source text.
    pub text: String,
    /// Optional reasoning / thinking block (Anthropic extended thinking etc.).
    pub reasoning: Option<TuiReasoningBlock>,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    pub content_hash: u64,
}

impl TuiAssistantBubble {
    /// 计算包含 text + reasoning.text + reasoning.collapsed 的 hash。
    /// build_view_models 和 push_view_models 都用同一公式，保证修改 collapsed 后 hash 一致。
    pub fn compute_hash(text: &str, reasoning: Option<&TuiReasoningBlock>) -> u64 {
        match reasoning {
            Some(r) => tui_hash_str(&format!("{}|{}|{}", text, r.text, r.collapsed)),
            None => tui_hash_str(text),
        }
    }

    /// 根据 text + reasoning 当前值重算 content_hash。
    /// 修改 reasoning.collapsed 后必须调用，否则按 hash 分片的渲染缓存会命中旧值。
    pub fn recompute_hash(&mut self) {
        self.content_hash = Self::compute_hash(&self.text, self.reasoning.as_ref());
    }
}

tui_impl_partial_eq!(TuiAssistantBubble: text, reasoning);

/// Tool invocation card -- name, summaries, optional diff.
#[derive(Debug, Clone)]
pub struct TuiToolCard {
    /// Stable identifier for this tool call.
    pub tool_id: String,
    /// Human-readable tool name (e.g. "Edit", "Bash").
    pub tool_name: String,
    /// One-line summary of the input / arguments.
    pub input_summary: String,
    /// One-line summary of the output / result.
    pub output_summary: String,
    /// Whether the tool invocation resulted in an error.
    pub is_error: bool,
    /// Whether the tool is still streaming/running.
    pub is_running: bool,
    /// Elapsed time in milliseconds for a running tool.
    pub running_duration_ms: Option<u64>,
    /// Inline diff preview (Write / Edit tools).
    pub diff: Option<TuiDiffBlock>,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    pub content_hash: u64,
    /// Agent 工具专用的子工具调用计数（由 build_view_models 后处理配对填充）。
    pub tool_calls_count: usize,
}

tui_impl_partial_eq!(TuiToolCard: tool_id, tool_name, input_summary, output_summary, is_error, is_running, running_duration_ms, diff);

/// System notification -- centered banner for model switches, compact, etc.
#[derive(Debug, Clone)]
pub struct TuiSystemNote {
    pub text: String,
    pub level: TuiNoteLevel,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    pub content_hash: u64,
}

tui_impl_partial_eq!(TuiSystemNote: text, level);

/// Severity of a system note.
#[derive(Debug, Clone, PartialEq)]
pub enum TuiNoteLevel {
    Info,
    Warning,
    Error,
}

/// Sub-agent message group -- bounded by start/stop events.
///
/// Nested `view_models` render inside a collapsible container.
#[derive(Debug, Clone)]
pub struct TuiSubAgentGroup {
    pub agent_id: String,
    pub agent_name: String,
    /// Nested view models produced by the sub-agent.
    pub view_models: im::Vector<TuiRenderUnit>,
    /// Whether the group is currently collapsed.
    pub collapsed: bool,
    /// Whether the sub-agent is still streaming.
    pub is_running: bool,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    pub content_hash: u64,
}

tui_impl_partial_eq!(TuiSubAgentGroup: agent_id, agent_name, view_models, collapsed, is_running);

/// Generic collapsible group -- e.g. batched tool calls.
#[derive(Debug, Clone)]
pub struct TuiCollapsedGroup {
    pub title: String,
    /// Number of items hidden when collapsed.
    pub count: u32,
    /// The view models inside the group (visible when expanded).
    pub view_models: Vec<TuiRenderUnit>,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    pub content_hash: u64,
}

tui_impl_partial_eq!(TuiCollapsedGroup: title, count, view_models);

/// Visual separator between iteration rounds.
#[derive(Debug, Clone)]
pub struct TuiDivider {
    /// Optional label rendered next to the line (e.g. "Round 3").
    pub label: Option<String>,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    pub content_hash: u64,
}

tui_impl_partial_eq!(TuiDivider: label);

/// AskUser question-answer block — rendered after user responds to AskUserQuestion tool.
#[derive(Debug, Clone)]
pub struct TuiAskUserBlock {
    /// Question-answer pairs extracted from tool input/output.
    pub items: Vec<TuiAskUserItem>,
    /// Whether any item indicates an error response.
    pub is_error: bool,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    pub content_hash: u64,
}

tui_impl_partial_eq!(TuiAskUserBlock: items, is_error);

/// A single question-answer pair in an AskUser block.
#[derive(Debug, Clone, PartialEq)]
pub struct TuiAskUserItem {
    /// Question header text.
    pub header: String,
    /// User's answer text.
    pub answer: String,
}

// ---------------------------------------------------------------------------
// Shared helper types
// ---------------------------------------------------------------------------

/// Collapsible reasoning / thinking block.
#[derive(Debug, Clone, PartialEq)]
pub struct TuiReasoningBlock {
    pub text: String,
    /// Whether the block is currently collapsed in the UI.
    pub collapsed: bool,
}

/// Inline diff preview (for Write / Edit tool results).
#[derive(Debug, Clone, PartialEq)]
pub struct TuiDiffBlock {
    /// File path the diff applies to.
    pub path: String,
    pub hunks: Vec<TuiHunk>,
    /// Binary file -- cannot display diff.
    pub is_binary: bool,
    /// Diff content exceeded safe size limit.
    pub is_too_large: bool,
    /// New file (Write, or Edit with empty old_string) -- cap at 6 lines.
    pub is_new_file: bool,
}

/// A single diff hunk.
#[derive(Debug, Clone, PartialEq)]
pub struct TuiHunk {
    /// Header range string for the old side (e.g. "@@ -1,3 +1,4 @@").
    pub old_range: String,
    /// Header range string for the new side.
    pub new_range: String,
    pub lines: Vec<TuiHunkLine>,
}

/// One line inside a diff hunk.
#[derive(Debug, Clone, PartialEq)]
pub struct TuiHunkLine {
    pub kind: TuiHunkLineKind,
    /// Content text (without the leading +/- or space prefix).
    pub text: String,
    /// Line number on the old side (None for pure-add lines).
    pub old_no: Option<u32>,
    /// Line number on the new side (None for pure-delete lines).
    pub new_no: Option<u32>,
}

/// Classification of a single diff line.
#[derive(Debug, Clone, PartialEq)]
pub enum TuiHunkLineKind {
    /// Unchanged context line.
    Context,
    /// Added line.
    Add,
    /// Deleted line.
    Del,
}

// ---------------------------------------------------------------------------
// system-reminder 检测函数
// ---------------------------------------------------------------------------

/// 提取 `<system-reminder>` 标签间的内部文本（首个匹配）。
fn extract_reminder_inner(text: &str) -> Option<String> {
    let tag = "<system-reminder>";
    let close_tag = "</system-reminder>";
    let start = text.find(tag)?;
    let content_start = start + tag.len();
    let end = text[content_start..].find(close_tag)?;
    Some(text[content_start..content_start + end].trim().to_string())
}

/// 从 reminder 内部文本提取 channel 来源短名。
fn extract_channel_source(inner: &str) -> Option<String> {
    // 匹配 plugin:name:name 格式
    if let Some(plugin_pos) = inner.find("plugin:") {
        let after = &inner[plugin_pos + "plugin:".len()..];
        if let Some(colon_pos) = after.find(':') {
            let raw = &after[..colon_pos];
            // 映射到显示名
            let display = match raw {
                "weixin" | "wechat" => i18n::tr("channel-wechat"),
                "slack" => "Slack".to_string(),
                "feishu" => i18n::tr("channel-feishu"),
                "dingtalk" => i18n::tr("channel-dingtalk"),
                "telegram" => "Telegram".to_string(),
                other => other.to_string(),
            };
            return Some(display.to_string());
        }
    }

    // channel source 关键词直搜
    let lower = inner.to_lowercase();
    for (kw, display) in &[
        ("weixin", i18n::tr("channel-wechat")),
        ("wechat", i18n::tr("channel-wechat")),
        ("slack", "Slack".to_string()),
        ("feishu", i18n::tr("channel-feishu")),
        ("dingtalk", i18n::tr("channel-dingtalk")),
        ("telegram", "Telegram".to_string()),
    ] {
        if lower.contains(kw) {
            return Some(display.to_string());
        }
    }

    None
}

/// 按优先级分类 reminder 类型。
fn classify_reminder_type(inner: &str, _full_text: &str) -> ReminderType {
    if inner.contains("CONTINUATION_HINT") {
        return ReminderType::ContinuationHint;
    }
    if let Some(source) = extract_channel_source(inner) {
        return ReminderType::ChannelMessage(source);
    }
    let lower = inner.to_lowercase();
    if lower.contains("cron") || lower.contains("scheduled") {
        ReminderType::CronReminder
    } else if lower.contains("background") || lower.contains("bgtask") || inner.contains("后台") {
        ReminderType::BgTaskCompleted
    } else if lower.contains("fork") {
        ReminderType::ForkMode
    } else if lower.contains("compact") || inner.contains("压缩") {
        ReminderType::ContextCompacted
    } else if inner.contains("Trust boundary") || inner.contains("信任边界") {
        ReminderType::TrustBoundary
    } else if lower.contains("tool") || inner.contains("工具") {
        ReminderType::ToolReminder
    } else if lower.contains("subagent") || lower.contains("sub_agent") || inner.contains("子Agent")
    {
        ReminderType::SubagentResult
    } else {
        ReminderType::GenericReminder
    }
}

/// 从 reminder 内部文本提取摘要：首非空行，截断到 200 字符。
fn extract_summary(inner: &str) -> String {
    let first_line = inner.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let trimmed = first_line.trim();
    if trimmed.chars().count() <= 200 {
        trimmed.to_string()
    } else {
        let trunc: String = trimmed.chars().take(200).collect();
        format!("{}…", trunc)
    }
}

/// 公开入口：从用户消息文本中检测 `<system-reminder>` 标签。
/// 返回 `Some(ReminderInfo)` 若存在合法标签，否则 `None`。
pub fn detect_reminder(text: &str) -> Option<ReminderInfo> {
    let inner = extract_reminder_inner(text)?;
    let reminder_type = classify_reminder_type(&inner, text);
    let summary = extract_summary(&inner);
    Some(ReminderInfo {
        reminder_type,
        summary,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── tui_hash_str ─────────────────────────────────────────────────────

    #[test]
    fn test_tui_hash_str_same_input_same_output() {
        assert_eq!(tui_hash_str("hello"), tui_hash_str("hello"));
    }

    #[test]
    fn test_tui_hash_str_different_input_different_output() {
        assert_ne!(tui_hash_str("hello"), tui_hash_str("world"));
    }

    #[test]
    fn test_tui_hash_str_empty_string() {
        // 空字符串不 panic
        let _h = tui_hash_str("");
    }

    // ── TuiRenderUnit::content_hash() dispatch ──────────────────────────

    #[test]
    fn test_content_hash_returns_inner_field_for_each_variant() {
        // 验证 content_hash() 方法正确派发到各变体的内部字段
        let user = TuiRenderUnit::TuiUserBubble(TuiUserBubble {
            text: "u".into(),
            reminder: None,
            content_hash: 11,
        });
        assert_eq!(user.content_hash(), 11);
        let assistant = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
            text: "a".into(),
            reasoning: None,
            content_hash: 22,
        });
        assert_eq!(assistant.content_hash(), 22);
        let tool = TuiRenderUnit::TuiToolCard(TuiToolCard {
            tool_id: "t1".into(),
            tool_name: "Bash".into(),
            input_summary: "ls".into(),
            output_summary: String::new(),
            is_error: false,
            is_running: false,
            running_duration_ms: None,
            diff: None,
            tool_calls_count: 0,
            content_hash: 33,
        });
        assert_eq!(tool.content_hash(), 33);
        let note = TuiRenderUnit::TuiSystemNote(TuiSystemNote {
            text: "n".into(),
            level: TuiNoteLevel::Info,
            content_hash: 44,
        });
        assert_eq!(note.content_hash(), 44);
    }

    // ── TuiAssistantBubble::compute_hash / recompute_hash ──────────────

    #[test]
    fn test_compute_hash_no_reasoning_only_hashes_text() {
        // 无 reasoning：hash 只基于 text
        let h1 = TuiAssistantBubble::compute_hash("hello", None);
        let h2 = TuiAssistantBubble::compute_hash("hello", None);
        let h3 = TuiAssistantBubble::compute_hash("world", None);
        assert_eq!(h1, h2, "相同 text 应有相同 hash");
        assert_ne!(h1, h3, "不同 text 应有不同 hash");
    }

    #[test]
    fn test_compute_hash_includes_collapsed_state() {
        // [回归测试] Bug 2 修复：reasoning.collapsed 必须纳入 hash，
        // 否则按 hash 分片的渲染缓存命中旧值、折叠/展开后 UI 不刷新。
        let reasoning_open = TuiReasoningBlock {
            text: "thinking".into(),
            collapsed: false,
        };
        let reasoning_collapsed = TuiReasoningBlock {
            text: "thinking".into(),
            collapsed: true,
        };
        let h_open = TuiAssistantBubble::compute_hash("reply", Some(&reasoning_open));
        let h_collapsed = TuiAssistantBubble::compute_hash("reply", Some(&reasoning_collapsed));
        assert_ne!(
            h_open, h_collapsed,
            "collapsed 状态变化时 content_hash 必须变化"
        );
    }

    #[test]
    fn test_compute_hash_includes_reasoning_text() {
        let r1 = TuiReasoningBlock {
            text: "thought A".into(),
            collapsed: false,
        };
        let r2 = TuiReasoningBlock {
            text: "thought B".into(),
            collapsed: false,
        };
        let h1 = TuiAssistantBubble::compute_hash("reply", Some(&r1));
        let h2 = TuiAssistantBubble::compute_hash("reply", Some(&r2));
        assert_ne!(h1, h2, "reasoning.text 变化时 content_hash 必须变化");
    }

    #[test]
    fn test_recompute_hash_after_collapse_change() {
        // [回归测试] push_view_models 修改 collapsed 后必须调用 recompute_hash，
        // 否则缓存命中旧 hash 渲染不更新。
        let mut bubble = TuiAssistantBubble {
            text: "reply".into(),
            reasoning: Some(TuiReasoningBlock {
                text: "thinking".into(),
                collapsed: false,
            }),
            content_hash: 0,
        };
        bubble.content_hash =
            TuiAssistantBubble::compute_hash(&bubble.text, bubble.reasoning.as_ref());
        let initial_hash = bubble.content_hash;
        // 修改 collapsed 状态
        bubble.reasoning.as_mut().unwrap().collapsed = true;
        // 不调用 recompute_hash → content_hash 仍是旧值（错误状态）
        assert_eq!(bubble.content_hash, initial_hash);
        // 调用 recompute_hash → content_hash 更新
        bubble.recompute_hash();
        assert_ne!(
            bubble.content_hash, initial_hash,
            "recompute_hash 后 content_hash 必须反映新 collapsed"
        );
        // 验证 recompute_hash 的结果与 compute_hash 一致
        let expected = TuiAssistantBubble::compute_hash(&bubble.text, bubble.reasoning.as_ref());
        assert_eq!(bubble.content_hash, expected);
    }

    #[test]
    fn test_recompute_hash_no_reasoning_hashes_text_only() {
        let mut bubble = TuiAssistantBubble {
            text: "plain reply".into(),
            reasoning: None,
            content_hash: 0,
        };
        bubble.recompute_hash();
        let expected = TuiAssistantBubble::compute_hash(&bubble.text, None);
        assert_eq!(bubble.content_hash, expected);
    }

    // ── tui_impl_partial_eq! (content_hash excluded) ────────────────────

    #[test]
    fn test_user_bubble_partial_eq_ignores_content_hash() {
        let a = TuiUserBubble {
            text: "hi".into(),
            reminder: None,
            content_hash: 1,
        };
        let b = TuiUserBubble {
            text: "hi".into(),
            reminder: None,
            content_hash: 2,
        };
        assert_eq!(a, b, "content_hash 不同但其他字段相同 → 应相等");
    }

    #[test]
    fn test_user_bubble_partial_eq_respects_text() {
        let a = TuiUserBubble {
            text: "hi".into(),
            reminder: None,
            content_hash: 0,
        };
        let b = TuiUserBubble {
            text: "ho".into(),
            reminder: None,
            content_hash: 0,
        };
        assert_ne!(a, b, "text 不同 → 应不等");
    }

    #[test]
    fn test_assistant_bubble_partial_eq_ignores_content_hash() {
        let a = TuiAssistantBubble {
            text: "hello".into(),
            reasoning: None,
            content_hash: 42,
        };
        let b = TuiAssistantBubble {
            text: "hello".into(),
            reasoning: None,
            content_hash: 99,
        };
        assert_eq!(a, b);
    }

    #[test]
    fn test_tool_card_partial_eq_ignores_content_hash() {
        let a = TuiToolCard {
            tool_id: "tc-1".into(),
            tool_name: "Edit".into(),
            input_summary: "path: foo".into(),
            output_summary: "done".into(),
            is_error: false,
            is_running: false,
            running_duration_ms: None,
            diff: None,
            tool_calls_count: 0,
            content_hash: 1,
        };
        let b = TuiToolCard {
            content_hash: 2,
            ..a.clone()
        };
        assert_eq!(a, b);
    }

    #[test]
    fn test_tui_render_unit_subagent_group_construction() {
        let inner = TuiRenderUnit::TuiDivider(TuiDivider {
            label: Some("inner".into()),
            content_hash: tui_hash_str("inner"),
        });
        let vm = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
            agent_id: "sa-1".into(),
            agent_name: "explorer".into(),
            view_models: im::Vector::from(vec![inner]),
            collapsed: true,
            is_running: false,
            content_hash: 0,
        });
        match &vm {
            TuiRenderUnit::TuiSubAgentGroup(data) => {
                assert_eq!(data.agent_name, "explorer");
                assert_eq!(data.view_models.len(), 1);
                assert!(data.collapsed);
            }
            _ => panic!("expected TuiSubAgentGroup"),
        }
    }

    #[test]
    fn test_tui_render_unit_divider_no_label() {
        let vm = TuiRenderUnit::TuiDivider(TuiDivider {
            label: None,
            content_hash: 0,
        });
        match &vm {
            TuiRenderUnit::TuiDivider(data) => assert!(data.label.is_none()),
            _ => panic!("expected TuiDivider"),
        }
    }

    // ── reminder 检测 ────────────────────────────────────────────────────

    mod reminder_tests {
        use super::*;

        #[test]
        fn test_detect_no_tag_returns_none() {
            assert!(detect_reminder("hello world").is_none());
        }

        #[test]
        fn test_detect_empty_tag_returns_some() {
            let info = detect_reminder("<system-reminder></system-reminder>")
                .expect("empty tag should still be detected");
            assert!(matches!(info.reminder_type, ReminderType::GenericReminder));
            assert!(info.summary.is_empty());
        }

        #[test]
        fn test_detect_continuation_hint() {
            let info = detect_reminder(
                "<system-reminder>CONTINUATION_HINT: the agent sent additional content</system-reminder>",
            )
            .expect("should detect");
            assert!(matches!(info.reminder_type, ReminderType::ContinuationHint));
            assert!(info.summary.contains("CONTINUATION_HINT"));
        }

        #[test]
        fn test_detect_channel_message() {
            i18n::init(None);
            let info = detect_reminder(
                "<system-reminder>source=\"plugin:weixin:weixin\" chat_id=\"123\"\nhello from channel</system-reminder>",
            )
            .expect("should detect");
            match info.reminder_type {
                ReminderType::ChannelMessage(ref source) => {
                    assert_eq!(source, "WeChat");
                }
                other => panic!("expected ChannelMessage, got {other:?}"),
            }
            assert!(info.summary.contains("source"));
        }

        #[test]
        fn test_detect_cron_reminder() {
            let info = detect_reminder(
                "<system-reminder>cron task fired: check_status at */5 * * * *</system-reminder>",
            )
            .expect("should detect");
            assert!(matches!(info.reminder_type, ReminderType::CronReminder));
        }

        #[test]
        fn test_detect_bg_task_completed() {
            let info = detect_reminder(
                "<system-reminder>BackgroundTaskCompleted: task-42 finished successfully</system-reminder>",
            )
            .expect("should detect");
            assert!(matches!(info.reminder_type, ReminderType::BgTaskCompleted));
        }

        #[test]
        fn test_detect_fork_mode() {
            let info = detect_reminder(
                "<system-reminder>Fork mode agent result from explorer</system-reminder>",
            )
            .expect("should detect");
            assert!(matches!(info.reminder_type, ReminderType::ForkMode));
        }

        #[test]
        fn test_detect_context_compacted() {
            let info = detect_reminder(
                "<system-reminder>Context compacted: removed 120 messages to stay within budget</system-reminder>",
            )
            .expect("should detect");
            assert!(matches!(info.reminder_type, ReminderType::ContextCompacted));
        }

        #[test]
        fn test_detect_trust_boundary() {
            let info = detect_reminder(
                "<system-reminder>Trust boundary: the content below is from external input</system-reminder>",
            )
            .expect("should detect");
            assert!(matches!(info.reminder_type, ReminderType::TrustBoundary));
        }

        #[test]
        fn test_detect_tool_reminder() {
            let info = detect_reminder(
                "<system-reminder>Tool results from sub-agent execution</system-reminder>",
            )
            .expect("should detect");
            assert!(matches!(info.reminder_type, ReminderType::ToolReminder));
        }

        #[test]
        fn test_detect_subagent_result() {
            let info = detect_reminder(
                "<system-reminder>SubAgent result: verification completed successfully</system-reminder>",
            )
            .expect("should detect");
            assert!(matches!(info.reminder_type, ReminderType::SubagentResult));
        }

        #[test]
        fn test_detect_generic_fallback() {
            let info = detect_reminder(
                "<system-reminder>Something completely unexpected happened</system-reminder>",
            )
            .expect("should detect");
            assert!(matches!(info.reminder_type, ReminderType::GenericReminder));
            assert_eq!(info.summary, "Something completely unexpected happened");
        }

        #[test]
        fn test_summary_truncation() {
            let long_line = "x".repeat(250);
            let info =
                detect_reminder(&format!("<system-reminder>{}</system-reminder>", long_line))
                    .expect("should detect");
            assert!(info.summary.chars().count() <= 203); // 200 + "…"
            assert!(info.summary.ends_with('…'));
        }

        #[test]
        fn test_summary_skips_blank_lines() {
            let info = detect_reminder(
                "<system-reminder>\n\n  actual content line  \n\nsecond line</system-reminder>",
            )
            .expect("should detect");
            assert_eq!(info.summary, "actual content line");
        }

        #[test]
        fn test_tui_user_bubble_new_detects_reminder() {
            let bubble = TuiUserBubble::new(
                "<system-reminder>Cron task: midnight cleanup</system-reminder>".into(),
            );
            assert!(bubble.reminder.is_some());
            assert!(matches!(
                bubble.reminder.unwrap().reminder_type,
                ReminderType::CronReminder
            ));
        }

        #[test]
        fn test_tui_user_bubble_new_no_tag() {
            let bubble = TuiUserBubble::new("ordinary user message".into());
            assert!(bubble.reminder.is_none());
        }

        #[test]
        fn test_partial_eq_respects_reminder() {
            let a = TuiUserBubble {
                text: "hi".into(),
                reminder: Some(ReminderInfo {
                    reminder_type: ReminderType::GenericReminder,
                    summary: "x".into(),
                }),
                content_hash: 0,
            };
            let b = TuiUserBubble {
                text: "hi".into(),
                reminder: None,
                content_hash: 0,
            };
            assert_ne!(a, b, "reminder 不同 → 应不等");
        }

        #[test]
        fn test_label_channel_message() {
            let t = ReminderType::ChannelMessage("微信".into());
            assert_eq!(t.label(), "Channel (微信)");
        }

        #[test]
        fn test_label_static_types() {
            i18n::init(None);
            assert_eq!(ReminderType::CronReminder.label(), "Cron Task");
            assert_eq!(ReminderType::BgTaskCompleted.label(), "Background Task");
            assert_eq!(ReminderType::ForkMode.label(), "Fork Mode");
            assert_eq!(ReminderType::ContextCompacted.label(), "Context Compaction");
            assert_eq!(ReminderType::ContinuationHint.label(), "System Prompt");
            assert_eq!(ReminderType::TrustBoundary.label(), "Trust Boundary");
            assert_eq!(ReminderType::ToolReminder.label(), "Tool Reminder");
            assert_eq!(ReminderType::SubagentResult.label(), "SubAgent Result");
            assert_eq!(ReminderType::GenericReminder.label(), "System Reminder");
        }
    }
}
