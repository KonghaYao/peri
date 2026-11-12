use super::{CurrentTurn, TurnSegment};
use crate::kit::display_retention::DisplayBudget;
use crate::kit::tui_render_unit::TuiSystemNote;

impl CurrentTurn {
    pub(crate) fn bound_system_notes(&mut self, budget: &mut DisplayBudget) -> bool {
        let mut pending_remove = Vec::new();
        for (index, note) in self.pending_notes.iter().enumerate().rev() {
            if !budget.retain(std::mem::size_of::<TuiSystemNote>() + note.text.capacity()) {
                pending_remove.push(index);
            }
        }
        for &index in &pending_remove {
            self.pending_notes.remove(index);
        }
        let mut segment_remove = Vec::new();
        for (index, segment) in self.segments.iter().enumerate().rev() {
            if let TurnSegment::SystemNote { text, .. } = segment
                && !budget.retain(std::mem::size_of::<TuiSystemNote>() + text.capacity())
            {
                segment_remove.push(index);
            }
        }
        for &index in &segment_remove {
            self.segments.remove(index);
        }
        let changed = !pending_remove.is_empty() || !segment_remove.is_empty();
        if changed {
            // Segment indices changed; discard the projection as well as source bodies.
            self.cached_view_models = im::Vector::new();
            self.pending_notes.shrink_to_fit();
            self.segments.shrink_to_fit();
            self.invalidate_cache();
        }
        changed
    }
}
