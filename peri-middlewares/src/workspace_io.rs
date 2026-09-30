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

struct PendingFileRead {
    peer: Peer<RoleClient>,
    id: Option<RequestId>,
}

impl Drop for PendingFileRead {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            let peer = self.peer.clone();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(1),
                        peer.notify_cancelled(CancelledNotificationParam::new(
                            Some(id),
                            Some("workspace text read cancelled".into()),
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
            pool: pool.filter(|_| !crate::mcp::builtin::is_closed("workspace", &closed)),
            session_id,
        }
    }
}

#[async_trait]
impl WorkspaceFileReader for McpWorkspaceFileReader {
    async fn read_text(&self, path: &Path) -> Result<String, WorkspaceReadError> {
        let pool = self
            .pool
            .as_ref()
            .filter(|pool| !workspace_closed(pool))
            .ok_or(WorkspaceReadError::Unavailable)?;
        let handle = pool.get_client_visible_to("workspace", self.session_id.as_deref())
            .filter(|handle| matches!(handle.status, ClientStatus::Connected))
            .filter(|handle| matches!(handle.source.as_ref(), Some(crate::mcp::config::ConfigSource::Builtin { instance }) if instance == "workspace"))
            .ok_or(WorkspaceReadError::Unavailable)?;
        let peer = handle
            .peer
            .as_ref()
            .ok_or(WorkspaceReadError::Unavailable)?;
        let generation = pool.handle_generation(&handle);
        let path = path.to_str().ok_or(WorkspaceReadError::InvalidPath)?;
        let request = CustomRequest::new(
            "workspace/readText",
            Some(serde_json::json!({ "path": path })),
        );
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
        let mut pending = PendingFileRead {
            peer: peer.clone(),
            id: Some(request.id.clone()),
        };
        let result = tokio::time::timeout_at(deadline, request.await_response())
            .await
            .map_err(|_| WorkspaceReadError::Timeout)?
            .map_err(|_| WorkspaceReadError::ReadFailed)?;
        pending.id.take();
        if workspace_closed(pool)
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
            ServerResult::CustomResult(result) => result
                .0
                .get("text")
                .and_then(|text| text.as_str())
                .map(str::to_string)
                .ok_or(WorkspaceReadError::InvalidResponse),
            _ => Err(WorkspaceReadError::InvalidResponse),
        }
    }
}

fn workspace_closed(pool: &McpClientPool) -> bool {
    !pool.is_open()
        || pool
            .builtin_instance_context()
            .is_some_and(|context| crate::mcp::builtin::is_closed("workspace", &context.closed))
}

#[cfg(test)]
#[path = "workspace_io_test.rs"]
mod tests;
