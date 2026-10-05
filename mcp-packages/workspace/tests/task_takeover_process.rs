//! A standalone Workspace keeps session-scoped tasks across Agent connections.

#![cfg(unix)]

use std::{net::TcpListener, process::Stdio, time::Duration};

use peri_mcp_workspace::{TaskScopeAuthority, TASK_SCOPE_META_KEY};
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CancelTaskParams, ClientCapabilities,
        ClientConfig, ClientRequest, CustomRequest, GetTaskParams, ProtocolVersion, TaskStatus,
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
async fn standalone_tasks_remain_accessible_over_new_http_connections() {
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
    let old = old_issuer.issue("session-a");
    let first = connect(&url).await;
    custom(&first, "workspace/taskSnapshot", &old)
        .await
        .expect("first session snapshot");
    let meta = rmcp::model::RequestMetaObject(rmcp::model::MetaObject(
        serde_json::json!({TASK_SCOPE_META_KEY: old})
            .as_object()
            .unwrap()
            .clone(),
    ));
    let mut request = CallToolRequestParams::new("Bash").with_arguments(
        serde_json::json!({"command":"sleep 30", "run_in_background":true})
            .as_object()
            .unwrap()
            .clone(),
    );
    request.meta = Some(meta.clone());
    let CallToolResponse::Task(created) = first.peer().call_tool_once(request).await.unwrap()
    else {
        panic!("task handle")
    };
    let task_id = created.task.task_id;
    first.cancel().await.expect("old Agent disconnect");

    let new_issuer = TaskScopeAuthority::trusted_connection();
    let new = new_issuer.issue("session-a");
    let second = connect(&url).await;
    custom(&second, "workspace/taskSnapshot", &old)
        .await
        .expect("original session capability remains valid");
    let mut late = CallToolRequestParams::new("Bash").with_arguments(
        serde_json::json!({"command":"printf connection-task", "run_in_background":true})
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
    second
        .peer()
        .call_tool_once(late)
        .await
        .expect("session tool call needs no execution claim");
    custom(&second, "workspace/taskSnapshot", &new)
        .await
        .expect("new Agent snapshot");
    let mut get = GetTaskParams::new(&task_id);
    get.meta = Some(meta.clone());
    assert_eq!(
        second
            .peer()
            .get_task(get.clone())
            .await
            .unwrap()
            .task
            .status(),
        TaskStatus::Working
    );
    let mut cancel = CancelTaskParams::new(&task_id);
    cancel.meta = Some(meta);
    second.peer().cancel_task(cancel).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let result = second.peer().get_task(get.clone()).await.unwrap();
            if result.task.status().is_terminal() {
                assert_eq!(result.task.status(), TaskStatus::Cancelled);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("cancelled status follows shell cleanup");
    second.cancel().await.expect("new Agent disconnect");
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    assert!(!dir.path().join(".peri/workspace-owner-unclean").exists());
}

#[tokio::test]
async fn old_owner_markers_do_not_block_restart_and_are_not_modified() {
    let dir = tempfile::tempdir().unwrap();
    let guard_dir = dir.path().join(".peri");
    std::fs::create_dir_all(&guard_dir).unwrap();
    let markers = [
        guard_dir.join("workspace-owner-unclean"),
        guard_dir.join("previous.workspace-owner-unclean"),
    ];
    for marker in &markers {
        std::fs::write(marker, "existing-user-evidence").unwrap();
    }
    let bind = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = bind.local_addr().unwrap();
    drop(bind);
    let url = format!("http://{address}/mcp");
    let token = TaskScopeAuthority::trusted_connection().issue("session-a");
    for attempt in 0..2 {
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_peri-mcp-workspace"))
            .args([
                "--http",
                "--workspace",
                dir.path().to_str().unwrap(),
                "--bind",
                &address.to_string(),
                "--no-skill-roots",
                "--no-agent-roots",
                "--disable-bundled",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let client = connect(&url).await;
        custom(&client, "workspace/taskSnapshot", &token)
            .await
            .expect("old markers do not block MCP readiness");
        client.cancel().await.unwrap();
        if attempt == 0 {
            child.kill().await.unwrap();
            child.wait().await.unwrap();
        } else {
            let signal = tokio::process::Command::new("kill")
                .args(["-TERM", &child.id().unwrap().to_string()])
                .status()
                .await
                .unwrap();
            assert!(signal.success());
            let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await
                .expect("graceful shutdown")
                .unwrap();
            assert!(status.success());
        }
        for marker in &markers {
            assert_eq!(
                std::fs::read_to_string(marker).unwrap(),
                "existing-user-evidence"
            );
        }
    }
}
