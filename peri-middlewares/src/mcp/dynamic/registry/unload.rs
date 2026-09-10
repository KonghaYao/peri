//! Unload 的精确 incarnation 撤销、任务停止与在途调用 drain。

use super::{DynamicMcpRegistry, OperationRecord};
use crate::mcp::task_scope::{DynamicMcpTaskKind, McpTaskKey};
use peri_acp_types::dynamic_mcp::{
    CanonicalDynamicMcpUnloadRequest, DynamicMcpErrorCode, DynamicMcpFailure,
    DynamicMcpInstanceKey, DynamicMcpLogicalKey, DynamicMcpOperationId, DynamicMcpOperationState,
    DynamicMcpResponse,
};
use std::sync::Arc;

impl DynamicMcpRegistry {
    pub(super) async fn unload(
        &self,
        session_id: &str,
        request: CanonicalDynamicMcpUnloadRequest,
    ) -> Result<DynamicMcpResponse, DynamicMcpFailure> {
        let logical = DynamicMcpLogicalKey {
            session_id: session_id.to_string(),
            server_name: request.name,
        };
        let (instance, operation_id) = {
            let mut state = self.state.lock();
            if state.closing || state.closed_sessions.contains(session_id) {
                return Err(Self::failure(
                    DynamicMcpErrorCode::TaskOwnerClosed,
                    DynamicMcpOperationState::Failed,
                    "Dynamic MCP task admission is closed",
                ));
            }
            let Some(entry) = state.entries.get(&logical) else {
                return Err(Self::failure(
                    DynamicMcpErrorCode::NotFound,
                    DynamicMcpOperationState::Failed,
                    "Dynamic MCP server was not found",
                ));
            };
            if request
                .expected_instance
                .as_ref()
                .is_some_and(|expected| expected != &entry.instance)
            {
                return Err(Self::failure(
                    DynamicMcpErrorCode::ServerBusy,
                    entry.state,
                    "Dynamic MCP server incarnation changed before unload",
                ));
            }
            if let Some(operation_id) = &entry.unload_operation {
                let operation = state
                    .operations
                    .get(operation_id)
                    .expect("unload operation exists");
                if operation.state != DynamicMcpOperationState::Failed {
                    return Ok(Self::accepted(operation, true));
                }
            }
            let instance = entry.instance.clone();
            let config = entry.config.clone();
            let operation_id = DynamicMcpOperationId::new();
            state.operations.insert(
                operation_id.clone(),
                OperationRecord {
                    operation_id: operation_id.clone(),
                    instance: instance.clone(),
                    config,
                    state: DynamicMcpOperationState::Revoking,
                    error: None,
                    tool_count: 0,
                    resource_count: 0,
                },
            );
            state
                .entries
                .get_mut(&logical)
                .expect("checked entry exists")
                .unload_operation = Some(operation_id.clone());
            (instance, operation_id)
        };
        let (start_tx, start_rx) = tokio::sync::oneshot::channel();
        let weak = self.self_weak.clone();
        let task_instance = instance.clone();
        let task_operation = operation_id.clone();
        let admission = self.task_spawner.spawn(
            McpTaskKey::dynamic(DynamicMcpTaskKind::Unload, &instance),
            async move {
                if start_rx.await.is_ok() {
                    if let Some(registry) = weak.upgrade() {
                        registry.run_unload(task_instance, task_operation).await;
                    }
                }
            },
        );
        match admission {
            Ok(()) => {
                let response = {
                    let mut state = self.state.lock();
                    let active = state
                        .entries
                        .get(&instance.logical)
                        .and_then(|entry| entry.active.clone());
                    if let Some(active) = active {
                        active.gate.begin_draining();
                    }
                    if let Some(entry) = state.entries.get_mut(&instance.logical) {
                        entry.state = DynamicMcpOperationState::Draining;
                    }
                    if let Some(operation) = state.operations.get_mut(&operation_id) {
                        operation.state = DynamicMcpOperationState::Draining;
                    }
                    self.revoke_capability(&mut state, &instance);
                    Self::accepted(
                        state
                            .operations
                            .get(&operation_id)
                            .expect("reserved operation exists"),
                        false,
                    )
                };
                let _ = self.notify_operation(&operation_id);
                let _ = start_tx.send(());
                Ok(response)
            }
            Err(_) => {
                let failure = Self::failure(
                    DynamicMcpErrorCode::TaskOwnerClosed,
                    DynamicMcpOperationState::Failed,
                    "Dynamic MCP unload task could not be admitted",
                );
                let mut state = self.state.lock();
                if let Some(operation) = state.operations.get_mut(&operation_id) {
                    operation.state = DynamicMcpOperationState::Failed;
                    operation.error = Some(failure.clone());
                }
                if let Some(entry) = state.entries.get_mut(&instance.logical) {
                    entry.unload_operation = None;
                }
                Err(failure)
            }
        }
    }

    async fn run_unload(
        self: Arc<Self>,
        instance: DynamicMcpInstanceKey,
        operation_id: DynamicMcpOperationId,
    ) {
        self.task_spawner
            .stop_instance_except(&instance, DynamicMcpTaskKind::Unload)
            .await;
        let active = self
            .state
            .lock()
            .entries
            .get(&instance.logical)
            .filter(|entry| entry.instance == instance)
            .and_then(|entry| entry.active.clone());
        let result = if let Some(active) = &active {
            match tokio::time::timeout(self.drain_timeout, active.gate.drain()).await {
                Ok(()) => active.close().await,
                Err(_) => Err(Self::failure(
                    DynamicMcpErrorCode::ShutdownIncomplete,
                    DynamicMcpOperationState::Draining,
                    "Dynamic MCP in-flight calls did not drain in time",
                )),
            }
        } else {
            Ok(())
        };
        let mut state = self.state.lock();
        let entry_matches = state
            .entries
            .get(&instance.logical)
            .is_some_and(|entry| entry.instance == instance);
        if !entry_matches {
            return;
        }
        match result {
            Ok(()) => {
                if let Some(active) = active {
                    active.gate.close();
                }
                if let Some(operation) = state.operations.get_mut(&operation_id) {
                    operation.state = DynamicMcpOperationState::Unloaded;
                }
                state.entries.remove(&instance.logical);
            }
            Err(failure) => {
                if let Some(operation) = state.operations.get_mut(&operation_id) {
                    operation.state = DynamicMcpOperationState::Failed;
                    operation.error = Some(failure);
                }
                if let Some(entry) = state.entries.get_mut(&instance.logical) {
                    entry.state = DynamicMcpOperationState::Failed;
                }
            }
        }
        drop(state);
        let _ = self.notify_operation(&operation_id);
    }
}
