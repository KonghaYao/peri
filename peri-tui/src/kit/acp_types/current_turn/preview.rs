//! Child preview retention is independent of parent TurnDone and viewport pins.

use super::{CurrentTurn, PendingSubagentGroup, TurnSegment};
use crate::kit::acp_types::subagent_preview::{STORE_BYTES, TERMINAL_BYTES, TERMINAL_ENTRIES};
use peri_acp_types::session::ExecutionBinding;

impl CurrentTurn {
    /// A newer execution may start before the old stop reaches this view.
    /// Retire only its display occurrence; task completion remains authoritative elsewhere.
    pub(crate) fn prepare_subagent_execution(
        &mut self,
        child_id: &str,
        execution: &ExecutionBinding,
    ) -> Vec<String> {
        let keep: Vec<_> = self
            .subagents
            .iter()
            .map(|child| {
                !(child.agent_id == child_id
                    && child.is_running
                    && child.execution.as_ref() != Some(execution))
            })
            .collect();
        // An absent terminal is unknown, not success. Retire this preview without
        // rendering a false Completed status or touching its real task owner.
        self.retain_subagent_previews(&keep)
    }

    pub(crate) fn bind_subagent_execution(&mut self, child_id: &str, execution: &ExecutionBinding) {
        if let Some(child) = self
            .subagents
            .iter_mut()
            .rev()
            .find(|s| s.agent_id == child_id && s.is_running)
        {
            child.execution = Some(execution.clone());
        }
    }

    pub(crate) fn accepts_subagent_execution(
        &self,
        child_id: &str,
        execution: &ExecutionBinding,
    ) -> bool {
        self.subagents.iter().rev().any(|s| {
            s.agent_id == child_id
                && s.is_running
                && !s.archived
                && s.execution.as_ref() == Some(execution)
        })
    }

    pub(crate) fn mark_subagent_preview_incomplete(
        &mut self,
        child_id: &str,
        execution: &ExecutionBinding,
    ) {
        if let Some(child) = self
            .subagents
            .iter_mut()
            .rev()
            .find(|s| s.agent_id == child_id && s.execution.as_ref() == Some(execution))
        {
            child.mark_preview_omitted();
            self.invalidate_cache();
        }
    }

    pub(crate) fn mark_subagent_preview_omitted(&mut self, child_id: &str) {
        if let Some(child) = self
            .subagents
            .iter_mut()
            .rev()
            .find(|s| s.agent_id == child_id)
        {
            child.mark_preview_omitted();
            self.invalidate_cache();
        }
    }

    pub(crate) fn bound_subagent_previews(&mut self) -> (bool, Vec<String>) {
        let mut changed = false;
        let mut retired = Vec::new();
        for child in &mut self.subagents {
            // Include the projection allocations before admission. Parent rendering
            // reuses this cache, so publication cannot create an uncharged duplicate.
            let _ = child.view_model();
            if child.preview_reserved() > crate::kit::acp_types::subagent_preview::DETAIL_BYTES {
                child.bound_preview();
                changed = true;
            }
        }
        let mut bytes: usize = self.subagents.iter().map(|s| s.preview_reserved()).sum();
        // Prefer stopped previews, then older running previews. This never changes execution.
        for running in [false, true] {
            for child in self
                .subagents
                .iter_mut()
                .filter(|s| s.is_running == running)
            {
                if bytes <= STORE_BYTES {
                    break;
                }
                let before = child.preview_reserved();
                child.release_preview();
                bytes = bytes
                    .saturating_sub(before)
                    .saturating_add(child.preview_reserved());
                changed = true;
            }
        }
        let mut keep = vec![true; self.subagents.len()];
        let (mut count, mut terminal_bytes) = (0usize, 0usize);
        for (index, child) in self.subagents.iter().enumerate().rev() {
            if child.is_running {
                continue;
            }
            let size = std::mem::size_of_val(child)
                + child.instance_id.capacity()
                + child.agent_id.capacity()
                + child.agent_name.capacity()
                + child
                    .execution
                    .as_ref()
                    .map_or(0, |execution| execution.attempt_id.as_str().len())
                + child.error_reason.as_ref().map_or(0, String::capacity)
                + child.preview_reserved();
            if count >= TERMINAL_ENTRIES || terminal_bytes.saturating_add(size) > TERMINAL_BYTES {
                keep[index] = false;
                changed = true;
            } else {
                count += 1;
                terminal_bytes += size;
            }
        }
        retired.extend(self.retain_subagent_previews(&keep));
        if changed {
            self.cached_view_models = im::Vector::new();
            self.invalidate_cache();
        }
        (changed, retired)
    }

    pub(crate) fn append_subagent_reminder(
        &mut self,
        agent_id: &str,
        reminder: crate::kit::tui_render_unit::TuiSystemReminder,
        omitted: bool,
    ) -> bool {
        let Some(child) = self
            .subagents
            .iter_mut()
            .rev()
            .find(|child| child.agent_id == agent_id && child.is_running)
        else {
            return false;
        };
        child.child_turn.push_system_reminder(reminder);
        child.preview_omitted |= omitted;
        child.cached_view_model.replace(None);
        child.bound_preview();
        self.invalidate_cache();
        true
    }

    fn retain_subagent_previews(&mut self, keep: &[bool]) -> Vec<String> {
        if keep.iter().all(|keep| *keep) {
            return Vec::new();
        }
        let old = std::mem::take(&mut self.subagents);
        let mut remap = vec![None; old.len()];
        let mut retired = Vec::new();
        for (index, child) in old.into_iter().enumerate() {
            if keep[index] {
                remap[index] = Some(self.subagents.len());
                self.subagents.push(child);
            } else {
                retired.push(child.instance_id);
            }
        }
        self.segments = std::mem::take(&mut self.segments)
            .into_iter()
            .filter_map(|segment| match segment {
                TurnSegment::SubAgent { subagent_idx } => {
                    remap[subagent_idx].map(|subagent_idx| TurnSegment::SubAgent { subagent_idx })
                }
                other => Some(other),
            })
            .collect();
        self.pending_subagent_groups = std::mem::take(&mut self.pending_subagent_groups)
            .into_iter()
            .filter_map(|pending| {
                remap[pending.subagent_idx].map(|subagent_idx| PendingSubagentGroup {
                    subagent_idx,
                    parent_tool_call_id: pending.parent_tool_call_id,
                })
            })
            .collect();
        self.cached_view_models = im::Vector::new();
        self.invalidate_cache();
        retired
    }
}
