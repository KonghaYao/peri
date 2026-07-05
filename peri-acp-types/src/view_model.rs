//! ViewModel -- the rendering atom consumed by TUI.
//!
//! Seven variants define every visual element on screen. Conversion from
//! BaseMessage / AgentEvent lives in the ACP layer (view mapper); the TUI only
//! consumes these DTOs. Defined in `peri-acp-types` so both TUI and ACP share
//! the contract without pulling in Agent runtime types.
//!
//! Reference: `docs/design/peri-tui-architecture.md` section 4.1.

use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};

// ---------------------------------------------------------------------------
// Hash helper
// ---------------------------------------------------------------------------

/// 内容哈希——rebuild 时用于检测是否需重新渲染。
pub fn hash_str(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

// ---------------------------------------------------------------------------
// PartialEq helper macro -- skips content_hash field
// ---------------------------------------------------------------------------

/// Implement `PartialEq` for a struct, comparing only the listed fields
/// (excluding `content_hash`).
macro_rules! impl_partial_eq {
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

/// Discriminated-union ViewModel consumed by the TUI renderer.
///
/// JSON wire format uses `"type": "user-bubble"` etc. via serde tag.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ViewModel {
    UserBubble(UserBubbleData),
    AssistantBubble(AssistantBubbleData),
    ToolCard(ToolCardData),
    SystemNote(SystemNoteData),
    SubAgentGroup(SubAgentGroupData),
    CollapsedGroup(CollapsedGroupData),
    Divider(DividerData),
}

// ---------------------------------------------------------------------------
// Leaf data structures
// ---------------------------------------------------------------------------

/// User message bubble -- right-aligned plain text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserBubbleData {
    pub text: String,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    #[serde(skip)]
    pub content_hash: u64,
    /// 是否为 system_reminder 注入消息（不含 CONTINUATION_HINT 的裸 <system-reminder>）。
    #[serde(default)]
    pub is_system_reminder: bool,
}

impl_partial_eq!(UserBubbleData: text);

/// Agent reply bubble -- left-aligned markdown with optional reasoning block.
///
/// Tool invocations are **siblings** (separate `ToolCard` entries), not
/// embedded inside the bubble. `tool_card_ids` references them so the
/// renderer can visually group bubble + its tool cards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantBubbleData {
    /// Markdown source text.
    pub text: String,
    /// Optional reasoning / thinking block (Anthropic extended thinking etc.).
    pub reasoning: Option<ReasoningBlock>,
    /// Tool-card IDs that belong to this assistant turn (siblings, not children).
    pub tool_card_ids: Vec<String>,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    #[serde(skip)]
    pub content_hash: u64,
}

impl_partial_eq!(AssistantBubbleData: text, reasoning, tool_card_ids);

/// Tool invocation card -- name, summaries, optional diff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCardData {
    /// Stable identifier (matches `tool_card_ids` in AssistantBubbleData).
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
    #[serde(default)]
    pub is_running: bool,
    /// Inline diff preview (Write / Edit tools).
    pub diff: Option<DiffBlock>,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    #[serde(skip)]
    pub content_hash: u64,
}

impl_partial_eq!(ToolCardData: tool_id, tool_name, input_summary, output_summary, is_error, is_running, diff);

/// System notification -- centered banner for model switches, compact, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemNoteData {
    pub text: String,
    pub level: NoteLevel,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    #[serde(skip)]
    pub content_hash: u64,
}

impl_partial_eq!(SystemNoteData: text, level);

/// Severity of a system note.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum NoteLevel {
    Info,
    Warning,
    Error,
}

/// Sub-agent message group -- bounded by start/stop events.
///
/// Nested `view_models` render inside a collapsible container.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentGroupData {
    pub agent_id: String,
    pub agent_name: String,
    /// Nested view models produced by the sub-agent.
    pub view_models: Vec<ViewModel>,
    /// Whether the group is currently collapsed.
    pub collapsed: bool,
    /// Whether the sub-agent is still streaming.
    #[serde(default)]
    pub is_running: bool,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    #[serde(skip)]
    pub content_hash: u64,
}

impl_partial_eq!(SubAgentGroupData: agent_id, agent_name, view_models, collapsed, is_running);

/// Generic collapsible group -- e.g. batched tool calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollapsedGroupData {
    pub title: String,
    /// Number of items hidden when collapsed.
    pub count: u32,
    /// The view models inside the group (visible when expanded).
    pub view_models: Vec<ViewModel>,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    #[serde(skip)]
    pub content_hash: u64,
}

impl_partial_eq!(CollapsedGroupData: title, count, view_models);

/// Visual separator between iteration rounds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DividerData {
    /// Optional label rendered next to the line (e.g. "Round 3").
    pub label: Option<String>,
    /// 内容哈希——rebuild 时用于检测是否需重新渲染
    #[serde(skip)]
    pub content_hash: u64,
}

impl_partial_eq!(DividerData: label);

// ---------------------------------------------------------------------------
// Shared helper types
// ---------------------------------------------------------------------------

/// Collapsible reasoning / thinking block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReasoningBlock {
    pub text: String,
    /// Whether the block is currently collapsed in the UI.
    pub collapsed: bool,
}

/// Inline diff preview (for Write / Edit tool results).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiffBlock {
    /// File path the diff applies to.
    pub path: String,
    pub hunks: Vec<Hunk>,
}

/// A single diff hunk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Hunk {
    /// Header range string for the old side (e.g. "@@ -1,3 +1,4 @@").
    pub old_range: String,
    /// Header range string for the new side.
    pub new_range: String,
    pub lines: Vec<HunkLine>,
}

/// One line inside a diff hunk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HunkLine {
    pub kind: HunkLineKind,
    /// Content text (without the leading +/- or space prefix).
    pub text: String,
    /// Line number on the old side (None for pure-add lines).
    pub old_no: Option<u32>,
    /// Line number on the new side (None for pure-delete lines).
    pub new_no: Option<u32>,
}

/// Classification of a single diff line.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum HunkLineKind {
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

    #[test]
    fn test_view_model_user_bubble_roundtrip() {
        let vm = ViewModel::UserBubble(UserBubbleData {
            text: "hello".into(),
            content_hash: hash_str("hello"),
            is_system_reminder: false,
        });
        let json = serde_json::to_string(&vm).unwrap();
        let back: ViewModel = serde_json::from_str(&json).unwrap();
        assert_eq!(vm, back);
    }

    #[test]
    fn test_view_model_assistant_bubble_roundtrip() {
        let vm = ViewModel::AssistantBubble(AssistantBubbleData {
            text: "done".into(),
            reasoning: Some(ReasoningBlock {
                text: "thinking...".into(),
                collapsed: false,
            }),
            tool_card_ids: vec!["tc-1".into()],
            content_hash: hash_str("done|thinking..."),
        });
        let json = serde_json::to_string(&vm).unwrap();
        let back: ViewModel = serde_json::from_str(&json).unwrap();
        assert_eq!(vm, back);
    }

    #[test]
    fn test_view_model_tool_card_roundtrip() {
        let vm = ViewModel::ToolCard(ToolCardData {
            tool_id: "tc-1".into(),
            tool_name: "Edit".into(),
            input_summary: "path: foo.rs".into(),
            output_summary: "updated 3 lines".into(),
            is_error: false,
            is_running: false,
            diff: Some(DiffBlock {
                path: "foo.rs".into(),
                hunks: vec![Hunk {
                    old_range: "-1,3".into(),
                    new_range: "+1,4".into(),
                    lines: vec![HunkLine {
                        kind: HunkLineKind::Add,
                        text: "new line".into(),
                        old_no: None,
                        new_no: Some(4),
                    }],
                }],
            }),
            content_hash: hash_str("tc-1|Edit|path: foo.rs|updated 3 lines|false|false|foo.rs"),
        });
        let json = serde_json::to_string(&vm).unwrap();
        let back: ViewModel = serde_json::from_str(&json).unwrap();
        assert_eq!(vm, back);
    }

    #[test]
    fn test_view_model_system_note_roundtrip() {
        let vm = ViewModel::SystemNote(SystemNoteData {
            text: "model switched".into(),
            level: NoteLevel::Warning,
            content_hash: hash_str("model switched|Warning"),
        });
        let json = serde_json::to_string(&vm).unwrap();
        let back: ViewModel = serde_json::from_str(&json).unwrap();
        assert_eq!(vm, back);
    }

    #[test]
    fn test_view_model_subagent_group_roundtrip() {
        let vm = ViewModel::SubAgentGroup(SubAgentGroupData {
            agent_id: "sa-1".into(),
            agent_name: "file-searcher".into(),
            view_models: vec![ViewModel::Divider(DividerData {
                label: Some("inner".into()),
                content_hash: hash_str("inner"),
            })],
            collapsed: true,
            is_running: false,
            content_hash: hash_str("sa-1|file-searcher|1|true|false"),
        });
        let json = serde_json::to_string(&vm).unwrap();
        let back: ViewModel = serde_json::from_str(&json).unwrap();
        assert_eq!(vm, back);
    }

    #[test]
    fn test_view_model_collapsed_group_roundtrip() {
        let vm = ViewModel::CollapsedGroup(CollapsedGroupData {
            title: "3 searches".into(),
            count: 3,
            view_models: vec![],
            content_hash: hash_str("3 searches|3"),
        });
        let json = serde_json::to_string(&vm).unwrap();
        let back: ViewModel = serde_json::from_str(&json).unwrap();
        assert_eq!(vm, back);
    }

    #[test]
    fn test_view_model_divider_roundtrip() {
        let vm = ViewModel::Divider(DividerData {
            label: None,
            content_hash: hash_str(""),
        });
        let json = serde_json::to_string(&vm).unwrap();
        let back: ViewModel = serde_json::from_str(&json).unwrap();
        assert_eq!(vm, back);
    }

    #[test]
    fn test_json_tag_is_kebab_case() {
        let vm = ViewModel::UserBubble(UserBubbleData {
            text: "hi".into(),
            content_hash: hash_str("hi"),
            is_system_reminder: false,
        });
        let json = serde_json::to_value(&vm).unwrap();
        assert_eq!(json["type"], "user-bubble");
    }

    #[test]
    fn test_note_level_kebab_case() {
        assert_eq!(
            serde_json::to_value(&NoteLevel::Warning).unwrap(),
            "warning"
        );
    }

    #[test]
    fn test_hunk_line_kind_kebab_case() {
        assert_eq!(serde_json::to_value(&HunkLineKind::Add).unwrap(), "add");
    }
}
