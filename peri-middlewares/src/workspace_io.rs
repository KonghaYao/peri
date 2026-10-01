use std::{path::Path, sync::Arc, time::Duration};

use async_trait::async_trait;
use rmcp::{
    model::{CancelledNotificationParam, ClientRequest, CustomRequest, RequestId, ServerResult},
    service::{Peer, PeerRequestOptions, RoleClient},
};

use crate::mcp::{ClientStatus, McpClientPool};

#[async_trait]
pub trait WorkspaceFileReader: Send + Sync {
    async fn read_text(&self, path: &Path) -> Result<String, WorkspaceReadError>;

    async fn current_branch(&self) -> Result<Option<String>, WorkspaceReadError> {
        Err(WorkspaceReadError::Unavailable)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WorkspaceReadError {
    #[error("workspace reader unavailable")]
    Unavailable,
    #[error("workspace path is not UTF-8")]
    InvalidPath,
    #[error("workspace read timed out")]
    Timeout,
    #[error("workspace read failed")]
    ReadFailed,
    #[error("invalid workspace read response")]
    InvalidResponse,
}

pub(crate) struct McpWorkspaceFileReader {
    pool: Option<Arc<McpClientPool>>,
    session_id: Option<String>,
}

struct PendingWorkspaceRequest {
    peer: Peer<RoleClient>,
    id: Option<RequestId>,
}

impl Drop for PendingWorkspaceRequest {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            let peer = self.peer.clone();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(1),
                        peer.notify_cancelled(CancelledNotificationParam::new(
                            Some(id),
                            Some("workspace request cancelled".into()),
                        )),
                    )
                    .await;
                });
            }
        }
    }
}

impl McpWorkspaceFileReader {
    pub(crate) fn new(
        pool: Option<Arc<McpClientPool>>,
        session_id: Option<String>,
        disabled: &std::collections::HashSet<String>,
    ) -> Self {
        let closed = crate::mcp::builtin::closed_instances(disabled);
        Self {
            pool: pool.filter(|pool| {
                let source = pool
                    .get_client("workspace")
                    .and_then(|handle| handle.source.clone());
                !crate::mcp::builtin::is_closed_source("workspace", source.as_ref(), &closed)
            }),
            session_id,
        }
    }
    async fn request(
        &self,
        method: &str,
        path: Option<&Path>,
    ) -> Result<serde_json::Value, WorkspaceReadError> {
        let pool = self
            .pool
            .as_ref()
            .filter(|pool| !workspace_closed(pool))
            .ok_or(WorkspaceReadError::Unavailable)?;
        let handle = pool
            .get_client_visible_to("workspace", self.session_id.as_deref())
            .filter(|handle| matches!(handle.status, ClientStatus::Connected))
            .filter(|handle| crate::mcp::builtin::is_workspace_source(handle.source.as_ref()))
            .ok_or(WorkspaceReadError::Unavailable)?;
        let peer = handle
            .peer
            .as_ref()
            .ok_or(WorkspaceReadError::Unavailable)?;
        let generation = pool.handle_generation(&handle);
        let owner = pool.acp_owners.read().get("workspace").cloned();
        let params = path
            .map(|path| {
                path.to_str()
                    .map(|path| serde_json::json!({ "path": path }))
                    .ok_or(WorkspaceReadError::InvalidPath)
            })
            .transpose()?;
        let request = CustomRequest::new(method, params);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let request = tokio::time::timeout_at(
            deadline,
            peer.send_request_with_option(
                ClientRequest::CustomRequest(request),
                PeerRequestOptions::no_options(),
            ),
        )
        .await
        .map_err(|_| WorkspaceReadError::Timeout)?
        .map_err(|_| WorkspaceReadError::ReadFailed)?;
        let mut pending = PendingWorkspaceRequest {
            peer: peer.clone(),
            id: Some(request.id.clone()),
        };
        let result = tokio::time::timeout_at(deadline, request.await_response())
            .await
            .map_err(|_| WorkspaceReadError::Timeout)?
            .map_err(|_| WorkspaceReadError::ReadFailed)?;
        pending.id.take();
        if workspace_closed(pool)
            || pool.acp_owners.read().get("workspace").cloned() != owner
            || !pool
                .get_client_visible_to("workspace", self.session_id.as_deref())
                .is_some_and(|current| {
                    Arc::ptr_eq(&current, &handle)
                        && matches!(current.status, ClientStatus::Connected)
                        && pool.handle_generation(&current) == generation
                })
        {
            return Err(WorkspaceReadError::Unavailable);
        }
        match result {
            ServerResult::CustomResult(result) => Ok(result.0),
            _ => Err(WorkspaceReadError::InvalidResponse),
        }
    }
}

#[async_trait]
impl WorkspaceFileReader for McpWorkspaceFileReader {
    async fn read_text(&self, path: &Path) -> Result<String, WorkspaceReadError> {
        self.request("workspace/readText", Some(path))
            .await?
            .get("text")
            .and_then(|text| text.as_str())
            .map(str::to_string)
            .ok_or(WorkspaceReadError::InvalidResponse)
    }

    async fn current_branch(&self) -> Result<Option<String>, WorkspaceReadError> {
        let result = self.request("workspace/gitBranch", None).await?;
        match result.get("branch") {
            Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::String(branch)) => Ok(Some(branch.clone())),
            _ => Err(WorkspaceReadError::InvalidResponse),
        }
    }
}

fn workspace_closed(pool: &McpClientPool) -> bool {
    !pool.is_open()
        || pool.builtin_instance_context().is_some_and(|context| {
            let source = pool
                .get_client("workspace")
                .and_then(|handle| handle.source.clone());
            crate::mcp::builtin::is_closed_source("workspace", source.as_ref(), &context.closed)
        })
}

#[cfg(test)]
#[path = "workspace_io_test.rs"]
mod tests;
