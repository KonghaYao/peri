//! Operation 响应、状态查询与带 instance 检查的通知投影。

use super::{DynamicMcpRegistry, OperationRecord, RegistryState};
use peri_acp_types::dynamic_mcp::{
    DynamicMcpAccepted, DynamicMcpErrorCode, DynamicMcpFailure, DynamicMcpNotification,
    DynamicMcpOperationId, DynamicMcpOperationState, DynamicMcpOperationStatus, DynamicMcpResponse,
    DynamicMcpStatusRequest, DynamicMcpStatusResponse,
};
use std::sync::Weak;

fn notification_text(operation: &OperationRecord) -> (String, &'static str) {
    let server = &operation.instance.logical.server_name;
    match operation.state {
        DynamicMcpOperationState::Starting => (format!("Dynamic MCP {server} is starting"), "info"),
        DynamicMcpOperationState::Authorizing => (
            format!("Dynamic MCP {server} is awaiting authorization"),
            "info",
        ),
        DynamicMcpOperationState::Connecting => {
            (format!("Dynamic MCP {server} is connecting"), "info")
        }
        DynamicMcpOperationState::Discovering => (
            format!("Dynamic MCP {server} is discovering capabilities"),
            "info",
        ),
        DynamicMcpOperationState::Ready => (format!("Dynamic MCP {server} is ready"), "info"),
        DynamicMcpOperationState::Revoking | DynamicMcpOperationState::Draining => {
            (format!("Dynamic MCP {server} is draining"), "info")
        }
        DynamicMcpOperationState::Unloaded => {
            (format!("Dynamic MCP {server} was unloaded"), "info")
        }
        DynamicMcpOperationState::Failed => {
            let code = operation
                .error
                .as_ref()
                .map_or("INTERNAL", |failure| failure.code.as_str());
            (format!("Dynamic MCP {server} failed ({code})"), "error")
        }
    }
}

impl DynamicMcpRegistry {
    pub(super) fn failure(
        code: DynamicMcpErrorCode,
        phase: DynamicMcpOperationState,
        summary: &'static str,
    ) -> DynamicMcpFailure {
        DynamicMcpFailure::new(code, phase, summary)
    }

    fn operation_status(
        state: &RegistryState,
        operation: &OperationRecord,
    ) -> DynamicMcpOperationStatus {
        let generation = state
            .capabilities
            .get(&operation.instance.logical.session_id)
            .map_or(0, |snapshot| snapshot.generation);
        DynamicMcpOperationStatus {
            operation_id: operation.operation_id.clone(),
            server: operation.instance.logical.server_name.clone(),
            state: operation.state,
            instance_key: operation.instance.clone(),
            config: operation.config.safe_summary(),
            error: operation.error.clone(),
            tool_count: operation.tool_count,
            resource_count: operation.resource_count,
            capability_generation: generation,
        }
    }

    pub(super) fn accepted(operation: &OperationRecord, idempotent: bool) -> DynamicMcpResponse {
        DynamicMcpResponse::Accepted(DynamicMcpAccepted {
            operation_id: operation.operation_id.clone(),
            server: operation.instance.logical.server_name.clone(),
            state: operation.state,
            scope: "session".to_string(),
            idempotent,
        })
    }

    pub(super) fn notify_operation(&self, operation_id: &DynamicMcpOperationId) -> bool {
        let (sink, notification) = {
            let state = self.state.lock();
            let Some(operation) = state.operations.get(operation_id) else {
                return false;
            };
            let session_id = &operation.instance.logical.session_id;
            if state.closing || state.closed_sessions.contains(session_id) {
                return false;
            }
            let current_matches = state
                .entries
                .get(&operation.instance.logical)
                .is_some_and(|entry| entry.instance == operation.instance);
            let completed_unload = operation.state == DynamicMcpOperationState::Unloaded
                && !state.entries.contains_key(&operation.instance.logical);
            if !current_matches && !completed_unload {
                return false;
            }
            let Some(sink) = state
                .notification_sinks
                .get(session_id)
                .and_then(Weak::upgrade)
            else {
                return false;
            };
            let (text, _) = notification_text(operation);
            (
                sink,
                DynamicMcpNotification {
                    session_id: operation.instance.logical.session_id.clone(),
                    operation_id: operation.operation_id.clone(),
                    instance_key: operation.instance.clone(),
                    state: operation.state,
                    safe_summary: text,
                },
            )
        };
        sink.accepts(&notification.instance_key) && sink.notify(notification)
    }

    pub(super) fn status(
        &self,
        session_id: &str,
        request: DynamicMcpStatusRequest,
    ) -> Result<DynamicMcpResponse, DynamicMcpFailure> {
        let state = self.state.lock();
        if state.closed_sessions.contains(session_id) {
            return Err(Self::failure(
                DynamicMcpErrorCode::NotFound,
                DynamicMcpOperationState::Failed,
                "Dynamic MCP state was not found",
            ));
        }
        let operations = state
            .operations
            .values()
            .filter(|operation| operation.instance.logical.session_id == session_id)
            .filter(|operation| {
                request
                    .operation_id
                    .as_ref()
                    .is_none_or(|id| id == &operation.operation_id)
            })
            .filter(|operation| {
                request
                    .name
                    .as_ref()
                    .is_none_or(|name| name == &operation.instance.logical.server_name)
            })
            .map(|operation| Self::operation_status(&state, operation))
            .collect::<Vec<_>>();
        if (request.operation_id.is_some() || request.name.is_some()) && operations.is_empty() {
            return Err(Self::failure(
                DynamicMcpErrorCode::NotFound,
                DynamicMcpOperationState::Failed,
                "Dynamic MCP state was not found",
            ));
        }
        let generation = state
            .capabilities
            .get(session_id)
            .map_or(0, |snapshot| snapshot.generation);
        Ok(DynamicMcpResponse::Status(DynamicMcpStatusResponse {
            operations,
            capability_generation: generation,
        }))
    }
}
