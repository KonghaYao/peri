//! A standalone Workspace owner keeps its execution floor across Agent connections.

#![cfg(unix)]

use std::{net::TcpListener, process::Stdio, time::Duration};

use peri_mcp_workspace::{TaskScopeAuthority, TASK_SCOPE_META_KEY};
use rmcp::{
    model::{
        CallToolRequestParams, ClientCapabilities, ClientConfig, ClientRequest, CustomRequest,
        ProtocolVersion,
    },
    service::{RoleClient, RunningService},
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport,
    },
    ClientLifecycleMode,
};

async fn connect(url: &str) -> RunningService<RoleClient, ClientConfig> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let transport = StreamableHttpClientTransport::with_client(
                reqwest::Client::new(),
                StreamableHttpClientTransportConfig::with_uri(url),
            );
            let mut config = ClientConfig::default();
            config.capabilities = ClientCapabilities::builder().enable_tasks().build();
            if let Ok(client) = rmcp::serve_client_with_lifecycle(
                config,
                transport,
                ClientLifecycleMode::Auto {
                    preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                    legacy_version: None,
                },
            )
            .await
            {
                return client;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("standalone Workspace MCP readiness")
}

async fn custom(
    client: &RunningService<RoleClient, ClientConfig>,
    method: &str,
    token: &str,
) -> Result<(), rmcp::ServiceError> {
    client
        .peer()
        .send_request(ClientRequest::CustomRequest(CustomRequest::new(
            method,
            Some(serde_json::json!({"_meta": {TASK_SCOPE_META_KEY: token}})),
        )))
        .await
        .map(|_| ())
}

#[tokio::test]
async fn standalone_owner_rejects_late_old_agent_over_new_http_connection() {
    let dir = tempfile::tempdir().unwrap();
    let bind = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = bind.local_addr().unwrap();
    drop(bind);
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_peri-mcp-workspace"))
        .args([
            "--http",
            "--workspace",
            dir.path().to_str().unwrap(),
            "--bind",
            &address.to_string(),
            "--no-skill-roots",
            "--no-agent-roots",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let url = format!("http://{address}/mcp");
    let old_issuer = TaskScopeAuthority::trusted_connection();
    let old = old_issuer.issue_execution("session-a", 1, "agent-one");
    let first = connect(&url).await;
    custom(&first, "workspace/taskFence", &old)
        .await
        .expect("first Agent fence");
    first.cancel().await.expect("old Agent disconnect");

    let new_issuer = TaskScopeAuthority::trusted_connection();
    let new = new_issuer.issue_execution("session-a", 2, "agent-two");
    let second = connect(&url).await;
    custom(&second, "workspace/taskFence", &new)
        .await
        .expect("takeover fence");
    assert!(custom(&second, "workspace/taskFence", &old).await.is_err());
    assert!(custom(&second, "workspace/taskSnapshot", &old)
        .await
        .is_err());
    let mut late = CallToolRequestParams::new("Bash").with_arguments(
        serde_json::json!({"command":"printf should-not-run", "run_in_background":true})
            .as_object()
            .unwrap()
            .clone(),
    );
    late.meta = Some(rmcp::model::RequestMetaObject(rmcp::model::MetaObject(
        serde_json::json!({TASK_SCOPE_META_KEY: old})
            .as_object()
            .unwrap()
            .clone(),
    )));
    assert!(second.peer().call_tool_once(late).await.is_err());
    custom(&second, "workspace/taskSnapshot", &new)
        .await
        .expect("new Agent snapshot");
    second.cancel().await.expect("new Agent disconnect");
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    let replacement = tokio::process::Command::new(env!("CARGO_BIN_EXE_peri-mcp-workspace"))
        .args([
            "--http",
            "--workspace",
            dir.path().to_str().unwrap(),
            "--bind",
            &address.to_string(),
            "--no-skill-roots",
            "--no-agent-roots",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        !replacement.status.success(),
        "crashed owner must not restart with an empty registry"
    );
    assert!(dir.path().join(".peri/workspace-owner-unclean").exists());
    std::fs::rename(
        dir.path().join(".peri/workspace-owner-unclean"),
        dir.path().join(".peri/previous.workspace-owner-unclean"),
    )
    .unwrap();
    let legacy_guard_restart =
        tokio::process::Command::new(env!("CARGO_BIN_EXE_peri-mcp-workspace"))
            .args([
                "--http",
                "--workspace",
                dir.path().to_str().unwrap(),
                "--bind",
                &address.to_string(),
                "--no-skill-roots",
                "--no-agent-roots",
            ])
            .output()
            .await
            .unwrap();
    assert!(!legacy_guard_restart.status.success());
}
