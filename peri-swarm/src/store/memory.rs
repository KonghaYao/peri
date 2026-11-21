use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;

use super::{
    InputPut, StoreError, SwarmBindingRow, SwarmInputRow, SwarmItemRow, SwarmSessionRow,
    SwarmSessionUpdate, SwarmStore,
};

/// In-memory fake for the S3 ports. Semantics mirror the future remote store:
/// `(logical_id, seq)` is unique, inputs are insert-if-absent, and rows are
/// returned in deterministic order.
#[derive(Debug, Default)]
pub struct MemorySwarmStore {
    state: Mutex<MemoryState>,
}

#[derive(Debug, Default)]
struct MemoryState {
    sessions: BTreeMap<String, SwarmSessionRow>,
    items: BTreeMap<String, BTreeMap<u64, SwarmItemRow>>,
    inputs: HashMap<(String, String), SwarmInputRow>,
    bindings: HashMap<String, SwarmBindingRow>,
}

impl MemorySwarmStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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

    async fn swarm_append_item(&self, row: SwarmItemRow) -> Result<(), StoreError> {
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
}

#[cfg(test)]
#[path = "memory_test.rs"]
mod tests;
