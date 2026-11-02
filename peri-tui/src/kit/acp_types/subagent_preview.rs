//! Bounded, disposable child display data. Never changes model/tool input or history.

use super::{CurrentTurn, SubAgentAccumulator, ToolCardAccumulator};
use crate::kit::retained_bytes::{SeenAllocations, retained_unit_bytes};
use crate::kit::stream_data::TuiToolStarted;

pub(crate) const FIELD_BYTES: usize = 16 * 1024;
pub(crate) const DETAIL_BYTES: usize = 256 * 1024;
pub(crate) const STORE_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const TERMINAL_ENTRIES: usize = 256;
pub(crate) const TERMINAL_BYTES: usize = 512 * 1024;

pub(crate) fn text(value: &str, limit: usize) -> (&str, bool) {
    let mut start = value.len().saturating_sub(limit);
    while !value.is_char_boundary(start) {
        start += 1;
    }
    (&value[start..], start != 0)
}

// Charge containers as well as strings before making the display copy.
pub(crate) fn json_bytes(value: &serde_json::Value) -> usize {
    crate::kit::retained_bytes::json_value_bytes(value).reserved
}

pub(crate) fn tool(
    source: &TuiToolStarted,
    previous_todos: Option<&crate::kit::tool_semantics::TodoSnapshot>,
) -> (ToolCardAccumulator, bool) {
    let (summary, mut omitted) = text(&source.input_summary, FIELD_BYTES);
    let raw_input = if json_bytes(&source.raw_input) > FIELD_BYTES {
        omitted = true;
        serde_json::Value::Null
    } else {
        source.raw_input.clone()
    };
    let (name, name_omitted) = text(&source.tool_name, 256);
    omitted |= name_omitted;
    let previous_fits = previous_todos.is_none_or(|todos| {
        std::mem::size_of_val(todos.items())
            + todos
                .items()
                .iter()
                .map(|item| {
                    item.content.capacity() + item.active_form.as_ref().map_or(0, String::capacity)
                })
                .sum::<usize>()
            <= FIELD_BYTES
    });
    omitted |= !previous_fits;
    let mut tool = ToolCardAccumulator::with_input(
        source.tool_id.clone(),
        name.to_owned(),
        summary.to_owned(),
        raw_input,
        previous_todos.filter(|_| previous_fits),
    );
    if omitted {
        tool.presentation = crate::kit::tui_render_unit::TuiToolPresentation::Generic;
    }
    (tool, omitted)
}

impl SubAgentAccumulator {
    pub(crate) fn mark_preview_omitted(&mut self) {
        self.preview_omitted = true;
        // Stopped previews have no raw duplicate. Preserve their sole frozen body.
        let cached = self.cached_view_model.take().map(|mut unit| {
            if let crate::kit::tui_render_unit::TuiRenderUnit::TuiSubAgentGroup(group) = &mut unit {
                group.preview_omitted = true;
                group.recompute_hash();
            }
            unit
        });
        self.cached_view_model.replace(cached);
    }

    pub(crate) fn preview_reserved(&self) -> usize {
        let mut bytes = self.child_turn.preview_memory_bytes();
        if let Some(vm) = self.cached_view_model.borrow().as_ref() {
            bytes = bytes.saturating_add(retained_unit_bytes(vm, &mut SeenAllocations::default()));
        }
        bytes
    }

    pub(crate) fn release_preview(&mut self) {
        self.child_turn = CurrentTurn::new();
        self.cached_view_model.replace(None);
        self.preview_omitted = true;
    }

    pub(super) fn admit_preview_text<'a>(&mut self, value: &'a str) -> &'a str {
        let (retained, omitted) = text(value, FIELD_BYTES);
        self.preview_omitted |= omitted;
        if self
            .preview_reserved()
            .saturating_add(retained.len().saturating_mul(4))
            > DETAIL_BYTES
        {
            self.release_preview();
        }
        retained
    }

    pub(super) fn bound_preview(&mut self) {
        if self.preview_reserved() > DETAIL_BYTES {
            self.release_preview();
        }
    }

    pub(crate) fn seal_preview(&mut self) {
        self.bound_preview();
        let frozen = self.view_model();
        // Keep only the immutable bounded view after stopping, not its raw duplicate.
        self.child_turn = CurrentTurn::new();
        self.cached_view_model.replace(Some(frozen));
        self.bound_preview();
    }
}
