//! Bound child previews in both an unfinished parent and committed history.
//! Eviction is explicit display expiry, never a durable-history acknowledgement.

use super::{BridgeState, TuiRenderUnit};
use crate::kit::acp_types::subagent_preview::{STORE_BYTES, TERMINAL_BYTES, TERMINAL_ENTRIES};
use crate::kit::retained_bytes::{SeenAllocations, retained_unit_bytes};

pub(super) fn enforce(state: &mut BridgeState) {
    let (mut changed, mut retired) = state.current_turn.bound_subagent_previews();
    let mut bytes: usize = state
        .committed
        .iter()
        .filter(|unit| matches!(unit, TuiRenderUnit::TuiSubAgentGroup(_)))
        .map(|unit| retained_unit_bytes(unit, &mut SeenAllocations::default()))
        .sum();
    for index in 0..state.committed.len() {
        if bytes <= STORE_BYTES {
            break;
        }
        let TuiRenderUnit::TuiSubAgentGroup(group) = &state.committed[index] else {
            continue;
        };
        if group.view_models.is_empty() {
            continue;
        }
        let before = retained_unit_bytes(&state.committed[index], &mut SeenAllocations::default());
        // Copy only bounded header metadata; do not COW-clone every nested body.
        let mut next = group.clone();
        next.view_models = im::Vector::new();
        next.preview_omitted = true;
        next.recompute_hash();
        state
            .committed
            .set(index, TuiRenderUnit::TuiSubAgentGroup(next));
        let after = retained_unit_bytes(&state.committed[index], &mut SeenAllocations::default());
        let freed = before.saturating_sub(after);
        bytes = bytes.saturating_sub(freed);
        changed = true;
    }
    // A committed group is a display snapshot, including formerly-running BG
    // groups. Its live task remains independently represented by the task layer.
    let (mut count, mut retained) = (0usize, 0usize);
    let mut remove = Vec::new();
    for (index, unit) in state.committed.iter().enumerate().rev() {
        if !matches!(unit, TuiRenderUnit::TuiSubAgentGroup(_)) {
            continue;
        }
        let size = retained_unit_bytes(unit, &mut SeenAllocations::default());
        if count >= TERMINAL_ENTRIES || retained.saturating_add(size) > TERMINAL_BYTES {
            remove.push(index);
        } else {
            count += 1;
            retained += size;
        }
    }
    for index in remove {
        if !state.window.remove_unmapped_display_slot(index) {
            tracing::warn!(
                index,
                "child preview overlaps a canonical display range; eviction refused"
            );
            continue;
        }
        if let TuiRenderUnit::TuiSubAgentGroup(group) = state.committed.remove(index) {
            retired.push(group.instance_id);
        }
        changed = true;
    }
    if !retired.is_empty() {
        let atom = crate::kit::atoms::FOLD_OVERRIDES.state();
        let mut overrides = atom.write();
        for id in retired {
            overrides.remove(&crate::kit::tui_render_unit::FoldKey::SubAgent(id));
        }
        overrides.shrink_to_fit();
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
