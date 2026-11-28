//! The single [`SwarmStore`] implementation for [`SwarmStateStore`].
//!
//! Rust allows one trait impl per type, so the bodies live in the responsibility
//! modules (`session`, `item`, `turn`) as inherent methods and this block only
//! preserves the port's names and signatures. The signature set is the contract
//! in `store/mod.rs`; nothing here adds a method or changes a return type.

use async_trait::async_trait;
use serde_json::Value;

use super::SwarmStateStore;
use crate::domain::{ItemOrder, SessionsWindow, TurnStatus, TurnsWindow};
use crate::store::{
    InputPut, StoreError, SwarmBindingRow, SwarmInputRow, SwarmItemPage, SwarmItemRow,
    SwarmSessionPatch, SwarmSessionRow, SwarmSessionUpdate, SwarmStore, SwarmTurnRow,
    SwarmTurnUsageRow,
};

#[async_trait]
impl SwarmStore for SwarmStateStore {
    async fn swarm_upsert_session(&self, row: SwarmSessionRow) -> Result<(), StoreError> {
        self.upsert_session(row).await
    }

    async fn swarm_create_hosted_session(
        &self,
        row: SwarmSessionRow,
        binding: SwarmBindingRow,
        require_existing: bool,
    ) -> Result<(), StoreError> {
        self.create_hosted_session(row, binding, require_existing)
            .await
    }

    async fn swarm_get_session(
        &self,
        logical_id: &str,
    ) -> Result<Option<SwarmSessionRow>, StoreError> {
        self.get_session(logical_id).await
    }

    async fn swarm_update_session(&self, update: SwarmSessionUpdate) -> Result<(), StoreError> {
        self.update_session(update).await
    }

    async fn swarm_patch_session(&self, patch: SwarmSessionPatch) -> Result<(), StoreError> {
        self.patch_session(patch).await
    }

    async fn swarm_list_sessions(
        &self,
        window: &SessionsWindow,
    ) -> Result<Vec<SwarmSessionRow>, StoreError> {
        self.list_sessions(window).await
    }

    async fn swarm_delete_session(&self, logical_id: &str) -> Result<(), StoreError> {
        self.delete_session(logical_id).await
    }

    async fn swarm_append_item(&self, row: SwarmItemRow) -> Result<(), StoreError> {
        self.append_item(row).await
    }

    async fn swarm_append_items(
        &self,
        rows: &[SwarmItemRow],
        progress: Option<SwarmSessionUpdate>,
    ) -> Result<(), StoreError> {
        self.append_items(rows, progress).await
    }

    async fn swarm_items_after(
        &self,
        logical_id: &str,
        after: u64,
        limit: u32,
    ) -> Result<Vec<SwarmItemRow>, StoreError> {
        self.items_after(logical_id, after, limit).await
    }

    async fn swarm_items_before(
        &self,
        logical_id: &str,
        before: Option<u64>,
        limit: u32,
    ) -> Result<Vec<SwarmItemRow>, StoreError> {
        self.items_before(logical_id, before, limit).await
    }

    async fn swarm_item_page(
        &self,
        logical_id: &str,
        order: ItemOrder,
        after: Option<u64>,
        limit: u32,
    ) -> Result<SwarmItemPage, StoreError> {
        self.item_page(logical_id, order, after, limit).await
    }

    async fn swarm_last_seq(&self, logical_id: &str) -> Result<u64, StoreError> {
        self.last_seq(logical_id).await
    }

    async fn swarm_get_item(
        &self,
        logical_id: &str,
        item_id: &str,
    ) -> Result<Option<SwarmItemRow>, StoreError> {
        self.get_item(logical_id, item_id).await
    }

    async fn swarm_put_input(&self, row: SwarmInputRow) -> Result<InputPut, StoreError> {
        self.put_input(row).await
    }

    async fn swarm_get_input(
        &self,
        logical_id: &str,
        input_id: &str,
    ) -> Result<Option<SwarmInputRow>, StoreError> {
        self.get_input(logical_id, input_id).await
    }

    async fn swarm_put_binding(&self, row: SwarmBindingRow) -> Result<(), StoreError> {
        self.put_binding(row).await
    }

    async fn swarm_get_binding(
        &self,
        logical_env_id: &str,
    ) -> Result<Option<SwarmBindingRow>, StoreError> {
        self.get_binding(logical_env_id).await
    }

    async fn swarm_upsert_turn(&self, row: SwarmTurnRow) -> Result<(), StoreError> {
        self.upsert_turn(row).await
    }

    async fn swarm_record_turn_usage(
        &self,
        logical_id: &str,
        turn_id: &str,
        created_at: i64,
        started_at: Option<i64>,
        usage: &Value,
    ) -> Result<(), StoreError> {
        self.record_turn_usage(logical_id, turn_id, created_at, started_at, usage)
            .await
    }

    async fn swarm_get_turn(
        &self,
        logical_id: &str,
        turn_id: &str,
    ) -> Result<Option<SwarmTurnRow>, StoreError> {
        self.get_turn(logical_id, turn_id).await
    }

    async fn swarm_turn_page(
        &self,
        logical_id: &str,
        window: &TurnsWindow,
    ) -> Result<Vec<SwarmTurnRow>, StoreError> {
        self.turn_page(logical_id, window).await
    }

    async fn swarm_session_usage_turns(
        &self,
        logical_id: &str,
        limit: u32,
    ) -> Result<Vec<SwarmTurnUsageRow>, StoreError> {
        self.session_usage_turns(logical_id, limit).await
    }

    async fn swarm_turn_usage_window(
        &self,
        start_at: i64,
        end_at: i64,
        limit: u32,
    ) -> Result<Vec<SwarmTurnUsageRow>, StoreError> {
        self.turn_usage_window(start_at, end_at, limit).await
    }

    async fn swarm_close_open_turns(
        &self,
        logical_id: &str,
        status: TurnStatus,
        completed_at: i64,
        error: Option<Value>,
    ) -> Result<u64, StoreError> {
        self.close_open_turns(logical_id, status, completed_at, error)
            .await
    }

    async fn swarm_has_open_turn(&self, logical_id: &str) -> Result<bool, StoreError> {
        self.has_open_turn(logical_id).await
    }
}
