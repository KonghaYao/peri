use super::*;
use crate::mcp::client::output_store::tests::{assert_remote_readback, large_output, Wire};

#[tokio::test]
async fn recovered_resource_output_is_stored_over_workspace_wire() {
    let remote = tempfile::tempdir().unwrap();
    let wire = Wire::connect(peri_mcp_workspace::WorkspaceMcpServer::new(
        remote.path().to_str().unwrap(),
        None,
    ))
    .await;
    let pool = Arc::new(McpClientPool::new_empty());
    wire.install(&pool, "workspace", true);
    let tool = McpResourceTool::new(pool.clone(), Arc::new(McpSkillRegistry::new()))
        .with_session_id("session");
    let output = tool
        .format_recovered_content(&large_output(), Some("text/markdown"))
        .await;
    let saved_path = assert_remote_readback(
        wire.client.peer(),
        &output,
        &format!("[text/text/markdown]\n{}", large_output()),
    )
    .await;
    std::fs::remove_file(saved_path).unwrap();
    pool.begin_shutdown();
    let output = tool.format_recovered_content(&large_output(), None).await;
    assert!(output.contains("Full output NOT saved"));
    assert!(!output.contains("resource_uri="));
    pool.clients.write().clear();
    wire.close().await;
}

#[tokio::test]
async fn opaque_resource_read_rejects_closed_pool() {
    let remote = tempfile::tempdir().unwrap();
    let wire = Wire::connect(peri_mcp_workspace::WorkspaceMcpServer::new(
        remote.path().to_str().unwrap(),
        None,
    ))
    .await;
    let pool = Arc::new(McpClientPool::new_empty());
    wire.install(&pool, "workspace", true);
    let stored = pool
        .store_output(Some("session"), "remote content")
        .await
        .unwrap();
    let tool = McpResourceTool::new(pool.clone(), Arc::new(McpSkillRegistry::new()))
        .with_session_id("session");
    pool.begin_shutdown();
    let result = tool
        .invoke(
            serde_json::json!({"server_name": "workspace", "uri": stored.uri}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err());
    std::fs::remove_file(stored.path).unwrap();
    pool.clients.write().clear();
    wire.close().await;
}
