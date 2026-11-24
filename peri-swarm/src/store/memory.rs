use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use serde_json::Value;

use crate::domain::{EnvironmentKind, ItemOrder, SessionsWindow, TurnStatus, TurnsWindow};

use super::{
    grouped_item_page, InputPut, StoreError, SwarmBindingRow, SwarmInputRow, SwarmItemPage,
    SwarmItemRow, SwarmSessionPatch, SwarmSessionRow, SwarmSessionUpdate, SwarmStore, SwarmTurnRow,
};

/// In-memory fake for the S3 ports. Semantics mirror the future remote store:
/// `(logical_id, seq)` is unique, inputs are insert-if-absent, and rows are
/// returned in deterministic order.
#[derive(Debug, Default)]
pub struct MemorySwarmStore {
    state: Mutex<MemoryState>,
    /// Fault injection for unit tests: number of approval settlement appends
    /// that must fail before the row is accepted.
    #[cfg(test)]
    settlement_failures: std::sync::atomic::AtomicUsize,
}

#[derive(Debug, Default)]
struct MemoryState {
    sessions: BTreeMap<String, SwarmSessionRow>,
    items: BTreeMap<String, BTreeMap<u64, SwarmItemRow>>,
    inputs: HashMap<(String, String), SwarmInputRow>,
    bindings: HashMap<String, SwarmBindingRow>,
    /// `(created_at, turn_id)` is the stable total order used by `turns.list`.
    turns: BTreeMap<String, BTreeMap<(i64, String), SwarmTurnRow>>,
}

impl MemorySwarmStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Test-only: make the next `count` approval settlement appends fail.
    #[cfg(test)]
    pub(crate) fn fail_next_settlement_appends(&self, count: usize) {
        self.settlement_failures
            .store(count, std::sync::atomic::Ordering::SeqCst);
    }

    fn lock(&self) -> MutexGuard<'_, MemoryState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[async_trait]
impl SwarmStore for MemorySwarmStore {
    async fn swarm_upsert_session(&self, row: SwarmSessionRow) -> Result<(), StoreError> {
        let mut state = self.lock();
        if let Some(existing) = state.sessions.get(&row.logical_id) {
            if existing.cwd != row.cwd || existing.root_session_id != row.root_session_id {
                return Err(StoreError::Conflict(format!(
                    "session {} already exists with a different identity",
                    row.logical_id
                )));
            }
        }
        state.sessions.insert(row.logical_id.clone(), row);
        Ok(())
    }

    async fn swarm_create_hosted_session(
        &self,
        row: SwarmSessionRow,
        binding: SwarmBindingRow,
        require_existing: bool,
    ) -> Result<(), StoreError> {
        let mut state = self.lock();
        if state.sessions.contains_key(&row.logical_id)
            || state
                .sessions
                .values()
                .any(|s| s.root_session_id == row.root_session_id)
        {
            return Err(StoreError::Conflict(
                "hosted session identity already exists".to_owned(),
            ));
        }
        let mut binding = binding;
        if require_existing {
            let prior = state
                .bindings
                .get(&binding.logical_env_id)
                .filter(|prior| {
                    prior.sandbox_id == binding.sandbox_id
                        && prior.workspace_path == binding.workspace_path
                })
                .ok_or_else(|| StoreError::NotFound(binding.logical_env_id.clone()))?;
            binding = SwarmBindingRow {
                logical_id: binding.logical_id,
                last_used_at: binding.last_used_at,
                last_run_id: binding.last_run_id.or_else(|| prior.last_run_id.clone()),
                ..prior.clone()
            };
        }
        state.sessions.insert(row.logical_id.clone(), row);
        state
            .bindings
            .insert(binding.logical_env_id.clone(), binding);
        Ok(())
    }

    async fn swarm_get_session(
        &self,
        logical_id: &str,
    ) -> Result<Option<SwarmSessionRow>, StoreError> {
        Ok(self.lock().sessions.get(logical_id).cloned())
    }

    async fn swarm_update_session(&self, update: SwarmSessionUpdate) -> Result<(), StoreError> {
        let mut state = self.lock();
        let session = state
            .sessions
            .get_mut(&update.logical_id)
            .ok_or_else(|| StoreError::NotFound(format!("session {}", update.logical_id)))?;
        session.state = update.state;
        session.last_item_seq = update.last_item_seq;
        session.last_activity_at = update.last_activity_at;
        Ok(())
    }

    async fn swarm_patch_session(&self, patch: SwarmSessionPatch) -> Result<(), StoreError> {
        let mut state = self.lock();
        let session = state
            .sessions
            .get_mut(&patch.logical_id)
            .ok_or_else(|| StoreError::NotFound(format!("session {}", patch.logical_id)))?;
        if let Some(model) = patch.model {
            session.model = Some(model);
        }
        if let Some(mcp_tools) = patch.mcp_tools {
            session.mcp_tools = mcp_tools;
        }
        if let Some(metadata) = patch.metadata {
            session.metadata = metadata;
        }
        if let Some(prompt_cache_options) = patch.prompt_cache_options {
            session.prompt_cache_options = prompt_cache_options;
        }
        if let Some(last_activity_at) = patch.last_activity_at {
            session.last_activity_at = last_activity_at;
        }
        Ok(())
    }

    async fn swarm_list_sessions(
        &self,
        window: &SessionsWindow,
    ) -> Result<Vec<SwarmSessionRow>, StoreError> {
        // The gateway has one root agent identity; any other id selects no
        // session instead of being ignored.
        if window
            .agent_id
            .as_deref()
            .is_some_and(|agent_id| agent_id != "root")
        {
            return Ok(Vec::new());
        }
        let state = self.lock();
        let matches = |row: &SwarmSessionRow| {
            window
                .metadata
                .iter()
                .all(|(key, value)| row.metadata.get(key) == Some(value))
        };
        let probe = window.limit.saturating_add(1) as usize;
        let rows: Vec<SwarmSessionRow> = match window.order {
            ItemOrder::Asc => state
                .sessions
                .values()
                .filter(|row| {
                    window
                        .after
                        .as_ref()
                        .is_none_or(|after| row.logical_id > *after)
                })
                .filter(|row| matches(row))
                .take(probe)
                .cloned()
                .collect(),
            ItemOrder::Desc => state
                .sessions
                .values()
                .rev()
                .filter(|row| {
                    window
                        .after
                        .as_ref()
                        .is_none_or(|after| row.logical_id < *after)
                })
                .filter(|row| matches(row))
                .take(probe)
                .cloned()
                .collect(),
        };
        Ok(rows)
    }

    async fn swarm_delete_session(&self, logical_id: &str) -> Result<(), StoreError> {
        let mut state = self.lock();
        state.sessions.remove(logical_id);
        state.items.remove(logical_id);
        state.turns.remove(logical_id);
        state.inputs.retain(|(session, _), _| session != logical_id);
        let MemoryState {
            bindings, sessions, ..
        } = &mut *state;
        bindings.retain(|env_id, binding| {
            if binding.logical_id != logical_id {
                return true;
            }
            let survivor = sessions.values().find(|session| {
                session.environment.kind == EnvironmentKind::OpenAiHosted
                    && session.environment.id.as_ref() == Some(env_id)
            });
            if let Some(session) = survivor {
                binding.logical_id.clone_from(&session.logical_id);
                true
            } else {
                false
            }
        });
        Ok(())
    }

    async fn swarm_append_item(&self, row: SwarmItemRow) -> Result<(), StoreError> {
        #[cfg(test)]
        if row.payload.get("state").and_then(Value::as_str) == Some("settled") {
            let remaining = self
                .settlement_failures
                .load(std::sync::atomic::Ordering::SeqCst);
            if remaining > 0 {
                self.settlement_failures
                    .store(remaining - 1, std::sync::atomic::Ordering::SeqCst);
                return Err(StoreError::Unavailable(
                    "injected approval settlement append failure".to_owned(),
                ));
            }
        }
        let mut state = self.lock();
        let items = state.items.entry(row.logical_id.clone()).or_default();
        if items.contains_key(&row.seq) {
            return Err(StoreError::Conflict(format!(
                "seq {} already exists for {}",
                row.seq, row.logical_id
            )));
        }
        items.insert(row.seq, row);
        Ok(())
    }

    async fn swarm_append_items(
        &self,
        rows: &[SwarmItemRow],
        progress: Option<SwarmSessionUpdate>,
    ) -> Result<(), StoreError> {
        // Test-only fault injection: one batch counts as one append, and it
        // rejects before any row is written.
        #[cfg(test)]
        if self
            .settlement_failures
            .load(std::sync::atomic::Ordering::SeqCst)
            > 0
            && rows
                .iter()
                .any(|row| row.payload.get("state").and_then(Value::as_str) == Some("settled"))
        {
            let remaining = self
                .settlement_failures
                .load(std::sync::atomic::Ordering::SeqCst);
            self.settlement_failures
                .store(remaining - 1, std::sync::atomic::Ordering::SeqCst);
            return Err(StoreError::Unavailable(
                "injected approval settlement append failure".to_owned(),
            ));
        }
        // One lock scope for validation and mutation: the caller retries a
        // conflict with a reloaded `MAX(seq)`, which is only sound if a
        // rejected run left no row behind, and a missing progress session must
        // not commit the rows either.
        let mut state = self.lock();
        let mut seen = std::collections::HashSet::new();
        for row in rows {
            if !seen.insert((row.logical_id.clone(), row.seq)) {
                return Err(StoreError::Conflict(format!(
                    "seq {} is duplicated inside the run for {}",
                    row.seq, row.logical_id
                )));
            }
            if state
                .items
                .get(&row.logical_id)
                .is_some_and(|items| items.contains_key(&row.seq))
            {
                return Err(StoreError::Conflict(format!(
                    "seq {} already exists for {}",
                    row.seq, row.logical_id
                )));
            }
        }
        if let Some(update) = progress.as_ref() {
            if !state.sessions.contains_key(&update.logical_id) {
                return Err(StoreError::NotFound(format!(
                    "session {}",
                    update.logical_id
                )));
            }
        }
        for row in rows {
            state
                .items
                .entry(row.logical_id.clone())
                .or_default()
                .insert(row.seq, row.clone());
        }
        if let Some(update) = progress {
            let session = state
                .sessions
                .get_mut(&update.logical_id)
                .ok_or_else(|| StoreError::NotFound(format!("session {}", update.logical_id)))?;
            session.state = update.state;
            session.last_item_seq = update.last_item_seq;
            session.last_activity_at = update.last_activity_at;
        }
        Ok(())
    }

    async fn swarm_items_after(
        &self,
        logical_id: &str,
        after: u64,
        limit: u32,
    ) -> Result<Vec<SwarmItemRow>, StoreError> {
        let state = self.lock();
        let Some(items) = state.items.get(logical_id) else {
            return Ok(Vec::new());
        };
        Ok(items
            .range((std::ops::Bound::Excluded(after), std::ops::Bound::Unbounded))
            .take(limit as usize)
            .map(|(_, row)| row.clone())
            .collect())
    }

    async fn swarm_items_before(
        &self,
        logical_id: &str,
        before: Option<u64>,
        limit: u32,
    ) -> Result<Vec<SwarmItemRow>, StoreError> {
        let state = self.lock();
        let Some(items) = state.items.get(logical_id) else {
            return Ok(Vec::new());
        };
        let upper = before.unwrap_or(u64::MAX);
        Ok(items
            .range(..upper)
            .rev()
            .take(limit as usize)
            .map(|(_, row)| row.clone())
            .collect())
    }

    async fn swarm_item_page(
        &self,
        logical_id: &str,
        order: ItemOrder,
        after: Option<u64>,
        limit: u32,
    ) -> Result<SwarmItemPage, StoreError> {
        let state = self.lock();
        let Some(rows) = state.items.get(logical_id) else {
            return Ok(grouped_item_page(Vec::new(), order, limit));
        };
        // Item membership is decided by each item's greatest `seq`, so a cursor
        // resolved from an item id advances past the whole item.
        let mut maxima: BTreeMap<&str, u64> = BTreeMap::new();
        for row in rows.values() {
            let entry = maxima.entry(row.item_id.as_str()).or_insert(row.seq);
            *entry = (*entry).max(row.seq);
        }
        let mut selected: Vec<(&str, u64)> = maxima
            .into_iter()
            .filter(|(_, max_seq)| match (order, after) {
                (ItemOrder::Asc, Some(after)) => *max_seq > after,
                (ItemOrder::Desc, Some(after)) => *max_seq < after,
                (_, None) => true,
            })
            .collect();
        selected.sort_by_key(|(_, max_seq)| *max_seq);
        if order == ItemOrder::Desc {
            selected.reverse();
        }
        selected.truncate(limit.saturating_add(1) as usize);
        let keep: BTreeSet<&str> = selected.into_iter().map(|(item_id, _)| item_id).collect();
        let page_rows: Vec<SwarmItemRow> = rows
            .values()
            .filter(|row| keep.contains(row.item_id.as_str()))
            .cloned()
            .collect();
        Ok(grouped_item_page(page_rows, order, limit))
    }

    async fn swarm_last_seq(&self, logical_id: &str) -> Result<u64, StoreError> {
        let state = self.lock();
        Ok(state
            .items
            .get(logical_id)
            .and_then(BTreeMap::last_key_value)
            .map_or(0, |(seq, _)| *seq))
    }

    async fn swarm_get_item(
        &self,
        logical_id: &str,
        item_id: &str,
    ) -> Result<Option<SwarmItemRow>, StoreError> {
        Ok(self.lock().items.get(logical_id).and_then(|items| {
            items
                .values()
                .rev()
                .find(|row| row.item_id == item_id)
                .cloned()
        }))
    }

    async fn swarm_put_input(&self, row: SwarmInputRow) -> Result<InputPut, StoreError> {
        let mut state = self.lock();
        let key = (row.logical_id.clone(), row.input_id.clone());
        if let Some(existing) = state.inputs.get(&key) {
            return Ok(InputPut::Existing(existing.clone()));
        }
        state.inputs.insert(key, row);
        Ok(InputPut::Inserted)
    }

    async fn swarm_get_input(
        &self,
        logical_id: &str,
        input_id: &str,
    ) -> Result<Option<SwarmInputRow>, StoreError> {
        Ok(self
            .lock()
            .inputs
            .get(&(logical_id.to_owned(), input_id.to_owned()))
            .cloned())
    }

    async fn swarm_put_binding(&self, row: SwarmBindingRow) -> Result<(), StoreError> {
        let mut state = self.lock();
        state.bindings.insert(row.logical_env_id.clone(), row);
        Ok(())
    }

    async fn swarm_get_binding(
        &self,
        logical_env_id: &str,
    ) -> Result<Option<SwarmBindingRow>, StoreError> {
        Ok(self.lock().bindings.get(logical_env_id).cloned())
    }

    async fn swarm_upsert_turn(&self, row: SwarmTurnRow) -> Result<(), StoreError> {
        let mut state = self.lock();
        let turns = state.turns.entry(row.logical_id.clone()).or_default();
        // Keep the opening `created_at`: a terminal write carries the same
        // value, but the store must not re-key the row if a caller omits it.
        let created_at = turns
            .iter()
            .find(|((_, turn_id), _)| turn_id == &row.turn_id)
            .map_or(row.created_at, |((created_at, _), _)| *created_at);
        turns.retain(|(_, turn_id), _| turn_id != &row.turn_id);
        let mut row = row;
        row.created_at = created_at;
        turns.insert((created_at, row.turn_id.clone()), row);
        Ok(())
    }

    async fn swarm_get_turn(
        &self,
        logical_id: &str,
        turn_id: &str,
    ) -> Result<Option<SwarmTurnRow>, StoreError> {
        Ok(self
            .lock()
            .turns
            .get(logical_id)
            .and_then(|turns| turns.values().find(|row| row.turn_id == turn_id).cloned()))
    }

    async fn swarm_turn_page(
        &self,
        logical_id: &str,
        window: &TurnsWindow,
    ) -> Result<Vec<SwarmTurnRow>, StoreError> {
        let state = self.lock();
        let Some(turns) = state.turns.get(logical_id) else {
            return Ok(Vec::new());
        };
        let after_key = window.after.as_ref().and_then(|after| {
            turns
                .iter()
                .find(|((_, turn_id), _)| turn_id == after)
                .map(|(key, _)| key.clone())
        });
        let probe = window.limit.saturating_add(1) as usize;
        let rows: Vec<SwarmTurnRow> = match (window.order, after_key) {
            (ItemOrder::Asc, Some(after)) => turns
                .range((std::ops::Bound::Excluded(after), std::ops::Bound::Unbounded))
                .take(probe)
                .map(|(_, row)| row.clone())
                .collect(),
            (ItemOrder::Asc, None) => turns.values().take(probe).cloned().collect(),
            (ItemOrder::Desc, Some(after)) => turns
                .range(..after)
                .rev()
                .take(probe)
                .map(|(_, row)| row.clone())
                .collect(),
            (ItemOrder::Desc, None) => turns.values().rev().take(probe).cloned().collect(),
        };
        Ok(rows)
    }

    async fn swarm_close_open_turns(
        &self,
        logical_id: &str,
        status: TurnStatus,
        completed_at: i64,
        error: Option<Value>,
    ) -> Result<u64, StoreError> {
        let mut state = self.lock();
        let Some(turns) = state.turns.get_mut(logical_id) else {
            return Ok(0);
        };
        let mut updated = 0u64;
        for row in turns.values_mut() {
            if row.status.is_open() {
                row.status = status;
                row.completed_at = Some(completed_at);
                row.error = error.clone();
                updated += 1;
            }
        }
        Ok(updated)
    }

    async fn swarm_has_open_turn(&self, logical_id: &str) -> Result<bool, StoreError> {
        let state = self.lock();
        Ok(state
            .turns
            .get(logical_id)
            .is_some_and(|turns| turns.values().any(|row| row.status.is_open())))
    }
}

#[cfg(test)]
#[path = "memory_test.rs"]
mod tests;
