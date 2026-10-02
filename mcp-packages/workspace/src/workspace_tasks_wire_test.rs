use std::time::Duration;

use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CancelTaskParams, ClientCapabilities,
        ClientConfig, GetTaskParams, ProtocolVersion, ServerNotification, SubscriptionFilter,
        TaskStatus,
    },
    ClientLifecycleMode,
};

use super::WorkspaceMcpServer;

async fn connect(
    server: WorkspaceMcpServer,
    capabilities: ClientCapabilities,
) -> (
    rmcp::service::RunningService<rmcp::service::RoleClient, ClientConfig>,
    tokio::task::JoinHandle<()>,
) {
    let (client_io, server_io) = tokio::io::duplex(8192);
    let server_task = tokio::spawn(async move {
        rmcp::serve_server(server, tokio::io::split(server_io))
            .await
            .expect("serve Workspace MCP")
            .waiting()
            .await
            .expect("server clean close");
    });
    let mut client_info = ClientConfig::default();
    client_info.capabilities = capabilities;
    let client = rmcp::serve_client_with_lifecycle(
        client_info,
        tokio::io::split(client_io),
        ClientLifecycleMode::Auto {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            legacy_version: None,
        },
    )
    .await
    .expect("MCP handshake");
    (client, server_task)
}

#[tokio::test]
async fn task_completion_is_delivered_to_its_subscription() {
    let dir = tempfile::tempdir().expect("workspace");
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy());
    let (client, server_task) = connect(
        server.clone(),
        ClientCapabilities::builder().enable_tasks().build(),
    )
    .await;
    let request = CallToolRequestParams::new("Bash").with_arguments(
        serde_json::json!({"command": "sleep 0.2; printf subscribed", "run_in_background": true})
            .as_object()
            .expect("object")
            .clone(),
    );
    let CallToolResponse::Task(created) = client
        .peer()
        .call_tool_once(request)
        .await
        .expect("background task")
    else {
        panic!("expected task handle")
    };
    let mut subscription = client
        .listen(
            SubscriptionFilter::builder()
                .task_id(&created.task.task_id)
                .build(),
        )
        .await
        .expect("subscribe to owned task");
    assert_eq!(
        subscription.acknowledged().task_ids.as_deref(),
        Some(&[created.task.task_id.clone()][..])
    );
    let notification = tokio::time::timeout(Duration::from_secs(5), subscription.next())
        .await
        .expect("task notification deadline")
        .expect("subscription transport")
        .expect("notification");
    let ServerNotification::TaskStatusNotification(status) = notification else {
        panic!("expected task status notification")
    };
    assert_eq!(status.params.task.task.task_id, created.task.task_id);
    assert_eq!(status.params.task.status(), TaskStatus::Completed);
    drop(subscription);
    client.cancel().await.expect("close client");
    server_task.await.expect("join server");
    assert!(server.shutdown_shell_tasks().await.is_some());
}

#[tokio::test]
async fn task_cancellation_is_delivered_to_its_subscription() {
    let dir = tempfile::tempdir().expect("workspace");
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy());
    let (client, server_task) = connect(
        server.clone(),
        ClientCapabilities::builder().enable_tasks().build(),
    )
    .await;
    let request = CallToolRequestParams::new("Bash").with_arguments(
        serde_json::json!({"command": "sleep 30", "run_in_background": true})
            .as_object()
            .expect("object")
            .clone(),
    );
    let CallToolResponse::Task(created) = client
        .peer()
        .call_tool_once(request)
        .await
        .expect("background task")
    else {
        panic!("expected task handle")
    };
    let task_id = created.task.task_id;
    assert!(client
        .listen(
            SubscriptionFilter::builder()
                .task_id("unknown-task")
                .build()
        )
        .await
        .is_err());
    let mut subscription = client
        .listen(SubscriptionFilter::builder().task_id(&task_id).build())
        .await
        .expect("subscribe to task");
    client
        .peer()
        .cancel_task(CancelTaskParams::new(&task_id))
        .await
        .expect("cancel task");
    let notification = tokio::time::timeout(Duration::from_secs(5), subscription.next())
        .await
        .expect("cancellation notification deadline")
        .expect("subscription transport")
        .expect("notification");
    let ServerNotification::TaskStatusNotification(status) = notification else {
        panic!("expected task status notification")
    };
    assert_eq!(status.params.task.task.task_id, task_id);
    assert_eq!(status.params.task.status(), TaskStatus::Cancelled);
    drop(subscription);
    client.cancel().await.expect("close client");
    server_task.await.expect("join server");
    assert!(server.shutdown_shell_tasks().await.is_some());
}

#[tokio::test]
async fn task_result_remains_queryable_across_connections() {
    let dir = tempfile::tempdir().expect("workspace");
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy());
    let capabilities = ClientCapabilities::builder().enable_tasks().build();
    let (first, first_server) = connect(server.clone(), capabilities.clone()).await;
    let request = CallToolRequestParams::new("Bash").with_arguments(
        serde_json::json!({"command": "sleep 0.4; printf task-finished", "run_in_background": true})
            .as_object()
            .expect("object")
            .clone(),
    );
    let CallToolResponse::Task(created) = first
        .peer()
        .call_tool_once(request)
        .await
        .expect("background tool call")
    else {
        panic!("Tasks-capable client must receive CreateTaskResult")
    };
    let task_id = created.task.task_id;
    assert_eq!(
        first
            .peer()
            .get_task(GetTaskParams::new(&task_id))
            .await
            .expect("task is queryable immediately")
            .task
            .task
            .task_id,
        task_id
    );

    let (second, second_server) = connect(server.clone(), capabilities).await;
    first.cancel().await.expect("close first connection");
    first_server.await.expect("join first server");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state = second
                .peer()
                .get_task(GetTaskParams::new(&task_id))
                .await
                .expect("result survives first connection");
            if state.task.status() == TaskStatus::Completed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("task completion deadline");
    second.cancel().await.expect("close second connection");
    second_server.await.expect("join second server");
    assert_eq!(
        server.shutdown_shell_tasks().await,
        Some(peri_acp_types::tasks::TaskShutdownReport::Complete)
    );
}

#[tokio::test]
async fn task_query_requires_client_capability() {
    let dir = tempfile::tempdir().expect("workspace");
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy());
    let (client, server_task) = connect(server.clone(), ClientCapabilities::default()).await;
    let error = client
        .peer()
        .get_task(GetTaskParams::new("shell-any"))
        .await
        .expect_err("task query needs capability");
    assert!(format!("{error}").contains("capability"), "{error}");
    client.cancel().await.expect("close client");
    server_task.await.expect("join server");
    assert_eq!(
        server.shutdown_shell_tasks().await,
        Some(peri_acp_types::tasks::TaskShutdownReport::Complete)
    );
}

#[tokio::test]
async fn task_cancel_wire_stops_the_owned_shell() {
    let dir = tempfile::tempdir().expect("workspace");
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy());
    let capabilities = ClientCapabilities::builder().enable_tasks().build();
    let (client, server_task) = connect(server.clone(), capabilities).await;
    let request = CallToolRequestParams::new("Bash").with_arguments(
        serde_json::json!({"command": "sleep 30", "run_in_background": true})
            .as_object()
            .expect("object")
            .clone(),
    );
    let CallToolResponse::Task(created) = client
        .peer()
        .call_tool_once(request)
        .await
        .expect("background task")
    else {
        panic!("expected task handle")
    };
    let task_id = created.task.task_id;
    client
        .peer()
        .cancel_task(CancelTaskParams::new(&task_id))
        .await
        .expect("cancel ack");
    assert_eq!(
        client
            .peer()
            .get_task(GetTaskParams::new(&task_id))
            .await
            .expect("cancelled task")
            .task
            .status(),
        TaskStatus::Cancelled
    );
    client.cancel().await.expect("close client");
    server_task.await.expect("join server");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), server.shutdown_shell_tasks())
            .await
            .expect("process cleanup deadline"),
        Some(peri_acp_types::tasks::TaskShutdownReport::Complete)
    );
}

#[tokio::test]
async fn foreground_timeout_promotion_returns_queryable_task() {
    let dir = tempfile::tempdir().expect("workspace");
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy());
    let capabilities = ClientCapabilities::builder().enable_tasks().build();
    let (client, server_task) = connect(server.clone(), capabilities).await;
    let request = CallToolRequestParams::new("Bash").with_arguments(
        serde_json::json!({"command": "sleep 0.3; printf promoted", "timeout": 50})
            .as_object()
            .expect("object")
            .clone(),
    );
    let CallToolResponse::Task(created) = client
        .peer()
        .call_tool_once(request)
        .await
        .expect("promoted tool call")
    else {
        panic!("foreground timeout must become a queryable task")
    };
    let task_id = created.task.task_id;
    let initial = client
        .peer()
        .get_task(GetTaskParams::new(&task_id))
        .await
        .expect("promoted task immediately queryable");
    assert_eq!(initial.task.task.task_id, task_id);
    let terminal = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = client
                .peer()
                .get_task(GetTaskParams::new(&task_id))
                .await
                .expect("task status");
            if snapshot.task.status().is_terminal() {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("promoted task deadline");
    assert_eq!(terminal.task.status(), TaskStatus::Completed);
    client.cancel().await.expect("close client");
    server_task.await.expect("join server");
    assert_eq!(
        server.shutdown_shell_tasks().await,
        Some(peri_acp_types::tasks::TaskShutdownReport::Complete)
    );
}
