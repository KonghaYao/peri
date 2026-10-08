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
use crate::{TaskScopeAuthority, TASK_SCOPE_META_KEY};

fn scope_meta(token: &str) -> rmcp::model::RequestMetaObject {
    let mut fields = serde_json::Map::new();
    fields.insert(TASK_SCOPE_META_KEY.into(), token.into());
    rmcp::model::RequestMetaObject(rmcp::model::MetaObject(fields))
}

async fn scope_request(
    client: &rmcp::service::RunningService<rmcp::service::RoleClient, ClientConfig>,
    method: &str,
    token: &str,
    cursor: Option<u64>,
) -> Result<serde_json::Value, rmcp::ServiceError> {
    let mut params = serde_json::json!({"_meta": {TASK_SCOPE_META_KEY: token}});
    params["_meta"] = serde_json::to_value(scope_meta(token)).unwrap();
    if let Some(cursor) = cursor {
        params["cursor"] = cursor.into();
    }
    let result = client
        .peer()
        .send_request(rmcp::model::ClientRequest::CustomRequest(
            rmcp::model::CustomRequest::new(method, Some(params)),
        ))
        .await?;
    let rmcp::model::ServerResult::CustomResult(result) = result else {
        panic!("custom result")
    };
    Ok(result.0)
}

async fn scope_epoch_request(
    client: &rmcp::service::RunningService<rmcp::service::RoleClient, ClientConfig>,
    method: &str,
    token: &str,
    epoch: u64,
) -> Result<serde_json::Value, rmcp::ServiceError> {
    let params = serde_json::json!({"_meta": serde_json::to_value(scope_meta(token)).unwrap(), "epoch": epoch});
    let result = client
        .peer()
        .send_request(rmcp::model::ClientRequest::CustomRequest(
            rmcp::model::CustomRequest::new(method, Some(params)),
        ))
        .await?;
    let rmcp::model::ServerResult::CustomResult(result) = result else {
        panic!("custom result")
    };
    Ok(result.0)
}

#[tokio::test]
async fn session_capabilities_work_across_connections_without_execution_fencing() {
    let dir = tempfile::tempdir().expect("workspace");
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy(), false)
        .with_task_scope_authority(TaskScopeAuthority::trusted_connection());
    let token = TaskScopeAuthority::trusted_connection().issue("session-a");
    let (client, server_task) = connect(
        server.clone(),
        ClientCapabilities::builder().enable_tasks().build(),
    )
    .await;
    let snapshot = scope_request(&client, "workspace/taskSnapshot", &token, None)
        .await
        .expect("session capability needs no execution claim");
    assert_eq!(snapshot["tasks"], serde_json::json!([]));
    let error = scope_request(&client, "workspace/taskFence", &token, None)
        .await
        .expect_err("removed method");
    assert!(matches!(error, rmcp::ServiceError::McpError(error)
        if error.code == rmcp::model::ErrorCode::METHOD_NOT_FOUND));
    let mut request = CallToolRequestParams::new("Bash").with_arguments(
        serde_json::json!({"command":"sleep 30", "run_in_background":true})
            .as_object()
            .unwrap()
            .clone(),
    );
    request.meta = Some(scope_meta(&token));
    let CallToolResponse::Task(created) = client
        .peer()
        .call_tool_once(request)
        .await
        .expect("session task")
    else {
        panic!("task handle")
    };
    let task_id = created.task.task_id;
    drop(client);
    server_task.await.expect("first connection closed");

    let (client, server_task) = connect(
        server.clone(),
        ClientCapabilities::builder().enable_tasks().build(),
    )
    .await;
    let next_token = TaskScopeAuthority::trusted_connection().issue("session-a");
    for capability in [&token, &next_token] {
        let snapshot = scope_request(&client, "workspace/taskSnapshot", capability, None)
            .await
            .expect("same session remains accessible");
        assert_eq!(snapshot["tasks"][0]["task"]["taskId"], task_id);
        assert_eq!(snapshot["tasks"][0]["initiatorSessionId"], "session-a");
    }
    let other = TaskScopeAuthority::trusted_connection().issue("session-b");
    let mut cancel = CancelTaskParams::new(&task_id);
    cancel.meta = Some(scope_meta(&other));
    assert!(client.peer().cancel_task(cancel).await.is_err());
    let mut cancel = CancelTaskParams::new(&task_id);
    cancel.meta = Some(scope_meta(&token));
    client
        .peer()
        .cancel_task(cancel)
        .await
        .expect("original session may cancel");
    drop(client);
    server_task.await.expect("second connection closed");
    assert!(server.shutdown_shell_tasks().await.is_some());
}

#[tokio::test]
async fn shared_peer_scopes_discovery_access_and_close() {
    let dir = tempfile::tempdir().expect("workspace");
    let authority = TaskScopeAuthority::new();
    let first = authority.issue("first-session");
    let second = authority.issue("second-session");
    let base = WorkspaceMcpServer::standalone(dir.path().to_string_lossy(), false);
    let server = base.clone().with_task_scope_authority(authority);
    let (client, server_task) =
        connect(base, ClientCapabilities::builder().enable_tasks().build()).await;
    let mut request = CallToolRequestParams::new("Bash").with_arguments(
        serde_json::json!({"command": "sleep 0.1; printf scoped", "run_in_background": true})
            .as_object()
            .unwrap()
            .clone(),
    );
    request.meta = Some(scope_meta(&first));
    let CallToolResponse::Task(created) = client
        .peer()
        .call_tool_once(request.clone())
        .await
        .expect("scoped task")
    else {
        panic!("task handle")
    };
    let id = created.task.task_id;

    let first_snapshot = scope_request(&client, "workspace/taskSnapshot", &first, None)
        .await
        .expect("snapshot");
    assert_eq!(first_snapshot["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(first_snapshot["tasks"][0]["task"]["taskId"], id);
    assert!(first_snapshot["tasks"][0]["summary"]
        .as_str()
        .unwrap()
        .contains("printf scoped"));
    let cursor = first_snapshot["cursor"].as_u64().unwrap();
    let second_snapshot = scope_request(&client, "workspace/taskSnapshot", &second, None)
        .await
        .expect("other snapshot");
    assert!(second_snapshot["tasks"].as_array().unwrap().is_empty());
    let mut other_get = GetTaskParams::new(&id);
    other_get.meta = Some(scope_meta(&second));
    assert!(client.peer().get_task(other_get).await.is_err());
    let mut missing_get = GetTaskParams::new(&id);
    missing_get.meta = None;
    assert!(client.peer().get_task(missing_get).await.is_err());

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let current = scope_request(&client, "workspace/taskSnapshot", &first, None)
                .await
                .expect("task status");
            if current["tasks"][0]["terminalTransitionId"].is_string() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("terminal result");
    let changes = scope_request(&client, "workspace/taskChanges", &first, Some(cursor))
        .await
        .expect("changes");
    assert!(changes["changes"]
        .as_array()
        .unwrap()
        .iter()
        .all(|change| change["initiatorSessionId"] == "first-session"));
    assert!(changes["changes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|change| change["terminalTransitionId"].is_string()));
    let epoch = first_snapshot["epoch"].as_u64().unwrap();
    let closed = scope_epoch_request(&client, "workspace/taskClose", &first, epoch)
        .await
        .expect("close gate");
    assert!(closed["barrierCursor"].as_u64().is_some());
    assert_eq!(closed["epoch"].as_u64(), Some(epoch));
    assert_eq!(closed["resourcesSettled"], true);
    assert!(client.peer().call_tool_once(request).await.is_err());
    let reopened = scope_epoch_request(&client, "workspace/taskOpen", &first, epoch)
        .await
        .expect("reopen settled scope");
    assert_eq!(reopened["epoch"], epoch + 1);
    assert!(
        scope_epoch_request(&client, "workspace/taskClose", &first, epoch)
            .await
            .is_err()
    );
    assert!(
        scope_epoch_request(&client, "workspace/taskOpen", &first, epoch)
            .await
            .is_err()
    );
    let mut reopened_call = CallToolRequestParams::new("Bash").with_arguments(
        serde_json::json!({"command":"printf reopened", "run_in_background":true})
            .as_object()
            .unwrap()
            .clone(),
    );
    reopened_call.meta = Some(scope_meta(&first));
    assert!(matches!(
        client.peer().call_tool_once(reopened_call).await,
        Ok(CallToolResponse::Task(_))
    ));

    let mut other_request = CallToolRequestParams::new("Bash").with_arguments(
        serde_json::json!({"command": "printf other", "run_in_background": true})
            .as_object()
            .unwrap()
            .clone(),
    );
    other_request.meta = Some(scope_meta(&second));
    assert!(matches!(
        client.peer().call_tool_once(other_request).await,
        Ok(CallToolResponse::Task(_))
    ));
    drop(client);
    server_task.await.expect("join server");
    assert!(server.shutdown_shell_tasks().await.is_some());
}

#[tokio::test]
async fn scoped_foreground_promotion_is_discoverable() {
    let dir = tempfile::tempdir().expect("workspace");
    let authority = TaskScopeAuthority::new();
    let token = authority.issue("promoted-session");
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy(), false)
        .with_task_scope_authority(authority);
    let (client, server_task) = connect(
        server.clone(),
        ClientCapabilities::builder().enable_tasks().build(),
    )
    .await;
    let mut request = CallToolRequestParams::new("Bash").with_arguments(
        serde_json::json!({"command":"sleep 0.2; printf promoted", "timeout":50})
            .as_object()
            .unwrap()
            .clone(),
    );
    request.meta = Some(scope_meta(&token));
    let CallToolResponse::Task(created) = client
        .peer()
        .call_tool_once(request)
        .await
        .expect("promote")
    else {
        panic!("task handle")
    };
    let snapshot = scope_request(&client, "workspace/taskSnapshot", &token, None)
        .await
        .expect("snapshot");
    assert_eq!(snapshot["tasks"][0]["task"]["taskId"], created.task.task_id);
    assert_eq!(
        snapshot["tasks"][0]["initiatorSessionId"],
        "promoted-session"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = scope_request(&client, "workspace/taskSnapshot", &token, None)
                .await
                .expect("snapshot");
            if snapshot["tasks"][0]["terminalTransitionId"].is_string() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("promoted terminal");
    drop(client);
    server_task.await.expect("join server");
    assert!(server.shutdown_shell_tasks().await.is_some());
}

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
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy(), false);
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
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy(), false);
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
    let status = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let notification = subscription
                .next()
                .await
                .expect("subscription transport")
                .expect("notification");
            let ServerNotification::TaskStatusNotification(status) = notification else {
                continue;
            };
            if status.params.task.status() == TaskStatus::Cancelled {
                break status;
            }
        }
    })
    .await
    .expect("cancellation notification deadline");
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
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy(), false);
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
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy(), false);
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
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy(), false);
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
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = client
                .peer()
                .get_task(GetTaskParams::new(&task_id))
                .await
                .expect("task status");
            if status.task.status() == TaskStatus::Cancelled {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("cancelled after cleanup");
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
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy(), false);
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

fn current_invocation_meta(scope: Option<&str>, session: &str) -> rmcp::model::RequestMetaObject {
    let mut metadata = scope.map(scope_meta).unwrap_or_default();
    metadata.0 .0.insert(
        "peri.invocation".into(),
        serde_json::json!({
            "initiatorSessionId": session,
            "invocationId": "current-invocation",
            "toolCallId": "model-call"
        }),
    );
    metadata
}

#[tokio::test]
async fn current_invocation_metadata_allows_ordinary_calls_without_task_owner() {
    let directory = tempfile::tempdir().unwrap();
    tokio::fs::write(directory.path().join("current.txt"), "current call result")
        .await
        .unwrap();
    let server = WorkspaceMcpServer::new(directory.path().to_string_lossy().into_owned(), None);
    let (client, server_task) = connect(server, ClientCapabilities::default()).await;
    for _attempt in 0..2 {
        let mut request = CallToolRequestParams::new("Read").with_arguments(
            serde_json::json!({"file_path":"current.txt"})
                .as_object()
                .unwrap()
                .clone(),
        );
        request.meta = Some(current_invocation_meta(None, "session"));
        let CallToolResponse::Complete(result) =
            client.peer().call_tool_once(request).await.unwrap()
        else {
            panic!("ordinary Read must return a result without a task owner")
        };
        assert_ne!(result.is_error, Some(true));
        assert!(serde_json::to_string(&result)
            .unwrap()
            .contains("current call result"));
    }
    client.cancel().await.unwrap();
    server_task.await.unwrap();
}

#[tokio::test]
async fn current_invocation_metadata_rejects_scope_mismatch_before_tool_execution() {
    let directory = tempfile::tempdir().unwrap();
    let authority = TaskScopeAuthority::trusted_connection();
    let token = authority.issue("session");
    let server = WorkspaceMcpServer::standalone(directory.path().to_string_lossy(), false)
        .with_task_scope_authority(authority);
    let (client, server_task) = connect(server.clone(), ClientCapabilities::default()).await;
    let mut request = CallToolRequestParams::new("Write").with_arguments(
        serde_json::json!({"file_path":"unexpected.txt", "content":"must not be written"})
            .as_object()
            .unwrap()
            .clone(),
    );
    request.meta = Some(current_invocation_meta(Some(&token), "other-session"));
    let error = client
        .peer()
        .call_tool_once(request)
        .await
        .expect_err("scope mismatch must be rejected");
    assert!(matches!(error, rmcp::ServiceError::McpError(error)
        if error.code == rmcp::model::ErrorCode::INVALID_PARAMS));
    assert!(!directory.path().join("unexpected.txt").exists());
    client.cancel().await.unwrap();
    server_task.await.unwrap();
    assert_eq!(
        server.shutdown_shell_tasks().await,
        Some(peri_acp_types::tasks::TaskShutdownReport::Complete)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn current_invocation_metadata_keeps_tasks_without_invocation_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let authority = TaskScopeAuthority::trusted_connection();
    let token = authority.issue("session");
    let server = WorkspaceMcpServer::standalone(directory.path().to_string_lossy(), false)
        .with_task_scope_authority(authority);
    let (client, server_task) = connect(
        server.clone(),
        ClientCapabilities::builder().enable_tasks().build(),
    )
    .await;
    let mut task_ids = Vec::new();
    for _attempt in 0..2 {
        let mut request = CallToolRequestParams::new("Bash").with_arguments(
            serde_json::json!({"command":"sleep 30", "run_in_background":true})
                .as_object()
                .unwrap()
                .clone(),
        );
        request.meta = Some(current_invocation_meta(Some(&token), "session"));
        let CallToolResponse::Task(created) = client.peer().call_tool_once(request).await.unwrap()
        else {
            panic!("current background call must return its normal task handle")
        };
        task_ids.push(created.task.task_id);
    }
    assert_ne!(
        task_ids[0], task_ids[1],
        "current metadata must not create replay fencing"
    );
    let snapshot = scope_request(&client, "workspace/taskSnapshot", &token, None)
        .await
        .unwrap();
    assert_eq!(snapshot["tasks"].as_array().unwrap().len(), 2);
    assert!(snapshot.get("invocations").is_none());
    for method in ["workspace/invocationSnapshot", "workspace/taskCapabilities"] {
        let error = scope_request(&client, method, &token, None)
            .await
            .expect_err("removed recovery protocol");
        assert!(matches!(error, rmcp::ServiceError::McpError(error)
            if error.code == rmcp::model::ErrorCode::METHOD_NOT_FOUND));
    }
    for task_id in task_ids {
        let mut cancel = CancelTaskParams::new(&task_id);
        cancel.meta = Some(scope_meta(&token));
        client.peer().cancel_task(cancel).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let mut query = GetTaskParams::new(&task_id);
                query.meta = Some(scope_meta(&token));
                let task = client.peer().get_task(query).await.unwrap();
                if task.task.status() == TaskStatus::Cancelled {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("cancelled task must become queryable terminal state");
    }
    client.cancel().await.unwrap();
    server_task.await.unwrap();
    assert_eq!(
        server.shutdown_shell_tasks().await,
        Some(peri_acp_types::tasks::TaskShutdownReport::Complete)
    );
}

// ── beta flag `full-async-tools`（生产路径：`standalone` 注入有效缺省）──────

/// 生产路径（`standalone`）：schema `default` 与 `call_tool` 判定同源——
/// 省略 `run_in_background` 的调用走 owned 后台任务，显式 `false` 仍走前台。
#[tokio::test]
async fn standalone_default_run_in_background_drives_schema_and_call_path() {
    let dir = tempfile::tempdir().expect("workspace");
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy(), true);
    let (client, server_task) = connect(
        server.clone(),
        ClientCapabilities::builder().enable_tasks().build(),
    )
    .await;

    let tools = client.peer().list_tools(None).await.expect("tools/list");
    let bash = tools
        .tools
        .iter()
        .find(|tool| tool.name.as_ref() == "Bash")
        .expect("Bash 必须声明");
    assert_eq!(
        bash.input_schema["properties"]["run_in_background"]["default"],
        serde_json::json!(true),
        "生产路径 schema default 必须跟随装配期有效缺省"
    );

    // 省略字段：owned 后台任务（client 支持 tasks ⇒ 返回 Task 句柄）。
    let request = CallToolRequestParams::new("Bash").with_arguments(
        serde_json::json!({"command": "sleep 30"})
            .as_object()
            .unwrap()
            .clone(),
    );
    let CallToolResponse::Task(created) = client
        .peer()
        .call_tool_once(request)
        .await
        .expect("省略字段且缺省为 true 时必须走后台")
    else {
        panic!("缺省后台必须返回 task handle")
    };
    assert!(created.task.task_id.starts_with("shell-"));

    // 显式 false：仍走前台（返回 Complete 而非 Task）。
    let request = CallToolRequestParams::new("Bash").with_arguments(
        serde_json::json!({"command": "printf explicit-foreground", "run_in_background": false})
            .as_object()
            .unwrap()
            .clone(),
    );
    let CallToolResponse::Complete(result) = client
        .peer()
        .call_tool_once(request)
        .await
        .expect("显式 false 必须走前台")
    else {
        panic!("显式 false 不得返回 task handle")
    };
    assert_eq!(result.is_error, Some(false));
    let text = result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.clone()))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("explicit-foreground"), "{text}");

    drop(client);
    server_task.await.expect("server closed");
    assert_eq!(
        server.shutdown_shell_tasks().await,
        Some(peri_acp_types::tasks::TaskShutdownReport::Complete)
    );
}
