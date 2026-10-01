use std::sync::Arc;

use rmcp::{
    model::{CustomRequest, CustomResult},
    service::{RequestContext, RoleServer, RunningService},
    RoleClient, ServerHandler, ServiceExt,
};
use tokio::sync::Notify;

use crate::mcp::{config::ConfigSource, McpClientHandle, OAuthStatus};

use super::*;

struct DelayedReader {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    cancelled: Arc<Notify>,
    branch_response: serde_json::Value,
}

impl ServerHandler for DelayedReader {
    async fn on_custom_request(
        &self,
        request: CustomRequest,
        context: RequestContext<RoleServer>,
    ) -> Result<CustomResult, rmcp::ErrorData> {
        assert!(matches!(
            request.method.as_str(),
            "workspace/readText" | "workspace/gitBranch"
        ));
        let result = if request.method == "workspace/gitBranch" {
            assert!(request
                .params
                .as_ref()
                .and_then(|params| params.get("path"))
                .is_none());
            assert!(request
                .params
                .as_ref()
                .and_then(|params| params.get("cwd"))
                .is_none());
            self.branch_response.clone()
        } else {
            serde_json::json!({"text": "remote"})
        };
        self.entered.notify_one();
        tokio::select! {
            biased;
            _ = context.ct.cancelled() => {
                self.cancelled.notify_one();
                Err(rmcp::ErrorData::internal_error("cancelled", None))
            }
            _ = self.release.notified() => Ok(CustomResult::new(result)),
        }
    }
}

struct Fixture {
    pool: Arc<McpClientPool>,
    client: RunningService<RoleClient, ()>,
    server: tokio::task::JoinHandle<()>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    cancelled: Arc<Notify>,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_branch_response(serde_json::json!({"branch": "remote-branch"})).await
    }

    async fn with_branch_response(branch_response: serde_json::Value) -> Self {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let cancelled = Arc::new(Notify::new());
        let handler = DelayedReader {
            entered: entered.clone(),
            release: release.clone(),
            cancelled: cancelled.clone(),
            branch_response,
        };
        let (client_io, server_io) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            handler
                .serve(server_io)
                .await
                .unwrap()
                .waiting()
                .await
                .unwrap();
        });
        let client = tokio::time::timeout(Duration::from_secs(2), ().serve(client_io))
            .await
            .unwrap()
            .unwrap();
        let pool = Arc::new(McpClientPool::new_empty());
        let handle = Arc::new(McpClientHandle {
            name: "workspace".to_string(),
            version: None,
            cache_version: None,
            peer: Some(client.peer().clone()),
            tools: Vec::new(),
            resources: Vec::new(),
            status: ClientStatus::Connected,
            oauth_status: OAuthStatus::None,
            source: Some(ConfigSource::Builtin {
                instance: "workspace".to_string(),
            }),
            url: None,
            skills_capable: false,
        });
        pool.advance_handle_generation(&handle);
        pool.clients.write().insert("workspace".to_string(), handle);
        Self {
            pool,
            client,
            server,
            entered,
            release,
            cancelled,
        }
    }

    fn read(&self) -> tokio::task::JoinHandle<Result<String, WorkspaceReadError>> {
        let reader = McpWorkspaceFileReader::new(
            Some(self.pool.clone()),
            Some("session".to_string()),
            &Default::default(),
        );
        tokio::spawn(async move { reader.read_text(Path::new("remote.txt")).await })
    }

    fn branch(&self) -> tokio::task::JoinHandle<Result<Option<String>, WorkspaceReadError>> {
        let reader = McpWorkspaceFileReader::new(
            Some(self.pool.clone()),
            Some("session".to_string()),
            &Default::default(),
        );
        tokio::spawn(async move { reader.current_branch().await })
    }

    async fn shutdown(mut self) {
        self.pool.clients.write().clear();
        self.client
            .close_with_timeout(Duration::from_secs(1))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), self.server)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn missing_workspace_never_falls_back_to_host_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("host.txt");
    std::fs::write(&path, "host").unwrap();
    let reader = McpWorkspaceFileReader::new(None, None, &Default::default());
    assert!(reader.read_text(&path).await.is_err());
}

#[tokio::test]
async fn construct_time_workspace_closure_is_enforced() {
    let fixture = Fixture::new().await;
    let reader = McpWorkspaceFileReader::new(
        Some(fixture.pool.clone()),
        Some("session".to_string()),
        &std::collections::HashSet::from(["WorkspaceMiddleware".to_string()]),
    );
    assert_eq!(
        reader.read_text(Path::new("remote.txt")).await.unwrap_err(),
        WorkspaceReadError::Unavailable
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn dropping_read_notifies_server_cancellation() {
    let fixture = Fixture::new().await;
    let read = fixture.read();
    tokio::time::timeout(Duration::from_secs(1), fixture.entered.notified())
        .await
        .unwrap();
    read.abort();
    assert!(read.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(1), fixture.cancelled.notified())
        .await
        .unwrap();
    fixture.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn timed_out_read_notifies_server_cancellation() {
    let fixture = Fixture::new().await;
    let read = fixture.read();
    fixture.entered.notified().await;
    tokio::time::advance(Duration::from_secs(11)).await;
    assert_eq!(
        read.await.unwrap().unwrap_err(),
        WorkspaceReadError::Timeout
    );
    tokio::time::timeout(Duration::from_secs(1), fixture.cancelled.notified())
        .await
        .unwrap();
    fixture.shutdown().await;
}

#[tokio::test]
async fn reconnected_workspace_response_is_rejected() {
    let fixture = Fixture::new().await;
    let read = fixture.read();
    tokio::time::timeout(Duration::from_secs(1), fixture.entered.notified())
        .await
        .unwrap();
    let current = fixture.pool.get_client("workspace").unwrap();
    let replacement = Arc::new((*current).clone());
    fixture.pool.advance_handle_generation(&replacement);
    fixture
        .pool
        .clients
        .write()
        .insert("workspace".to_string(), replacement);
    fixture.release.notify_one();
    assert!(read.await.unwrap().is_err());
    fixture.shutdown().await;
}

#[tokio::test]
async fn branch_construct_time_workspace_closure_is_enforced() {
    let fixture = Fixture::new().await;
    let reader = McpWorkspaceFileReader::new(
        Some(fixture.pool.clone()),
        Some("session".to_string()),
        &std::collections::HashSet::from(["WorkspaceMiddleware".to_string()]),
    );
    assert_eq!(
        reader.current_branch().await.unwrap_err(),
        WorkspaceReadError::Unavailable
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn dropping_branch_notifies_server_cancellation() {
    let fixture = Fixture::new().await;
    let read = fixture.branch();
    tokio::time::timeout(Duration::from_secs(1), fixture.entered.notified())
        .await
        .unwrap();
    read.abort();
    assert!(read.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(1), fixture.cancelled.notified())
        .await
        .unwrap();
    fixture.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn timed_out_branch_notifies_server_cancellation() {
    let fixture = Fixture::new().await;
    let read = fixture.branch();
    tokio::time::timeout(Duration::from_secs(1), fixture.entered.notified())
        .await
        .unwrap();
    tokio::time::advance(Duration::from_secs(11)).await;
    assert_eq!(
        read.await.unwrap().unwrap_err(),
        WorkspaceReadError::Timeout
    );
    tokio::time::timeout(Duration::from_secs(1), fixture.cancelled.notified())
        .await
        .unwrap();
    fixture.shutdown().await;
}

#[tokio::test]
async fn branch_reconnected_workspace_response_is_rejected() {
    let fixture = Fixture::new().await;
    let read = fixture.branch();
    tokio::time::timeout(Duration::from_secs(1), fixture.entered.notified())
        .await
        .unwrap();
    let current = fixture.pool.get_client("workspace").unwrap();
    let replacement = Arc::new((*current).clone());
    fixture.pool.advance_handle_generation(&replacement);
    fixture
        .pool
        .clients
        .write()
        .insert("workspace".to_string(), replacement);
    fixture.release.notify_one();
    assert!(read.await.unwrap().is_err());
    fixture.shutdown().await;
}

#[tokio::test]
async fn branch_removed_workspace_response_is_rejected() {
    let fixture = Fixture::new().await;
    let read = fixture.branch();
    tokio::time::timeout(Duration::from_secs(1), fixture.entered.notified())
        .await
        .unwrap();
    fixture.pool.clients.write().remove("workspace");
    fixture.release.notify_one();
    assert!(read.await.unwrap().is_err());
    fixture.shutdown().await;
}

#[tokio::test]
async fn branch_workspace_closed_before_response_is_rejected() {
    let fixture = Fixture::new().await;
    let read = fixture.branch();
    tokio::time::timeout(Duration::from_secs(1), fixture.entered.notified())
        .await
        .unwrap();
    let mut context = crate::mcp::builtin::context::BuiltinInstanceContext::new("/workspace");
    context.closed.insert("workspace".to_string());
    fixture
        .pool
        .set_builtin_instance_context(Arc::new(context))
        .unwrap();
    fixture.release.notify_one();
    assert!(read.await.unwrap().is_err());
    fixture.shutdown().await;
}

#[tokio::test]
async fn branch_ownership_changed_before_response_is_rejected() {
    let fixture = Fixture::new().await;
    let read = fixture.branch();
    tokio::time::timeout(Duration::from_secs(1), fixture.entered.notified())
        .await
        .unwrap();
    fixture
        .pool
        .acp_owners
        .write()
        .insert("workspace".to_string(), "session".to_string());
    fixture.release.notify_one();
    assert!(read.await.unwrap().is_err());
    fixture.shutdown().await;
}

#[tokio::test]
async fn branch_uses_current_workspace_binding() {
    let fixture = Fixture::new().await;
    let branch = fixture.branch();
    tokio::time::timeout(Duration::from_secs(1), fixture.entered.notified())
        .await
        .unwrap();
    fixture.release.notify_one();
    assert_eq!(branch.await.unwrap().unwrap(), Some("remote-branch".into()));
    fixture.shutdown().await;
}

#[tokio::test]
async fn missing_workspace_never_falls_back_to_host_git() {
    let reader = McpWorkspaceFileReader::new(None, None, &Default::default());
    assert_eq!(
        reader.current_branch().await,
        Err(WorkspaceReadError::Unavailable)
    );
}

#[tokio::test]
async fn branch_response_requires_string_or_null() {
    for (response, expected) in [
        (serde_json::json!({"branch": null}), Ok(None)),
        (
            serde_json::json!({}),
            Err(WorkspaceReadError::InvalidResponse),
        ),
        (
            serde_json::json!({"branch": 42}),
            Err(WorkspaceReadError::InvalidResponse),
        ),
    ] {
        let fixture = Fixture::with_branch_response(response).await;
        let branch = fixture.branch();
        tokio::time::timeout(Duration::from_secs(1), fixture.entered.notified())
            .await
            .unwrap();
        fixture.release.notify_one();
        assert_eq!(branch.await.unwrap(), expected);
        fixture.shutdown().await;
    }
}

#[tokio::test]
async fn removed_workspace_response_is_rejected() {
    let fixture = Fixture::new().await;
    let read = fixture.read();
    tokio::time::timeout(Duration::from_secs(1), fixture.entered.notified())
        .await
        .unwrap();
    fixture.pool.clients.write().remove("workspace");
    fixture.release.notify_one();
    assert!(read.await.unwrap().is_err());
    fixture.shutdown().await;
}

#[tokio::test]
async fn workspace_closed_before_response_is_rejected() {
    let fixture = Fixture::new().await;
    let read = fixture.read();
    tokio::time::timeout(Duration::from_secs(1), fixture.entered.notified())
        .await
        .unwrap();
    let mut context = crate::mcp::builtin::context::BuiltinInstanceContext::new("/workspace");
    context.closed.insert("workspace".to_string());
    fixture
        .pool
        .set_builtin_instance_context(Arc::new(context))
        .unwrap();
    fixture.release.notify_one();
    assert!(read.await.unwrap().is_err());
    fixture.shutdown().await;
}

#[tokio::test]
async fn ownership_changed_before_response_is_rejected() {
    let fixture = Fixture::new().await;
    let read = fixture.read();
    tokio::time::timeout(Duration::from_secs(1), fixture.entered.notified())
        .await
        .unwrap();
    fixture
        .pool
        .acp_owners
        .write()
        .insert("workspace".to_string(), "other".to_string());
    fixture.release.notify_one();
    assert!(read.await.unwrap().is_err());
    fixture.shutdown().await;
}
