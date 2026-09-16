//! Workspace discovery and scoped history queries remain owned by the host.

use peri_acp::transport::types::AcpError;
use peri_acp_types::workspace::{
    ResolvedWorkspace, ScopedThreadEntry, ScopedThreadPage, ScopedThreadQuery, SessionBinding,
    ThreadListCursor, ThreadScope,
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

impl AcpTuiClient {
    pub(crate) fn supports_session_workspace(&self) -> bool {
        self.session_workspace
            .load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn current_execution_cwd(&self) -> Option<String> {
        self.execution_cwd.lock().unwrap().clone()
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
        if context.version != 1 || (session_id.is_some() && context.binding.is_none()) {
            return Err(AcpError::new(
                -32603,
                "unsupported or missing session binding",
            ));
        }
        Ok(context)
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
        *self.execution_cwd.lock().unwrap() = cwd.clone();
        if self.projection_mode == super::ClientProjectionMode::Interactive {
            crate::kit::atoms::ACTIVE_EXECUTION_CWD.set(cwd.clone());
            if let Some(cwd) = cwd {
                let state = crate::kit::atoms::SERVICE_SNAPSHOT.state();
                let mut snapshot = state.write();
                snapshot.cwd = cwd;
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
    }
}

#[cfg(test)]
#[path = "workspace_test.rs"]
mod tests;
