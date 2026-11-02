//! Runtime child identity and bounded retirement watermarks for display routing.

use crate::kit::acp_types::{AcpEventData, CurrentTurn};
use peri_acp_types::event::ChildEventOrigin;
use std::collections::{HashMap, VecDeque};

const RETIRED_RUNS: usize = 512;

#[derive(Clone)]
struct ActiveRun {
    origin: ChildEventOrigin,
    name: String,
    background: bool,
}

/// Identities scale with observed live executions. Retired display identities are bounded.
#[derive(Default)]
pub(crate) struct ChildRunRouting {
    active: HashMap<String, ActiveRun>,
    retired: VecDeque<ChildEventOrigin>,
    // An unknown first Start older than this bounded window expires its preview.
    // No total arrival order across child pumps or business completion is implied.
    retired_floor: u64,
}

/// Business task settlement uses its own registry ownership, not preview lifetime.
pub(crate) fn is_child_preview_event(event: &AcpEventData) -> bool {
    matches!(
        event,
        AcpEventData::SubagentStarted { .. }
            | AcpEventData::SubagentStopped { .. }
            | AcpEventData::TextChunk(_)
            | AcpEventData::ReasoningChunk(_)
            | AcpEventData::ToolStarted(_)
            | AcpEventData::ToolEnded(_)
            | AcpEventData::SystemReminder { .. }
    )
}

fn event_child_id(event: &AcpEventData) -> Option<&str> {
    match event {
        AcpEventData::SubagentStarted { agent_id, .. }
        | AcpEventData::SubagentStopped { agent_id, .. } => Some(agent_id),
        AcpEventData::TextChunk(chunk) => chunk.agent_id.as_deref(),
        AcpEventData::ReasoningChunk(chunk) => chunk.agent_id.as_deref(),
        AcpEventData::ToolStarted(tool) => tool.agent_id.as_deref(),
        AcpEventData::ToolEnded(tool) => tool.agent_id.as_deref(),
        AcpEventData::SystemReminder {
            source_agent_id, ..
        } => source_agent_id.as_deref(),
        _ => None,
    }
}

fn reject(origin: &ChildEventOrigin, reason: &'static str) -> bool {
    tracing::debug!(target: "tui.child_preview", run_order = origin.run_order,
        sequence = origin.sequence, reason, "child preview event rejected");
    false
}

impl ChildRunRouting {
    pub(crate) fn accept(
        &mut self,
        event: &AcpEventData,
        origin: &ChildEventOrigin,
        same_view: bool,
    ) -> bool {
        if !origin.valid() || event_child_id(event) != Some(origin.child_thread_id.as_str()) {
            return reject(origin, "invalid_origin_or_source");
        }
        if let AcpEventData::SubagentStarted {
            agent_id,
            agent_name,
            is_background,
            ..
        } = event
        {
            let reason =
                if self
                    .active
                    .get(agent_id)
                    .is_some_and(|current| current.origin.run_order >= origin.run_order)
                {
                    Some("active_run_is_newer_or_duplicate")
                } else if self.retired.iter().any(|old| {
                    old.child_thread_id == *agent_id && old.run_order >= origin.run_order
                }) {
                    Some("run_already_retired")
                } else if origin.run_order <= self.retired_floor {
                    Some("first_start_outside_retirement_window")
                } else {
                    None
                };
            if let Some(reason) = reason {
                return reject(origin, reason);
            }
            let end = agent_name.floor_char_boundary(agent_name.len().min(256));
            self.active.insert(
                agent_id.clone(),
                ActiveRun {
                    origin: origin.clone(),
                    name: agent_name[..end].to_owned(),
                    background: *is_background,
                },
            );
            return true;
        }
        let Some(active) = self.active.get_mut(&origin.child_thread_id) else {
            return reject(origin, "unknown_or_retired_run");
        };
        if active.origin.execution != origin.execution
            || active.origin.run_order != origin.run_order
        {
            return reject(origin, "different_execution");
        }
        if origin.sequence <= active.origin.sequence {
            return reject(origin, "duplicate_or_reordered_sequence");
        }
        active.origin.sequence = origin.sequence;
        if matches!(event, AcpEventData::SubagentStopped { agent_id, .. } if agent_id == &origin.child_thread_id)
        {
            self.active.remove(&origin.child_thread_id);
            self.retired.push_back(origin.clone());
            while self.retired.len() > RETIRED_RUNS {
                if let Some(old) = self.retired.pop_front() {
                    self.retired_floor = self.retired_floor.max(old.run_order);
                }
            }
            if self.active.capacity() > self.active.len().saturating_mul(4).max(64) {
                self.active.shrink_to_fit();
            }
            // A real terminal still closes a rebound run even when queued before reset.
            return true;
        }
        same_view || reject(origin, "queued_before_view_reset")
    }

    pub(crate) fn restore(&self, turn: &mut CurrentTurn) {
        for (id, run) in &self.active {
            turn.start_subagent_with_background(id.clone(), run.name.clone(), run.background, None);
            turn.bind_subagent_execution(id, &run.origin.execution);
            turn.mark_subagent_preview_incomplete(id, &run.origin.execution);
        }
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
#[path = "child_run_routing_test.rs"]
mod tests;
