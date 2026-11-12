//! Expire local display records without treating them as reloadable history.

use super::{BridgeState, TuiRenderUnit};
use crate::kit::display_retention::{DisplayBudget, is_disposable};
use crate::kit::retained_bytes::{SeenAllocations, retained_unit_bytes};

pub(super) fn enforce(state: &mut BridgeState) {
    let mut budget = DisplayBudget::default();
    let mut changed = state.current_turn.bound_system_notes(&mut budget);
    // 关闭窗口时 legacy 分页区间仍占用槽位：区间内的单元属于页面条目，不能单独淘汰。
    let legacy_ranges = (!state.window.is_enabled())
        .then(crate::kit::history_paging::legacy_slot_ranges)
        .unwrap_or_default();
    let in_legacy_span = |slot: usize| {
        legacy_ranges
            .iter()
            .any(|(start, len)| slot >= *start && slot < start.saturating_add(*len))
    };
    let mut remove = Vec::new();
    for (slot, unit) in state.committed.iter().enumerate().rev() {
        if !is_disposable(unit)
            || !state.window.display_slot_available(slot)
            || in_legacy_span(slot)
            || matches!(unit, TuiRenderUnit::TuiAskUserBlock(block) if block.pending)
        {
            continue;
        }
        if !budget.retain(retained_unit_bytes(unit, &mut SeenAllocations::default())) {
            remove.push(slot);
        }
    }
    let mut committed_changed = false;
    for slot in remove {
        // Durable page records keep their source identity and normal release path.
        if state.window.remove_unmapped_display_slot(slot) {
            state.committed.remove(slot);
            committed_changed = true;
            if !state.window.is_enabled() {
                // 关闭窗口时没有驻留索引可重新派生，只能就地左移 legacy 分页区间。
                crate::kit::history_paging::shift_spans_after_slot_removal(slot);
            }
            changed = true;
        }
    }
    if changed {
        state.folded_history.invalidate();
        super::render::invalidate_structural_caches();
        if committed_changed && state.window.is_enabled() {
            crate::kit::history_paging::sync_spans_from_window(&state.window);
        }
        crate::kit::atoms::FOCUSED_ENTRY.set(None);
        let epoch = crate::kit::atoms::STRUCTURAL_EPOCH.get().saturating_add(1);
        crate::kit::atoms::STRUCTURAL_EPOCH.set(epoch);
    }
}
