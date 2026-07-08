//! TuiRenderUnit —— TUI 内部渲染单元类型，不共享给 ACP 层。

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

// ---------------------------------------------------------------------------
// Leaf data structures
// ---------------------------------------------------------------------------

/// User message bubble -- right-aligned plain text.
#[derive(Debug, Clone)]
pub struct TuiUserBubble {
    pub text: String,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    pub content_hash: u64,
    /// 是否为 system_reminder 注入消息（不含 CONTINUATION_HINT 的裸 <system-reminder>）。
    pub is_system_reminder: bool,
}

tui_impl_partial_eq!(TuiUserBubble: text);

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

    // ── tui_impl_partial_eq! (content_hash excluded) ────────────────────

    #[test]
    fn test_user_bubble_partial_eq_ignores_content_hash() {
        let a = TuiUserBubble {
            text: "hi".into(),
            is_system_reminder: false,
            content_hash: 1,
        };
        let b = TuiUserBubble {
            text: "hi".into(),
            is_system_reminder: false,
            content_hash: 2,
        };
        assert_eq!(a, b, "content_hash 不同但其他字段相同 → 应相等");
    }

    #[test]
    fn test_user_bubble_partial_eq_respects_text() {
        let a = TuiUserBubble {
            text: "hi".into(),
            is_system_reminder: false,
            content_hash: 0,
        };
        let b = TuiUserBubble {
            text: "ho".into(),
            is_system_reminder: false,
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
}
