//! Expire local display records without treating them as reloadable history.

use super::{BridgeState, TuiRenderUnit};
use crate::kit::display_retention::{DisplayBudget, is_disposable};
use crate::kit::retained_bytes::{SeenAllocations, retained_unit_bytes};

pub(super) fn enforce(state: &mut BridgeState) {
    let mut budget = DisplayBudget::default();
    let mut changed = state.current_turn.bound_system_notes(&mut budget);
    let mut remove = Vec::new();
    for (slot, unit) in state.committed.iter().enumerate().rev() {
        if !is_disposable(unit)
            || !state.window.display_slot_available(slot)
            || matches!(unit, TuiRenderUnit::TuiAskUserBlock(block) if block.pending)
        {
            continue;
        }
        if !budget.retain(retained_unit_bytes(unit, &mut SeenAllocations::default())) {
            remove.push(slot);
        }
    }
    for slot in remove {
        // Durable page records keep their source identity and normal release path.
        if state.window.remove_unmapped_display_slot(slot) {
            state.committed.remove(slot);
            changed = true;
        }
    }
    if changed {
        state.folded_history.invalidate();
        super::render::invalidate_structural_caches();
        crate::kit::history_paging::sync_spans_from_window(&state.window);
        crate::kit::atoms::FOCUSED_ENTRY.set(None);
        let epoch = crate::kit::atoms::STRUCTURAL_EPOCH.get().saturating_add(1);
        crate::kit::atoms::STRUCTURAL_EPOCH.set(epoch);
    }
}
