//! ACP 流式数据类型——`CurrentTurn` + `ToolCardAccumulator` + `AcpEventData`。
//!
//! 这三个类型在 kit 路径和 legacy state_machine 路径中都被使用。S11 起类型
//! 定义集中在本模块，`state_machine::current_turn` / `state_machine::event`
//! 通过 re-export 保持兼容。
//!
//! ## 设计
//!
//! - **纯数据 + 方法**：所有字段为 String/Vec/bool/u32/serde_json::Value，
//!   天然 Send+Sync+'static
//! - **依赖**：仅 `peri_acp_types::view_model::ViewModel` 和
//!   `peri_acp_types::event_data::*`（workspace crate，非 legacy）
//! - **零运行时依赖**：无 terminal / network / IO，可独立测试

use peri_acp_types::event_data::*;
use peri_acp_types::view_model::ViewModel;

// ---------------------------------------------------------------------------
// CurrentTurn + ToolCardAccumulator
// ---------------------------------------------------------------------------

/// Accumulated streaming data for the in-progress agent turn.
///
/// When `"view-commit"` arrives, the consumer clears this and replaces
/// the base view with the full snapshot. Rendering concatenates
/// `committed + CurrentTurn.view_models()`.
#[derive(Debug, Clone, Default)]
pub struct CurrentTurn {
    /// Accumulated assistant text for the current turn.
    pub text: String,

    /// Accumulated reasoning / thinking text for the current turn.
    pub reasoning: String,

    /// Tool cards created by `"tool-started"` and finalised by `"tool-ended"`.
    pub tool_cards: Vec<ToolCardAccumulator>,

    /// Whether a ViewCommit already replaced the canonical view for this turn.
    pub committed: bool,

    /// Spinner animation frame counter (advanced by `Tick`).
    pub spinner_frame: u32,

    /// Whether the turn is actively streaming (any text / tool event arrived).
    pub active: bool,

    /// Streaming sub-agent cards keyed by agent_id / instance_id.
    pub subagents: Vec<SubAgentAccumulator>,

    /// Cached ViewModels built from streaming data (populated by `build_view_models`).
    ///
    /// Cleared whenever new streaming data arrives (text/reasoning/tool events),
    /// and rebuilt on the next call to `view_models()`.
    cached_view_models: Vec<ViewModel>,
}

impl CurrentTurn {
    /// Create a new empty `CurrentTurn`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Invalidate the cached ViewModels (call after any streaming data mutation).
    fn invalidate_cache(&mut self) {
        self.cached_view_models.clear();
    }

    /// Append a text chunk from `"text-chunk"`.
    pub fn append_text(&mut self, t: &str) {
        self.text.push_str(t);
        self.active = true;
        self.invalidate_cache();
    }

    /// Append a reasoning chunk from `"reasoning-chunk"`.
    pub fn append_reasoning(&mut self, t: &str) {
        self.reasoning.push_str(t);
        self.active = true;
        self.invalidate_cache();
    }

    /// Begin a new tool card from `"tool-started"`.
    pub fn start_tool(&mut self, tool: ToolCardAccumulator) {
        self.tool_cards.push(tool);
        self.active = true;
        self.invalidate_cache();
    }

    /// Finalise an existing tool card from `"tool-ended"`.
    ///
    /// No-op if `tool_id` does not match any open card.
    pub fn end_tool(&mut self, tool_id: &str, output: String, is_error: bool) {
        if let Some(t) = self.tool_cards.iter_mut().find(|t| t.tool_id == tool_id) {
            t.output_summary = Some(output);
            t.is_error = is_error;
            self.invalidate_cache();
        }
    }

    /// Begin a new sub-agent group from `"subagent-started"`.
    pub fn start_subagent(&mut self, agent_id: String, agent_name: String) {
        if self.subagents.iter().any(|s| s.agent_id == agent_id) {
            return;
        }
        self.subagents
            .push(SubAgentAccumulator::new(agent_id, agent_name));
        self.active = true;
        self.invalidate_cache();
    }

    /// Mark a sub-agent group as done from `"subagent-stopped"`.
    pub fn stop_subagent(&mut self, agent_id: &str) {
        if let Some(s) = self.subagents.iter_mut().find(|s| s.agent_id == agent_id) {
            s.is_running = false;
            self.invalidate_cache();
        }
    }

    /// Route text chunks into a sub-agent child message.
    pub fn append_subagent_text(&mut self, agent_id: &str, text: &str) -> bool {
        if let Some(s) = self.subagents.iter_mut().find(|s| s.agent_id == agent_id) {
            s.append_text(text);
            self.active = true;
            self.invalidate_cache();
            true
        } else {
            false
        }
    }

    /// Route reasoning chunks into a sub-agent child message.
    pub fn append_subagent_reasoning(&mut self, agent_id: &str, text: &str) -> bool {
        if let Some(s) = self.subagents.iter_mut().find(|s| s.agent_id == agent_id) {
            s.append_reasoning(text);
            self.active = true;
            self.invalidate_cache();
            true
        } else {
            false
        }
    }

    /// Route tool start into a sub-agent child message.
    pub fn start_subagent_tool(&mut self, agent_id: &str, tool: ToolCardAccumulator) -> bool {
        if let Some(s) = self.subagents.iter_mut().find(|s| s.agent_id == agent_id) {
            s.start_tool(tool);
            self.active = true;
            self.invalidate_cache();
            true
        } else {
            false
        }
    }

    /// Route tool end into a sub-agent child message.
    pub fn end_subagent_tool(
        &mut self,
        agent_id: &str,
        tool_id: &str,
        output: String,
        is_error: bool,
    ) -> bool {
        if let Some(s) = self.subagents.iter_mut().find(|s| s.agent_id == agent_id) {
            s.end_tool(tool_id, output, is_error);
            self.active = true;
            self.invalidate_cache();
            true
        } else {
            false
        }
    }

    /// Advance the spinner frame counter (called on `Tick`).
    pub fn advance_spinner(&mut self) {
        self.spinner_frame = self.spinner_frame.wrapping_add(1);
    }

    /// Mark the turn as no longer active (e.g. on `"turn-interrupted"`).
    pub fn deactivate(&mut self) {
        self.active = false;
        self.invalidate_cache();
    }

    /// Mark current turn as committed by a canonical ViewCommit snapshot.
    pub fn mark_committed(&mut self) {
        self.text.clear();
        self.reasoning.clear();
        self.tool_cards.clear();
        self.subagents.clear();
        self.cached_view_models.clear();
        self.active = false;
        self.committed = true;
    }

    /// Clear current turn without marking a canonical commit boundary.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Whether this turn has no pending incremental ViewModels.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
            && self.reasoning.is_empty()
            && self.tool_cards.is_empty()
            && self.subagents.is_empty()
            && self.cached_view_models.is_empty()
    }

    /// Accessor: returns cached ViewModels, building them on first call.
    ///
    /// The cache is invalidated whenever streaming data changes (text/reasoning/
    /// tool events), so this always reflects the current turn state.
    pub fn view_models(&mut self) -> &[ViewModel] {
        if self.cached_view_models.is_empty()
            && (self.active
                || !self.text.is_empty()
                || !self.reasoning.is_empty()
                || !self.tool_cards.is_empty()
                || !self.subagents.is_empty())
        {
            self.build_view_models();
        }
        &self.cached_view_models
    }

    /// Build incremental ViewModels from accumulated streaming data into cache.
    fn build_view_models(&mut self) {
        use peri_acp_types::view_model::{AssistantBubbleData, ReasoningBlock, ToolCardData};

        let mut vms: Vec<ViewModel> = Vec::new();

        let tool_ids: Vec<String> = self.tool_cards.iter().map(|t| t.tool_id.clone()).collect();

        let has_content = !self.text.is_empty() || !self.reasoning.is_empty();

        if has_content {
            let reasoning = if self.reasoning.is_empty() {
                None
            } else {
                Some(ReasoningBlock {
                    text: self.reasoning.clone(),
                    collapsed: false,
                })
            };

            vms.push(ViewModel::AssistantBubble(AssistantBubbleData {
                text: self.text.clone(),
                reasoning,
                tool_card_ids: tool_ids.clone(),
            }));
        }

        for t in &self.tool_cards {
            vms.push(ViewModel::ToolCard(ToolCardData {
                tool_id: t.tool_id.clone(),
                tool_name: t.tool_name.clone(),
                input_summary: t.input_summary.clone(),
                output_summary: t.output_summary.clone().unwrap_or_default(),
                is_error: t.is_error,
                is_running: t.output_summary.is_none(),
                diff: None,
            }));
        }

        for s in &self.subagents {
            vms.push(s.view_model());
        }

        self.cached_view_models = vms;
    }
}

/// In-progress tool card accumulator.
#[derive(Debug, Clone)]
pub struct ToolCardAccumulator {
    /// Tool call identifier (matches `tool_id` from the protocol).
    pub tool_id: String,
    /// Human-readable tool name (e.g. `"Edit"`, `"Bash"`).
    pub tool_name: String,
    /// Short summary of the tool's input arguments.
    pub input_summary: String,
    /// Short summary of the tool's output (filled by `"tool-ended"`).
    pub output_summary: Option<String>,
    /// Whether the tool returned an error.
    pub is_error: bool,
}

impl ToolCardAccumulator {
    /// Create a new in-progress tool card from a `"tool-started"` payload.
    pub fn new(tool_id: String, tool_name: String, input_summary: String) -> Self {
        Self {
            tool_id,
            tool_name,
            input_summary,
            output_summary: None,
            is_error: false,
        }
    }
}

/// In-progress sub-agent accumulator.
#[derive(Debug, Clone)]
pub struct SubAgentAccumulator {
    pub agent_id: String,
    pub agent_name: String,
    pub is_running: bool,
    pub child_turn: CurrentTurn,
}

impl SubAgentAccumulator {
    pub fn new(agent_id: String, agent_name: String) -> Self {
        Self {
            agent_id,
            agent_name,
            is_running: true,
            child_turn: CurrentTurn::new(),
        }
    }

    fn append_text(&mut self, text: &str) {
        self.child_turn.append_text(text);
    }

    fn append_reasoning(&mut self, text: &str) {
        self.child_turn.append_reasoning(text);
    }

    fn start_tool(&mut self, tool: ToolCardAccumulator) {
        self.child_turn.start_tool(tool);
    }

    fn end_tool(&mut self, tool_id: &str, output: String, is_error: bool) {
        self.child_turn.end_tool(tool_id, output, is_error);
    }

    fn view_model(&self) -> ViewModel {
        let mut child_turn = self.child_turn.clone();
        ViewModel::SubAgentGroup(peri_acp_types::view_model::SubAgentGroupData {
            agent_id: self.agent_id.clone(),
            agent_name: self.agent_name.clone(),
            view_models: child_turn.view_models().to_vec(),
            collapsed: false,
            is_running: self.is_running,
        })
    }
}

// ---------------------------------------------------------------------------
// AcpEventData -- decoded ACP custom event
// ---------------------------------------------------------------------------

/// Decoded ACP custom event.
///
/// One variant per event name defined in the ACP protocol section 4
/// ("Event Directory", see `docs/design/peri-acp-protocol.md`).
///
/// The [`decode`](AcpEventData::decode) method maps a raw `{event, data}`
/// payload to the corresponding typed variant. Unknown event names are
/// captured as [`AcpEventData::Unknown`] for forward compatibility.
#[derive(Debug, Clone)]
pub enum AcpEventData {
    // -- §4.1 Streaming (high-frequency) ------------------------------------
    /// `"text-chunk"` -- incremental text for the current assistant bubble.
    TextChunk(TextChunk),

    /// `"reasoning-chunk"` -- incremental reasoning / thinking text.
    ReasoningChunk(ReasoningChunk),

    /// `"tool-started"` -- creates an in-progress tool card.
    ToolStarted(ToolStarted),

    /// `"tool-ended"` -- fills in the tool card result.
    ToolEnded(ToolEnded),

    // -- §4.2 Boundary (low-frequency) -------------------------------------
    /// `"view-commit"` -- complete ViewModel list, TUI replaces entire view.
    ViewCommit(ViewCommit),

    /// `"turn-done"` -- agent finished this turn (Streaming -> Idle).
    TurnDone,

    /// `"turn-interrupted"` -- agent was interrupted (user cancel / timeout).
    TurnInterrupted(TurnInterrupted),

    // -- §4.3 Status (status bar updates) ----------------------------------
    /// `"token-usage"` -- token consumption for the current turn.
    TokenUsage(TokenUsage),

    /// `"tool-count"` -- number of tool calls in the current turn.
    ToolCount(ToolCount),

    /// `"progress"` -- progress percentage with label.
    Progress(Progress),

    /// `"budget-warning"` -- context budget threshold crossed.
    BudgetWarning(BudgetWarning),

    /// `"system-notification"` -- system-level notification text with severity.
    SystemNotification(SystemNotification),

    // -- §4.4 Input assist -------------------------------------------------
    /// `"prediction"` -- input prediction suggestion (grey placeholder).
    Prediction(Prediction),

    /// `"file-suggestions"` -- @-mention file completion candidates.
    FileSuggestions(FileSuggestions),

    // -- §4.5 Interaction requests (require user decision) ------------------
    /// `"hitl-pending"` -- HITL tool approval request.
    HitlPending(HitlPending),

    /// `"ask-user"` -- multi-question form initiated by the agent.
    AskUser(AskUser),

    /// `"rewind-preview"` -- preview of changes that will be undone.
    RewindPreview(RewindPreview),

    /// `"oauth-needed"` -- MCP server authorization required.
    OauthNeeded(OauthNeeded),

    // -- §4.6 Structure (control message-area layout) ------------------------
    /// `"subagent-started"` -- sub-agent created, TUI opens a collapsible group.
    SubagentStarted(SubagentStarted),

    /// `"subagent-stopped"` -- sub-agent exited, TUI closes the group.
    SubagentStopped(SubagentStopped),

    /// Fallback for unknown / future event names.
    ///
    /// Keeps the raw event name and JSON data so the state machine can log or
    /// silently ignore new events without crashing.
    Unknown {
        event: String,
        data: serde_json::Value,
    },
}

impl AcpEventData {
    /// Decode a raw `{event, data}` payload into a typed [`AcpEventData`].
    ///
    /// Dispatches by event name (kebab-case string). On deserialization
    /// failure or unknown event name, falls back to [`AcpEventData::Unknown`].
    pub fn decode(event: &str, data: serde_json::Value) -> Self {
        match event {
            // §4.1 Streaming
            "text-chunk" => decode_or_unknown(event, data, AcpEventData::TextChunk),
            "reasoning-chunk" => decode_or_unknown(event, data, AcpEventData::ReasoningChunk),
            "tool-started" => decode_or_unknown(event, data, AcpEventData::ToolStarted),
            "tool-ended" => decode_or_unknown(event, data, AcpEventData::ToolEnded),

            // §4.2 Boundary
            "view-commit" => decode_or_unknown(event, data, AcpEventData::ViewCommit),
            "turn-done" => match serde_json::from_value::<TurnDone>(data.clone()) {
                Ok(_) => AcpEventData::TurnDone,
                Err(_) => AcpEventData::unknown(event, data),
            },
            "turn-interrupted" => decode_or_unknown(event, data, AcpEventData::TurnInterrupted),

            // §4.3 Status
            "token-usage" => decode_or_unknown(event, data, AcpEventData::TokenUsage),
            "tool-count" => decode_or_unknown(event, data, AcpEventData::ToolCount),
            "progress" => decode_or_unknown(event, data, AcpEventData::Progress),
            "budget-warning" => decode_or_unknown(event, data, AcpEventData::BudgetWarning),
            "system-notification" => {
                decode_or_unknown(event, data, AcpEventData::SystemNotification)
            }

            // §4.4 Input assist
            "prediction" => decode_or_unknown(event, data, AcpEventData::Prediction),
            "file-suggestions" => decode_or_unknown(event, data, AcpEventData::FileSuggestions),

            // §4.5 Interaction requests
            "hitl-pending" => decode_or_unknown(event, data, AcpEventData::HitlPending),
            "ask-user" => decode_or_unknown(event, data, AcpEventData::AskUser),
            "rewind-preview" => decode_or_unknown(event, data, AcpEventData::RewindPreview),
            "oauth-needed" => decode_or_unknown(event, data, AcpEventData::OauthNeeded),

            // §4.6 Structure
            "subagent-started" => decode_or_unknown(event, data, AcpEventData::SubagentStarted),
            "subagent-stopped" => decode_or_unknown(event, data, AcpEventData::SubagentStopped),

            // Unknown / future event names -- forward-compatible fallback.
            _ => AcpEventData::unknown(event, data),
        }
    }

    /// Helper to construct the [`AcpEventData::Unknown`] variant.
    fn unknown(event: &str, data: serde_json::Value) -> Self {
        AcpEventData::Unknown {
            event: event.to_owned(),
            data,
        }
    }
}

/// Decode `data` into `T` and apply the variant constructor, or fall back to
/// [`AcpEventData::Unknown`] with the original `data` preserved.
fn decode_or_unknown<T, F>(event: &str, data: serde_json::Value, ctor: F) -> AcpEventData
where
    T: serde::de::DeserializeOwned,
    F: FnOnce(T) -> AcpEventData,
{
    match serde_json::from_value::<T>(data.clone()) {
        Ok(v) => ctor(v),
        Err(_) => AcpEventData::unknown(event, data),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- CurrentTurn tests ----------------------------------------------------

    #[test]
    fn test_default_empty() {
        let mut ct = CurrentTurn::default();
        assert!(ct.text.is_empty());
        assert!(ct.reasoning.is_empty());
        assert!(ct.tool_cards.is_empty());
        assert_eq!(ct.spinner_frame, 0);
        assert!(!ct.active);
        assert!(ct.view_models().is_empty());
    }

    #[test]
    fn test_new_equals_default() {
        let a = CurrentTurn::new();
        let b = CurrentTurn::default();
        assert_eq!(a.text, b.text);
        assert_eq!(a.active, b.active);
    }

    #[test]
    fn test_append_text_sets_active() {
        let mut ct = CurrentTurn::new();
        assert!(!ct.active);
        ct.append_text("hello ");
        ct.append_text("world");
        assert_eq!(ct.text, "hello world");
        assert!(ct.active);
    }

    #[test]
    fn test_append_reasoning_sets_active() {
        let mut ct = CurrentTurn::new();
        ct.append_reasoning("thinking...");
        assert_eq!(ct.reasoning, "thinking...");
        assert!(ct.active);
    }

    #[test]
    fn test_start_then_end_tool() {
        let mut ct = CurrentTurn::new();
        ct.start_tool(ToolCardAccumulator::new(
            "tc-1".into(),
            "Edit".into(),
            "path: foo.rs".into(),
        ));
        assert_eq!(ct.tool_cards.len(), 1);
        assert!(ct.active);

        ct.end_tool("tc-1", "updated 3 lines".into(), false);
        let card = &ct.tool_cards[0];
        assert_eq!(card.output_summary.as_deref(), Some("updated 3 lines"));
        assert!(!card.is_error);
    }

    #[test]
    fn test_end_tool_unknown_id_is_noop() {
        let mut ct = CurrentTurn::new();
        ct.start_tool(ToolCardAccumulator::new(
            "tc-1".into(),
            "Edit".into(),
            "x".into(),
        ));
        ct.end_tool("does-not-exist", "out".into(), true);
        assert!(ct.tool_cards[0].output_summary.is_none());
        assert!(!ct.tool_cards[0].is_error);
    }

    #[test]
    fn test_advance_spinner_wraps() {
        let mut ct = CurrentTurn::new();
        ct.spinner_frame = u32::MAX;
        ct.advance_spinner();
        assert_eq!(ct.spinner_frame, 0);
    }

    #[test]
    fn test_deactivate() {
        let mut ct = CurrentTurn::new();
        ct.append_text("x");
        assert!(ct.active);
        ct.deactivate();
        assert!(!ct.active);
    }

    // -- AcpEventData decode tests -------------------------------------------

    #[test]
    fn test_current_turn_subagent_streaming_builds_nested_group() {
        let mut ct = CurrentTurn::new();
        ct.start_subagent("agent-1".into(), "researcher".into());
        assert!(ct.append_subagent_text("agent-1", "hello"));
        assert!(ct.start_subagent_tool(
            "agent-1",
            ToolCardAccumulator::new("tc-1".into(), "Read".into(), "path: foo.rs".into()),
        ));
        assert!(ct.end_subagent_tool("agent-1", "tc-1", "10 lines".into(), false));

        let vms = ct.view_models().to_vec();
        assert_eq!(vms.len(), 1);
        match &vms[0] {
            ViewModel::SubAgentGroup(group) => {
                assert_eq!(group.agent_id, "agent-1");
                assert_eq!(group.agent_name, "researcher");
                assert_eq!(group.view_models.len(), 2);
            }
            other => panic!("expected SubAgentGroup, got {other:?}"),
        }
    }

    #[test]
    fn test_current_turn_subagent_unknown_route_returns_false() {
        let mut ct = CurrentTurn::new();
        assert!(!ct.append_subagent_text("missing", "hello"));
        assert!(ct.view_models().is_empty());
    }

    #[test]
    fn test_decode_text_chunk() {
        let data = serde_json::json!({"text": "hello", "agent_id": null});
        let decoded = AcpEventData::decode("text-chunk", data);
        match decoded {
            AcpEventData::TextChunk(tc) => {
                assert_eq!(tc.text, "hello");
                assert!(tc.agent_id.is_none());
            }
            _ => panic!("expected TextChunk, got {:?}", decoded),
        }
    }

    #[test]
    fn test_decode_reasoning_chunk() {
        let data = serde_json::json!({"text": "thinking...", "agent_id": "sa-1"});
        let decoded = AcpEventData::decode("reasoning-chunk", data);
        match decoded {
            AcpEventData::ReasoningChunk(rc) => {
                assert_eq!(rc.text, "thinking...");
                assert_eq!(rc.agent_id.as_deref(), Some("sa-1"));
            }
            _ => panic!("expected ReasoningChunk"),
        }
    }

    #[test]
    fn test_decode_tool_started() {
        let data = serde_json::json!({
            "tool_id": "tc-1",
            "tool_name": "Edit",
            "input_summary": "path: foo.rs"
        });
        let decoded = AcpEventData::decode("tool-started", data);
        match decoded {
            AcpEventData::ToolStarted(ts) => assert_eq!(ts.tool_name, "Edit"),
            _ => panic!("expected ToolStarted"),
        }
    }

    #[test]
    fn test_decode_tool_ended() {
        let data = serde_json::json!({
            "tool_id": "tc-1",
            "output_summary": "ok",
            "is_error": false
        });
        let decoded = AcpEventData::decode("tool-ended", data);
        match decoded {
            AcpEventData::ToolEnded(te) => assert!(!te.is_error),
            _ => panic!("expected ToolEnded"),
        }
    }

    #[test]
    fn test_decode_view_commit() {
        let data = serde_json::json!({"view_models": []});
        let decoded = AcpEventData::decode("view-commit", data);
        match decoded {
            AcpEventData::ViewCommit(vc) => assert!(vc.view_models.is_empty()),
            _ => panic!("expected ViewCommit"),
        }
    }

    #[test]
    fn test_decode_turn_done() {
        let decoded = AcpEventData::decode("turn-done", serde_json::json!({}));
        match decoded {
            AcpEventData::TurnDone => {}
            _ => panic!("expected TurnDone"),
        }
    }

    #[test]
    fn test_decode_turn_interrupted() {
        let data = serde_json::json!({"reason": "user cancelled"});
        let decoded = AcpEventData::decode("turn-interrupted", data);
        match decoded {
            AcpEventData::TurnInterrupted(ti) => assert_eq!(ti.reason, "user cancelled"),
            _ => panic!("expected TurnInterrupted"),
        }
    }

    #[test]
    fn test_decode_token_usage() {
        let data = serde_json::json!({"input": 100, "output": 50});
        let decoded = AcpEventData::decode("token-usage", data);
        match decoded {
            AcpEventData::TokenUsage(tu) => {
                assert_eq!(tu.input, 100);
                assert_eq!(tu.output, 50);
            }
            _ => panic!("expected TokenUsage"),
        }
    }

    #[test]
    fn test_decode_tool_count() {
        let data = serde_json::json!({"count": 3});
        let decoded = AcpEventData::decode("tool-count", data);
        match decoded {
            AcpEventData::ToolCount(tc) => assert_eq!(tc.count, 3),
            _ => panic!("expected ToolCount"),
        }
    }

    #[test]
    fn test_decode_budget_warning() {
        let data = serde_json::json!({
            "used": 85000,
            "limit": 100000,
            "threshold": "0.85"
        });
        let decoded = AcpEventData::decode("budget-warning", data);
        match decoded {
            AcpEventData::BudgetWarning(bw) => assert_eq!(bw.threshold, "0.85"),
            _ => panic!("expected BudgetWarning"),
        }
    }

    #[test]
    fn test_decode_system_notification() {
        let data = serde_json::json!({"text": "model switched", "level": "info"});
        let decoded = AcpEventData::decode("system-notification", data);
        match decoded {
            AcpEventData::SystemNotification(sn) => assert_eq!(sn.level, "info"),
            _ => panic!("expected SystemNotification"),
        }
    }

    #[test]
    fn test_decode_prediction() {
        let data = serde_json::json!({"text": "fix typo"});
        let decoded = AcpEventData::decode("prediction", data);
        match decoded {
            AcpEventData::Prediction(p) => assert_eq!(p.text, "fix typo"),
            _ => panic!("expected Prediction"),
        }
    }

    #[test]
    fn test_decode_file_suggestions() {
        let data = serde_json::json!({"files": ["src/main.rs", "src/lib.rs"]});
        let decoded = AcpEventData::decode("file-suggestions", data);
        match decoded {
            AcpEventData::FileSuggestions(fs) => assert_eq!(fs.files.len(), 2),
            _ => panic!("expected FileSuggestions"),
        }
    }

    #[test]
    fn test_decode_hitl_pending_standalone() {
        let data = serde_json::json!({
            "tool_name": "Edit",
            "tool_input": {"path": "foo.rs"},
            "batch": null
        });
        let decoded = AcpEventData::decode("hitl-pending", data);
        match decoded {
            AcpEventData::HitlPending(hp) => assert!(hp.batch.is_none()),
            _ => panic!("expected HitlPending"),
        }
    }

    #[test]
    fn test_decode_ask_user() {
        let data = serde_json::json!({
            "questions": [{
                "id": "q1",
                "header": "Choose",
                "question": "Which?",
                "options": [],
                "multi_select": false
            }]
        });
        let decoded = AcpEventData::decode("ask-user", data);
        match decoded {
            AcpEventData::AskUser(au) => assert_eq!(au.questions.len(), 1),
            _ => panic!("expected AskUser"),
        }
    }

    #[test]
    fn test_decode_rewind_preview() {
        let data = serde_json::json!({"files": [], "messages": []});
        let decoded = AcpEventData::decode("rewind-preview", data);
        match decoded {
            AcpEventData::RewindPreview(rp) => assert!(rp.files.is_empty()),
            _ => panic!("expected RewindPreview"),
        }
    }

    #[test]
    fn test_decode_oauth_needed() {
        let data = serde_json::json!({
            "server_name": "github-mcp",
            "auth_url": "https://github.com/login/oauth"
        });
        let decoded = AcpEventData::decode("oauth-needed", data);
        match decoded {
            AcpEventData::OauthNeeded(on) => assert_eq!(on.server_name, "github-mcp"),
            _ => panic!("expected OauthNeeded"),
        }
    }

    #[test]
    fn test_decode_subagent_started() {
        let data = serde_json::json!({
            "agent_id": "sa-1",
            "agent_name": "file-searcher"
        });
        let decoded = AcpEventData::decode("subagent-started", data);
        match decoded {
            AcpEventData::SubagentStarted(ss) => assert_eq!(ss.agent_name, "file-searcher"),
            _ => panic!("expected SubagentStarted"),
        }
    }

    #[test]
    fn test_decode_subagent_stopped() {
        let data = serde_json::json!({"agent_id": "sa-1"});
        let decoded = AcpEventData::decode("subagent-stopped", data);
        match decoded {
            AcpEventData::SubagentStopped(ss) => assert_eq!(ss.agent_id, "sa-1"),
            _ => panic!("expected SubagentStopped"),
        }
    }

    #[test]
    fn test_decode_unknown_event_name() {
        let data = serde_json::json!({"foo": "bar"});
        let decoded = AcpEventData::decode("future-event", data);
        match decoded {
            AcpEventData::Unknown { event, data } => {
                assert_eq!(event, "future-event");
                assert_eq!(data["foo"], "bar");
            }
            _ => panic!("expected Unknown"),
        }
    }

    #[test]
    fn test_decode_malformed_data_falls_to_unknown() {
        let data = serde_json::json!("not an object");
        let decoded = AcpEventData::decode("text-chunk", data);
        match decoded {
            AcpEventData::Unknown { event, .. } => assert_eq!(event, "text-chunk"),
            _ => panic!("expected Unknown for malformed data"),
        }
    }
}
