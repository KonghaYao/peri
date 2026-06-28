//! Event data structures for the peri-acp protocol.
//!
//! Every custom event pushed through `peri/unstable-event` carries one of
//! these structs as its `data` payload. The event name (kebab-case string)
//! selects which struct to deserialize into.
//!
//! Reference: `docs/design/peri-acp-protocol.md` section 4 "Event Directory".

use serde::{Deserialize, Serialize};

// ===========================================================================
// §4.1 Streaming events (high-frequency)
// ===========================================================================

/// `"text-chunk"` — incremental text for the current assistant bubble.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TextChunk {
    pub text: String,
    /// Present when the event originates from a sub-agent.
    pub agent_id: Option<String>,
}

/// `"reasoning-chunk"` — incremental reasoning / thinking text.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ReasoningChunk {
    pub text: String,
    /// Present when the event originates from a sub-agent.
    pub agent_id: Option<String>,
}

/// `"tool-started"` — signals creation of an in-progress tool card.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolStarted {
    pub tool_id: String,
    pub tool_name: String,
    pub input_summary: String,
    /// Present when the event originates from a sub-agent.
    pub agent_id: Option<String>,
}

/// `"tool-ended"` — fills in the tool card result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolEnded {
    pub tool_id: String,
    pub output_summary: String,
    pub is_error: bool,
    /// Present when the event originates from a sub-agent.
    pub agent_id: Option<String>,
}

// ===========================================================================
// §4.2 Boundary events (low-frequency)
// ===========================================================================

/// `"view-commit"` — complete ViewModel list, TUI replaces its entire view.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ViewCommit {
    pub view_models: Vec<crate::view_model::ViewModel>,
}

/// `"turn-done"` — agent finished this turn, transition from Streaming to Idle.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TurnDone {}

/// `"turn-interrupted"` — agent was interrupted (user cancel or timeout).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TurnInterrupted {
    pub reason: String,
}

// ===========================================================================
// §4.3 Status events (update status bar, no message-area changes)
// ===========================================================================

/// `"token-usage"` — token consumption for the current turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
}

/// `"tool-count"` — number of tool calls in the current turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolCount {
    pub count: u64,
}

/// `"progress"` — progress percentage with a human-readable label.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Progress {
    pub percent: u32,
    pub label: String,
}

/// `"budget-warning"` — context budget threshold crossed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BudgetWarning {
    pub used: u64,
    pub limit: u64,
    pub threshold: String,
}

/// `"system-notification"` — system-level notification text with severity.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SystemNotification {
    pub text: String,
    pub level: String,
}

// ===========================================================================
// §4.4 Input assist events
// ===========================================================================

/// `"prediction"` — input prediction suggestion shown as a grey placeholder.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Prediction {
    pub text: String,
}

/// `"file-suggestions"` — @-mention file completion candidates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FileSuggestions {
    pub files: Vec<String>,
}

// ===========================================================================
// §4.5 Interaction request events (require user decision)
// ===========================================================================

/// `"hitl-pending"` — HITL tool approval request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HitlPending {
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    /// Additional tools in the same approval batch, or `null` if standalone.
    pub batch: Option<Vec<ToolApproval>>,
}

/// A single tool entry within an HITL approval batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolApproval {
    pub tool_id: String,
    pub tool_name: String,
    pub input_summary: String,
}

/// `"ask-user"` — multi-question form initiated by the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AskUser {
    pub questions: Vec<Question>,
}

/// A single question in an `AskUser` form.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Question {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<QuestionOption>,
    pub multi_select: bool,
}

/// A selectable option within a `Question`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

/// `"rewind-preview"` — preview of changes that will be undone by a rewind.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RewindPreview {
    pub files: Vec<FileChange>,
    pub messages: Vec<RewindMessage>,
}

/// A single file change in a rewind preview.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FileChange {
    pub path: String,
    pub change_type: String,
    /// Unified diff preview for the change, if available.
    pub diff: Option<String>,
}

/// A single message in a rewind preview.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RewindMessage {
    pub id: String,
    pub role: String,
    pub preview: String,
}

/// `"oauth-needed"` — MCP server authorization required.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OauthNeeded {
    pub server_name: String,
    pub auth_url: String,
}

// ===========================================================================
// §4.6 Structure events (control message-area layout)
// ===========================================================================

/// `"subagent-started"` — sub-agent created, TUI opens a collapsible group.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SubagentStarted {
    pub agent_id: String,
    pub agent_name: String,
}

/// `"subagent-stopped"` — sub-agent exited, TUI closes the group.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SubagentStopped {
    pub agent_id: String,
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- Streaming (§4.1) --------------------------------------------------

    #[test]
    fn test_text_chunk_roundtrip() {
        let tc = TextChunk {
            text: "hello".into(),
            agent_id: None,
        };
        let json = serde_json::to_string(&tc).unwrap();
        let back: TextChunk = serde_json::from_str(&json).unwrap();
        assert_eq!(tc.text, back.text);
        assert!(back.agent_id.is_none());
    }

    #[test]
    fn test_text_chunk_with_agent_id() {
        let tc = TextChunk {
            text: "sub-agent output".into(),
            agent_id: Some("sa-1".into()),
        };
        let json = serde_json::to_string(&tc).unwrap();
        let back: TextChunk = serde_json::from_str(&json).unwrap();
        assert_eq!(back.agent_id.as_deref(), Some("sa-1"));
    }

    #[test]
    fn test_reasoning_chunk_roundtrip() {
        let rc = ReasoningChunk {
            text: "thinking...".into(),
            agent_id: None,
        };
        let json = serde_json::to_string(&rc).unwrap();
        let back: ReasoningChunk = serde_json::from_str(&json).unwrap();
        assert_eq!(rc.text, back.text);
    }

    #[test]
    fn test_tool_started_roundtrip() {
        let ts = ToolStarted {
            tool_id: "tc-1".into(),
            tool_name: "Edit".into(),
            input_summary: "path: foo.rs".into(),
            agent_id: None,
        };
        let json = serde_json::to_string(&ts).unwrap();
        let back: ToolStarted = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tool_name, "Edit");
    }

    #[test]
    fn test_tool_ended_roundtrip() {
        let te = ToolEnded {
            tool_id: "tc-1".into(),
            output_summary: "updated 3 lines".into(),
            is_error: false,
            agent_id: None,
        };
        let json = serde_json::to_string(&te).unwrap();
        let back: ToolEnded = serde_json::from_str(&json).unwrap();
        assert!(!back.is_error);
    }

    // -- Boundary (§4.2) ---------------------------------------------------

    #[test]
    fn test_view_commit_roundtrip() {
        let vc = ViewCommit {
            view_models: vec![],
        };
        let json = serde_json::to_string(&vc).unwrap();
        let back: ViewCommit = serde_json::from_str(&json).unwrap();
        assert!(back.view_models.is_empty());
    }

    #[test]
    fn test_turn_done_roundtrip() {
        let td = TurnDone {};
        let json = serde_json::to_string(&td).unwrap();
        let _back: TurnDone = serde_json::from_str(&json).unwrap();
    }

    #[test]
    fn test_turn_interrupted_roundtrip() {
        let ti = TurnInterrupted {
            reason: "user cancelled".into(),
        };
        let json = serde_json::to_string(&ti).unwrap();
        let back: TurnInterrupted = serde_json::from_str(&json).unwrap();
        assert_eq!(back.reason, "user cancelled");
    }

    // -- Status (§4.3) -----------------------------------------------------

    #[test]
    fn test_token_usage_roundtrip() {
        let tu = TokenUsage {
            input: 100,
            output: 50,
        };
        let json = serde_json::to_string(&tu).unwrap();
        let back: TokenUsage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.input, 100);
        assert_eq!(back.output, 50);
    }

    #[test]
    fn test_tool_count_roundtrip() {
        let tc = ToolCount { count: 3 };
        let json = serde_json::to_string(&tc).unwrap();
        let back: ToolCount = serde_json::from_str(&json).unwrap();
        assert_eq!(back.count, 3);
    }

    #[test]
    fn test_progress_roundtrip() {
        let p = Progress {
            percent: 75,
            label: "indexing".into(),
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: Progress = serde_json::from_str(&json).unwrap();
        assert_eq!(back.percent, 75);
    }

    #[test]
    fn test_budget_warning_roundtrip() {
        let bw = BudgetWarning {
            used: 85000,
            limit: 100000,
            threshold: "0.85".into(),
        };
        let json = serde_json::to_string(&bw).unwrap();
        let back: BudgetWarning = serde_json::from_str(&json).unwrap();
        assert_eq!(back.threshold, "0.85");
    }

    #[test]
    fn test_system_notification_roundtrip() {
        let sn = SystemNotification {
            text: "model switched".into(),
            level: "info".into(),
        };
        let json = serde_json::to_string(&sn).unwrap();
        let back: SystemNotification = serde_json::from_str(&json).unwrap();
        assert_eq!(back.level, "info");
    }

    // -- Input assist (§4.4) -----------------------------------------------

    #[test]
    fn test_prediction_roundtrip() {
        let p = Prediction {
            text: "fix typo".into(),
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: Prediction = serde_json::from_str(&json).unwrap();
        assert_eq!(back.text, "fix typo");
    }

    #[test]
    fn test_file_suggestions_roundtrip() {
        let fs = FileSuggestions {
            files: vec!["src/main.rs".into(), "src/lib.rs".into()],
        };
        let json = serde_json::to_string(&fs).unwrap();
        let back: FileSuggestions = serde_json::from_str(&json).unwrap();
        assert_eq!(back.files.len(), 2);
    }

    // -- Interaction requests (§4.5) ----------------------------------------

    #[test]
    fn test_hitl_pending_standalone() {
        let hp = HitlPending {
            tool_name: "Edit".into(),
            tool_input: serde_json::json!({"path": "foo.rs"}),
            batch: None,
        };
        let json = serde_json::to_string(&hp).unwrap();
        let back: HitlPending = serde_json::from_str(&json).unwrap();
        assert!(back.batch.is_none());
    }

    #[test]
    fn test_hitl_pending_with_batch() {
        let hp = HitlPending {
            tool_name: "Write".into(),
            tool_input: serde_json::json!({"path": "bar.rs"}),
            batch: Some(vec![ToolApproval {
                tool_id: "tc-2".into(),
                tool_name: "Edit".into(),
                input_summary: "path: baz.rs".into(),
            }]),
        };
        let json = serde_json::to_string(&hp).unwrap();
        let back: HitlPending = serde_json::from_str(&json).unwrap();
        assert_eq!(back.batch.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_ask_user_roundtrip() {
        let au = AskUser {
            questions: vec![Question {
                id: "q1".into(),
                header: "Choose approach".into(),
                question: "Which pattern do you prefer?".into(),
                options: vec![QuestionOption {
                    label: "Option A".into(),
                    description: "The first approach".into(),
                }],
                multi_select: false,
            }],
        };
        let json = serde_json::to_string(&au).unwrap();
        let back: AskUser = serde_json::from_str(&json).unwrap();
        assert_eq!(back.questions.len(), 1);
        assert!(!back.questions[0].multi_select);
    }

    #[test]
    fn test_rewind_preview_roundtrip() {
        let rp = RewindPreview {
            files: vec![FileChange {
                path: "src/main.rs".into(),
                change_type: "modified".into(),
                diff: Some("- old\n+ new".into()),
            }],
            messages: vec![RewindMessage {
                id: "msg-1".into(),
                role: "assistant".into(),
                preview: "I will edit...".into(),
            }],
        };
        let json = serde_json::to_string(&rp).unwrap();
        let back: RewindPreview = serde_json::from_str(&json).unwrap();
        assert_eq!(back.files.len(), 1);
        assert_eq!(back.messages.len(), 1);
    }

    #[test]
    fn test_oauth_needed_roundtrip() {
        let on = OauthNeeded {
            server_name: "github-mcp".into(),
            auth_url: "https://github.com/login/oauth".into(),
        };
        let json = serde_json::to_string(&on).unwrap();
        let back: OauthNeeded = serde_json::from_str(&json).unwrap();
        assert_eq!(back.server_name, "github-mcp");
    }

    // -- Structure (§4.6) --------------------------------------------------

    #[test]
    fn test_subagent_started_roundtrip() {
        let ss = SubagentStarted {
            agent_id: "sa-1".into(),
            agent_name: "file-searcher".into(),
        };
        let json = serde_json::to_string(&ss).unwrap();
        let back: SubagentStarted = serde_json::from_str(&json).unwrap();
        assert_eq!(back.agent_name, "file-searcher");
    }

    #[test]
    fn test_subagent_stopped_roundtrip() {
        let ss = SubagentStopped {
            agent_id: "sa-1".into(),
        };
        let json = serde_json::to_string(&ss).unwrap();
        let back: SubagentStopped = serde_json::from_str(&json).unwrap();
        assert_eq!(back.agent_id, "sa-1");
    }

    // -- Snake_case serialization verification ------------------------------

    #[test]
    fn test_text_chunk_fields_are_snake_case() {
        let tc = TextChunk {
            text: "x".into(),
            agent_id: Some("a".into()),
        };
        let val = serde_json::to_value(&tc).unwrap();
        assert!(val.get("agent_id").is_some());
        assert!(val.get("agentId").is_none());
    }

    #[test]
    fn test_tool_started_fields_are_snake_case() {
        let ts = ToolStarted {
            tool_id: "1".into(),
            tool_name: "Edit".into(),
            input_summary: "x".into(),
            agent_id: None,
        };
        let val = serde_json::to_value(&ts).unwrap();
        assert!(val.get("tool_id").is_some());
        assert!(val.get("tool_name").is_some());
        assert!(val.get("input_summary").is_some());
        assert!(val.get("toolId").is_none());
    }

    #[test]
    fn test_question_fields_are_snake_case() {
        let q = Question {
            id: "q1".into(),
            header: "h".into(),
            question: "q".into(),
            options: vec![],
            multi_select: true,
        };
        let val = serde_json::to_value(&q).unwrap();
        assert!(val.get("multi_select").is_some());
        assert!(val.get("multiSelect").is_none());
    }
}
