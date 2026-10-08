//! Workspace discovery and scoped history queries remain owned by the host.

use peri_acp::transport::types::AcpError;
use peri_acp_types::workspace::{
    MachineInfo, ResolvedWorkspace, ScopedThreadEntry, ScopedThreadPage, ScopedThreadQuery,
    SessionBinding, ThreadListCursor, ThreadScope, WorkspaceInfo,
};
use serde::Deserialize;
use serde_json::json;

use super::AcpTuiClient;

#[derive(Debug, Deserialize)]
pub(crate) struct SessionContext {
    pub version: u16,
    pub workspace: ResolvedWorkspace,
    pub binding: Option<SessionBinding>,
}

#[derive(Deserialize)]
struct ThreadPageProjection {
    threads: Vec<ScopedThreadEntry>,
    #[serde(rename = "nextCursor")]
    next_cursor: Option<ThreadListCursor>,
}

#[derive(Deserialize)]
struct MachinePage {
    machines: Vec<MachineInfo>,
}

#[derive(Deserialize)]
struct WorkspacePage {
    workspaces: Vec<WorkspaceInfo>,
}

impl AcpTuiClient {
    pub(crate) async fn list_machines(&self) -> Result<Vec<MachineInfo>, AcpError> {
        let response = self
            .send_raw_request("peri/machines/list", json!({}))
            .await?;
        serde_json::from_value::<MachinePage>(response)
            .map(|page| page.machines)
            .map_err(|error| AcpError::new(-32603, format!("invalid Machine list: {error}")))
    }

    pub(crate) async fn list_workspaces(
        &self,
        machine_id: &str,
    ) -> Result<Vec<WorkspaceInfo>, AcpError> {
        let response = self
            .send_raw_request("peri/workspaces/list", json!({"machineId": machine_id}))
            .await?;
        serde_json::from_value::<WorkspacePage>(response)
            .map(|page| page.workspaces)
            .map_err(|error| AcpError::new(-32603, format!("invalid Workspace list: {error}")))
    }

    pub(crate) async fn set_session_archived(
        &self,
        session_id: &str,
        archived: bool,
    ) -> Result<(), AcpError> {
        self.send_raw_request(
            "peri/session/archive",
            json!({"sessionId": session_id, "archived": archived}),
        )
        .await?;
        Ok(())
    }

    pub(crate) async fn list_archived_threads(
        &self,
        query: &ScopedThreadQuery,
    ) -> Result<ScopedThreadPage, AcpError> {
        if !self.supports_session_workspace() {
            return Err(AcpError::new(
                -32601,
                "host does not support session workspace identity",
            ));
        }
        let response = self
            .send_raw_request(
                "session/list",
                json!({
                    "_meta": {"peri.sessionWorkspaceV1": {
                        "scope": query.scope, "cursor": query.cursor, "limit": query.limit,
                        "archived": true
                    }}
                }),
            )
            .await?;
        let projection = response
            .pointer("/_meta/peri.sessionWorkspaceV1")
            .cloned()
            .ok_or_else(|| AcpError::new(-32603, "missing archived session list"))?;
        let projection: ThreadPageProjection =
            serde_json::from_value(projection).map_err(|error| {
                AcpError::new(-32603, format!("invalid archived session list: {error}"))
            })?;
        Ok(ScopedThreadPage {
            entries: projection.threads,
            next_cursor: projection.next_cursor,
        })
    }
    pub(crate) fn supports_session_workspace(&self) -> bool {
        self.session_workspace
            .load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn current_execution_cwd(&self) -> Option<String> {
        self.execution_cwd.borrow().clone()
    }

    pub(crate) fn subscribe_execution_cwd(&self) -> tokio::sync::watch::Receiver<Option<String>> {
        self.execution_cwd.subscribe()
    }

    pub(crate) async fn session_context(
        &self,
        session_id: Option<&str>,
        cwd: Option<&str>,
    ) -> Result<SessionContext, AcpError> {
        if !self.supports_session_workspace() {
            return Err(AcpError::new(
                -32601,
                "host does not support session workspace identity",
            ));
        }
        let params = match (session_id, cwd) {
            (Some(id), None) => json!({"sessionId": id}),
            (None, Some(cwd)) => json!({"cwd": cwd}),
            _ => return Err(AcpError::new(-32602, "expected sessionId or cwd")),
        };
        let value = self
            .send_raw_request("peri/session_context", params)
            .await?;
        let context: SessionContext = serde_json::from_value(value)
            .map_err(|e| AcpError::new(-32603, format!("invalid session context: {e}")))?;
        if context.version != 1 {
            return Err(AcpError::new(-32603, "unsupported session context version"));
        }
        if context.binding.as_ref().is_some_and(|binding| {
            binding.schema_version != peri_acp_types::workspace::SESSION_BINDING_VERSION
                || binding.project_id != context.workspace.project_id
                || binding.workspace_id != context.workspace.workspace_id
                || binding.cwd_relative_to_workspace != context.workspace.relative_cwd
        }) {
            return Err(AcpError::new(-32603, "inconsistent session binding"));
        }
        Ok(context)
    }

    pub(crate) async fn read_session_history(
        &self,
        session_id: &str,
    ) -> Result<Vec<peri_acp_types::store::PersistedPayload>, AcpError> {
        let response = self
            .send_raw_request("peri/session_history", json!({"sessionId": session_id}))
            .await?;
        if response
            .get("sessionId")
            .and_then(serde_json::Value::as_str)
            != Some(session_id)
        {
            return Err(AcpError::new(-32603, "history session identity mismatch"));
        }
        let payloads = response
            .get("payloads")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| AcpError::new(-32603, "missing history payloads"))?;
        payloads
            .iter()
            .map(|payload| {
                peri_acp_types::store::deserialize_persisted_payload(&payload.to_string())
                    .map_err(|_| AcpError::new(-32603, "invalid history payload"))
            })
            .collect()
    }

    pub(crate) async fn list_scoped_threads(
        &self,
        query: &ScopedThreadQuery,
    ) -> Result<ScopedThreadPage, AcpError> {
        if !self.supports_session_workspace() {
            return Err(AcpError::new(
                -32601,
                "host does not support session workspace identity",
            ));
        }
        let result = self
            .send_raw_request(
                "session/list",
                json!({
                    "_meta": {"peri.sessionWorkspaceV1": query}
                }),
            )
            .await?;
        let projection = result
            .pointer("/_meta/peri.sessionWorkspaceV1")
            .cloned()
            .ok_or_else(|| AcpError::new(-32603, "missing scoped session list"))?;
        let projection: ThreadPageProjection = serde_json::from_value(projection)
            .map_err(|e| AcpError::new(-32603, format!("invalid scoped session list: {e}")))?;
        Ok(ScopedThreadPage {
            entries: projection.threads,
            next_cursor: projection.next_cursor,
        })
    }

    pub(crate) async fn latest_thread_in_directory(
        &self,
        cwd: &str,
    ) -> Result<Option<String>, AcpError> {
        let workspace = self.session_context(None, Some(cwd)).await?.workspace;
        let page = self
            .list_scoped_threads(&ScopedThreadQuery {
                scope: ThreadScope::ExactDirectory {
                    workspace_id: workspace.workspace_id,
                    relative_cwd: workspace.relative_cwd,
                },
                limit: 1,
                cursor: None,
            })
            .await?;
        Ok(page.entries.first().map(|entry| entry.thread.id.clone()))
    }

    pub(crate) async fn fail_startup_restore(&self, error: &AcpError) {
        let _operation = self.lifecycle.operation_gate().lock().await;
        *self.restore_error.lock().unwrap() = Some(error.to_string());
        self.startup_restore_tx.send_replace(false);
    }

    pub(super) fn check_restore_error(&self) -> Result<(), AcpError> {
        match self.restore_error.lock().unwrap().as_ref() {
            Some(error) => Err(AcpError::new(-32602, error.clone())),
            None => Ok(()),
        }
    }

    pub(super) fn project_execution_cwd(&self, cwd: Option<String>) {
        if self.projection_mode == super::ClientProjectionMode::Interactive {
            crate::kit::atoms::ACTIVE_EXECUTION_CWD.set(cwd.clone());
            if let Some(cwd) = cwd.as_ref() {
                let state = crate::kit::atoms::SERVICE_SNAPSHOT.state();
                let mut snapshot = state.write();
                snapshot.cwd.clone_from(cwd);
                snapshot.permission_mode.clear();
                snapshot.model_alias.clear();
                snapshot.model_name.clear();
                snapshot.provider_name.clear();
                snapshot.effort.clear();
            }
            crate::kit::atoms::FILE_LIST.state().write().clear();
            crate::kit::atoms::HOOK_LIST.state().write().clear();
            crate::kit::atoms::PLUGIN_LIST.state().write().clear();
            crate::kit::atoms::MCP_SERVERS.state().write().clear();
        }
        // Publish after the complete UI projection. Even the same directory can
        // belong to a newly loaded session with different mode/model settings.
        self.execution_cwd.send_replace(cwd);
    }
}

#[cfg(test)]
#[path = "workspace_test.rs"]
mod tests;
