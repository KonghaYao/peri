//! Load 准入、连接准备与同一 registry 锁内的 capability 提交。

use super::{DynamicEntry, DynamicMcpRegistry, OperationRecord, RegistryState};
use crate::mcp::task_scope::{DynamicMcpTaskKind, McpTaskKey, TaskAdmissionError};
use peri_acp_types::dynamic_mcp::{
    CanonicalDynamicMcpLoadRequest, DynamicMcpErrorCode, DynamicMcpFailure, DynamicMcpInstanceKey,
    DynamicMcpLogicalKey, DynamicMcpOperationId, DynamicMcpOperationState, DynamicMcpResponse,
};
use std::sync::Arc;

impl DynamicMcpRegistry {
    pub(super) async fn load(
        &self,
        session_id: &str,
        request: CanonicalDynamicMcpLoadRequest,
    ) -> Result<DynamicMcpResponse, DynamicMcpFailure> {
        let logical = DynamicMcpLogicalKey {
            session_id: session_id.to_string(),
            server_name: request.name.clone(),
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
            if state
                .entries
                .get(&logical)
                .is_some_and(|entry| entry.state == DynamicMcpOperationState::Failed)
            {
                state.entries.remove(&logical);
            }
            if let Some(entry) = state.entries.get(&logical) {
                if matches!(
                    entry.state,
                    DynamicMcpOperationState::Revoking | DynamicMcpOperationState::Draining
                ) {
                    return Err(Self::failure(
                        DynamicMcpErrorCode::ServerBusy,
                        entry.state,
                        "Dynamic MCP server is draining",
                    ));
                }
                if entry.config != request.config {
                    return Err(Self::failure(
                        DynamicMcpErrorCode::ConfigConflict,
                        entry.state,
                        "A different configuration already uses this server name",
                    ));
                }
                let operation = state
                    .operations
                    .get(&entry.load_operation)
                    .expect("load operation exists");
                return Ok(Self::accepted(operation, true));
            }
            let instance = DynamicMcpInstanceKey {
                logical: logical.clone(),
                incarnation_id: Default::default(),
            };
            let operation_id = DynamicMcpOperationId::new();
            state.operations.insert(
                operation_id.clone(),
                OperationRecord {
                    operation_id: operation_id.clone(),
                    instance: instance.clone(),
                    config: request.config.clone(),
                    state: DynamicMcpOperationState::Starting,
                    error: None,
                    tool_count: 0,
                    resource_count: 0,
                },
            );
            state.entries.insert(
                logical,
                DynamicEntry {
                    instance: instance.clone(),
                    config: request.config,
                    load_operation: operation_id.clone(),
                    unload_operation: None,
                    state: DynamicMcpOperationState::Starting,
                    active: None,
                },
            );
            (instance, operation_id)
        };
        let _ = self.notify_operation(&operation_id);

        let (start_tx, start_rx) = tokio::sync::oneshot::channel();
        let weak = self.self_weak.clone();
        let key = McpTaskKey::dynamic(DynamicMcpTaskKind::Connect, &instance);
        let task_instance = instance.clone();
        let task_operation = operation_id.clone();
        let admission = self.task_spawner.spawn(key, async move {
            if start_rx.await.is_ok() {
                if let Some(registry) = weak.upgrade() {
                    registry.run_load(task_instance, task_operation).await;
                }
            }
        });
        match admission {
            Ok(()) => {
                let state = self.state.lock();
                let operation = state
                    .operations
                    .get(&operation_id)
                    .expect("reserved operation exists");
                let response = Self::accepted(operation, false);
                drop(state);
                let _ = start_tx.send(());
                Ok(response)
            }
            Err(error) => {
                let failure = Self::failure(
                    match error {
                        TaskAdmissionError::OwnerClosed => DynamicMcpErrorCode::TaskOwnerClosed,
                        TaskAdmissionError::DuplicateKey => DynamicMcpErrorCode::Internal,
                    },
                    DynamicMcpOperationState::Failed,
                    "Dynamic MCP task could not be admitted",
                );
                let mut state = self.state.lock();
                if let Some(operation) = state.operations.get_mut(&operation_id) {
                    operation.state = DynamicMcpOperationState::Failed;
                    operation.error = Some(failure.clone());
                }
                if let Some(entry) = state.entries.get_mut(&instance.logical) {
                    entry.state = DynamicMcpOperationState::Failed;
                }
                Err(failure)
            }
        }
    }

    async fn run_load(
        self: Arc<Self>,
        instance: DynamicMcpInstanceKey,
        operation_id: DynamicMcpOperationId,
    ) {
        let config = {
            let mut state = self.state.lock();
            if !self.load_commit_allowed(&state, &instance, &operation_id) {
                return;
            }
            let config = state
                .entries
                .get(&instance.logical)
                .expect("checked entry exists")
                .config
                .clone();
            state
                .entries
                .get_mut(&instance.logical)
                .expect("checked entry exists")
                .state = DynamicMcpOperationState::Connecting;
            state
                .operations
                .get_mut(&operation_id)
                .expect("checked operation exists")
                .state = DynamicMcpOperationState::Connecting;
            config
        };
        let _ = self.notify_operation(&operation_id);
        let connection_key = crate::mcp::client::McpConnectionKey::dynamic(instance.clone());
        debug_assert!(connection_key.is_dynamic());
        let weak = self.self_weak.clone();
        let progress_instance = instance.clone();
        let progress_operation = operation_id.clone();
        let progress: Arc<dyn Fn(DynamicMcpOperationState) + Send + Sync> = Arc::new(move |next| {
            let Some(registry) = weak.upgrade() else {
                return;
            };
            let mut state = registry.state.lock();
            if !registry.load_commit_allowed(&state, &progress_instance, &progress_operation) {
                return;
            }
            state
                .entries
                .get_mut(&progress_instance.logical)
                .expect("checked entry exists")
                .state = next;
            state
                .operations
                .get_mut(&progress_operation)
                .expect("checked operation exists")
                .state = next;
            drop(state);
            let _ = registry.notify_operation(&progress_operation);
        });
        let staged = match self
            .connector
            .prepare(instance.clone(), operation_id.clone(), config, progress)
            .await
        {
            Ok(staged) => staged,
            Err(failure) => {
                self.fail_operation(&instance, &operation_id, failure);
                return;
            }
        };
        let allowed = {
            let state = self.state.lock();
            self.load_commit_allowed(&state, &instance, &operation_id)
        };
        if !allowed {
            if let Err(failure) = staged.cleanup().await {
                self.fail_operation(&instance, &operation_id, failure);
            }
            return;
        }
        {
            let mut state = self.state.lock();
            state
                .entries
                .get_mut(&instance.logical)
                .expect("checked entry exists")
                .state = DynamicMcpOperationState::Discovering;
            state
                .operations
                .get_mut(&operation_id)
                .expect("checked operation exists")
                .state = DynamicMcpOperationState::Discovering;
        }
        let _ = self.notify_operation(&operation_id);
        let dynamic_tools =
            match self.build_instance_tools(&staged.instance_key, &staged.handle, &staged.gate) {
                Ok(tools) => tools,
                Err(failure) => {
                    let cleanup_failure = staged.cleanup().await.err();
                    self.fail_operation(
                        &instance,
                        &operation_id,
                        cleanup_failure.unwrap_or(failure),
                    );
                    return;
                }
            };
        let active = Arc::new(staged.commit());
        let commit_result = {
            let mut state = self.state.lock();
            if !self.load_commit_allowed(&state, &instance, &operation_id) {
                Err(())
            } else if self.tools_collide(&state, &instance, &dynamic_tools) {
                let failure = Self::failure(
                    DynamicMcpErrorCode::ToolNameConflict,
                    DynamicMcpOperationState::Discovering,
                    "Dynamic MCP tool names conflict with the current catalog",
                );
                if let Some(operation) = state.operations.get_mut(&operation_id) {
                    operation.state = DynamicMcpOperationState::Failed;
                    operation.error = Some(failure.clone());
                }
                if let Some(entry) = state.entries.get_mut(&instance.logical) {
                    entry.state = DynamicMcpOperationState::Failed;
                }
                Err(())
            } else {
                let tool_count = active.handle.tools.len();
                let resource_count = active.handle.resources.len();
                let entry = state
                    .entries
                    .get_mut(&instance.logical)
                    .expect("checked entry exists");
                entry.state = DynamicMcpOperationState::Ready;
                entry.active = Some(Arc::clone(&active));
                let operation = state
                    .operations
                    .get_mut(&operation_id)
                    .expect("checked operation exists");
                operation.state = DynamicMcpOperationState::Ready;
                operation.tool_count = tool_count;
                operation.resource_count = resource_count;
                self.publish_capability(&mut state, &instance, dynamic_tools);
                Ok(())
            }
        };
        let _ = self.notify_operation(&operation_id);
        if commit_result.is_err() {
            let _ = active.close().await;
        }
    }

    fn load_commit_allowed(
        &self,
        state: &RegistryState,
        instance: &DynamicMcpInstanceKey,
        operation_id: &DynamicMcpOperationId,
    ) -> bool {
        !state.closing
            && !state.closed_sessions.contains(&instance.logical.session_id)
            && state.entries.get(&instance.logical).is_some_and(|entry| {
                entry.instance == *instance && entry.load_operation == *operation_id
            })
            && state.operations.contains_key(operation_id)
    }

    fn fail_operation(
        &self,
        instance: &DynamicMcpInstanceKey,
        operation_id: &DynamicMcpOperationId,
        failure: DynamicMcpFailure,
    ) {
        let mut state = self.state.lock();
        if !self.load_commit_allowed(&state, instance, operation_id) {
            return;
        }
        if let Some(operation) = state.operations.get_mut(operation_id) {
            operation.state = DynamicMcpOperationState::Failed;
            operation.error = Some(failure);
        }
        if let Some(entry) = state.entries.get_mut(&instance.logical) {
            entry.state = DynamicMcpOperationState::Failed;
        }
        drop(state);
        let _ = self.notify_operation(operation_id);
    }
}
