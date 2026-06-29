//! CurrentTurn -- accumulated streaming data for the in-progress agent turn.
//!
//! Holds the incremental text / reasoning / tool cards that arrive between two
//! `"view-commit"` events. When `"view-commit"` arrives, the state machine
//! clears this and replaces the base ViewStore with the full snapshot.
//!
//! Rendering concatenates `ViewStore.view_models + CurrentTurn.view_models()`.
//!
//! Reference: `docs/design/peri-tui-architecture.md` section 8.3.

use peri_acp_types::view_model::ViewModel;

/// Accumulated streaming data for the in-progress agent turn.
///
/// When `"view-commit"` arrives, the state machine clears this and replaces
/// the base `ViewStore` with the full snapshot. Rendering concatenates
/// `ViewStore.view_models + CurrentTurn.view_models()`.
#[derive(Debug, Clone, Default)]
pub struct CurrentTurn {
    /// Accumulated assistant text for the current turn.
    pub text: String,

    /// Accumulated reasoning / thinking text for the current turn.
    pub reasoning: String,

    /// Tool cards created by `"tool-started"` and finalised by `"tool-ended"`.
    pub tool_cards: Vec<ToolCardAccumulator>,

    /// Spinner animation frame counter (advanced by `Tick`).
    pub spinner_frame: u32,

    /// Whether the turn is actively streaming (any text / tool event arrived).
    pub active: bool,

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

    /// Advance the spinner frame counter (called on `Tick`).
    pub fn advance_spinner(&mut self) {
        self.spinner_frame = self.spinner_frame.wrapping_add(1);
    }

    /// Mark the turn as no longer active (e.g. on `"turn-interrupted"`).
    pub fn deactivate(&mut self) {
        self.active = false;
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
                || !self.tool_cards.is_empty())
        {
            self.build_view_models();
        }
        &self.cached_view_models
    }

    /// Build incremental ViewModels from accumulated streaming data into cache.
    ///
    /// Produces a sequence of ViewModels that should be appended after the
    /// base `State.view` for rendering:
    ///
    /// - **Reasoning** → `AssistantBubble` with reasoning block (if present).
    /// - **Text** → `AssistantBubble` with markdown text (if present).
    /// - **Tool cards** → `ToolCard` entries, linked via `tool_card_ids`.
    ///
    /// When neither text nor reasoning are present, tool cards are rendered
    /// as stand-alone entries without a parent bubble.
    fn build_view_models(&mut self) {
        use peri_acp_types::view_model::{AssistantBubbleData, ReasoningBlock, ToolCardData};

        let mut vms: Vec<ViewModel> = Vec::new();

        // Collect tool card IDs for the assistant bubble reference.
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

        // Tool cards follow the assistant bubble (or stand alone if no text).
        for t in &self.tool_cards {
            vms.push(ViewModel::ToolCard(ToolCardData {
                tool_id: t.tool_id.clone(),
                tool_name: t.tool_name.clone(),
                input_summary: t.input_summary.clone(),
                output_summary: t.output_summary.clone().unwrap_or_default(),
                is_error: t.is_error,
                diff: None,
            }));
        }

        self.cached_view_models = vms;
    }
}

/// In-progress tool card accumulator.
///
/// Created on `"tool-started"` and finalised on `"tool-ended"`. P5 will project
/// this into a `ViewModel::ToolCard` for rendering.
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
        // Original card is untouched.
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
}
