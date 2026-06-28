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

    /// Placeholder: incremental ViewModels derived from streaming events.
    ///
    /// P5 will replace this with real ViewModel construction from accumulated
    /// text chunks + tool cards. For now it stays empty so the
    /// `view_models()` accessor has a real backing list.
    pub _view_models: Vec<ViewModel>,
}

impl CurrentTurn {
    /// Create a new empty `CurrentTurn`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a text chunk from `"text-chunk"`.
    pub fn append_text(&mut self, t: &str) {
        self.text.push_str(t);
        self.active = true;
    }

    /// Append a reasoning chunk from `"reasoning-chunk"`.
    pub fn append_reasoning(&mut self, t: &str) {
        self.reasoning.push_str(t);
        self.active = true;
    }

    /// Begin a new tool card from `"tool-started"`.
    pub fn start_tool(&mut self, tool: ToolCardAccumulator) {
        self.tool_cards.push(tool);
        self.active = true;
    }

    /// Finalise an existing tool card from `"tool-ended"`.
    ///
    /// No-op if `tool_id` does not match any open card.
    pub fn end_tool(&mut self, tool_id: &str, output: String, is_error: bool) {
        if let Some(t) = self.tool_cards.iter_mut().find(|t| t.tool_id == tool_id) {
            t.output_summary = Some(output);
            t.is_error = is_error;
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

    /// Render-time accessor: references to the incremental ViewModels.
    ///
    /// P5 will derive real ViewModels from `text` + `tool_cards`. For now this
    /// returns references into the placeholder `_view_models` list so callers
    /// that already concatenate `view_store + current_turn.view_models()` keep
    /// working.
    pub fn view_models(&self) -> Vec<&ViewModel> {
        self._view_models.iter().collect()
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
        let ct = CurrentTurn::default();
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
