//! Bounded Todo comparison inputs. Missing previews never mean an empty baseline.

use super::BridgeState;
use crate::kit::retained_bytes::json_value_bytes;
use crate::kit::tool_semantics::TodoSnapshot;
use serde_json::Value;

pub(super) const INPUT_BYTES: usize = 64 * 1024;
const CACHE_BYTES: usize = 1024 * 1024;
const CACHE_ENTRIES: usize = 128;

impl BridgeState {
    /// No snapshot and no frontier means initial; a frontier without a snapshot
    /// means comparison data was lost and must not produce an initial Todo card.
    pub(crate) fn todo_baseline_unknown(&self) -> bool {
        self.last_successful_todos.is_none() && self.todo_baseline_sequence.is_some()
    }

    fn invalidate_todo_baseline(&mut self, sequence: u64) {
        if self
            .todo_baseline_sequence
            .is_none_or(|frontier| sequence >= frontier)
        {
            self.last_successful_todos = None;
            self.todo_baseline_sequence = Some(sequence);
        }
    }

    /// Measure borrowed input before copying it. Child display limits may be
    /// smaller than the root comparison limit; either omission advances the floor.
    pub(crate) fn record_todo_started(
        &mut self,
        tool_id: String,
        raw_input: &Value,
        input_limit: usize,
    ) {
        let sequence = self.next_todo_sequence;
        self.next_todo_sequence = self.next_todo_sequence.saturating_add(1);
        if raw_input.is_null()
            || json_value_bytes(raw_input).reserved > input_limit.min(INPUT_BYTES)
        {
            self.todo_call_inputs.remove(&tool_id);
            self.invalidate_todo_baseline(sequence);
            return;
        }
        self.todo_call_inputs
            .insert(tool_id, (sequence, raw_input.clone()));
        while self.todo_call_inputs.len() > CACHE_ENTRIES
            || self
                .todo_call_inputs
                .iter()
                .map(|(id, (_, value))| id.capacity() + json_value_bytes(value).reserved)
                .sum::<usize>()
                > CACHE_BYTES
        {
            let oldest = self
                .todo_call_inputs
                .iter()
                .min_by_key(|(_, (sequence, _))| *sequence)
                .map(|(id, _)| id.clone());
            let Some(id) = oldest else { break };
            if let Some((sequence, _)) = self.todo_call_inputs.remove(&id) {
                self.invalidate_todo_baseline(sequence);
            }
        }
        if self.todo_call_inputs.capacity() > CACHE_ENTRIES * 2 {
            self.todo_call_inputs.shrink_to_fit();
        }
    }

    /// A complete success at or beyond the frontier restores a known baseline.
    /// Completion is independent of whether its display card is still resident.
    pub(crate) fn complete_todo_if_current(&mut self, tool_id: &str, is_error: bool) -> bool {
        let Some((sequence, raw_input)) = self.todo_call_inputs.remove(tool_id) else {
            return false;
        };
        if is_error
            || self
                .todo_baseline_sequence
                .is_some_and(|frontier| sequence < frontier)
        {
            return false;
        }
        let Some(snapshot) = TodoSnapshot::parse(&raw_input) else {
            self.invalidate_todo_baseline(sequence);
            return false;
        };
        self.last_successful_todos = Some(snapshot);
        self.todo_baseline_sequence = Some(sequence);
        true
    }
}

#[cfg(test)]
#[path = "todo_cache_test.rs"]
mod tests;
