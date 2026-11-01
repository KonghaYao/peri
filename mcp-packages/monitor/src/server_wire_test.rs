//! Wire-level contract: `monitor_start` returns a standard MCP Tasks handle,
//! `tasks/get`/`tasks/cancel` carry the session owner, and event reads stay
//! owner-scoped.

use std::time::Duration;

use peri_mcp_common::task_owner::TASK_OWNER_META_KEY;
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CancelTaskParams, ClientCapabilities,
        ClientConfig, ClientRequest, GetTaskParams, ProtocolVersion, RequestMetaObject,
        ServerResult, TaskStatus,
    },
    ClientLifecycleMode,
};

use super::MonitorMcpServer;

fn owner_meta(owner: &str) -> RequestMetaObject {
    let mut fields = serde_json::Map::new();
    fields.insert(TASK_OWNER_META_KEY.into(), owner.into());
    RequestMetaObject(rmcp::model::MetaObject(fields))
}

async fn connect(
    server: MonitorMcpServer,
) -> (
    rmcp::service::RunningService<rmcp::service::RoleClient, ClientConfig>,
    tokio::task::JoinHandle<()>,
) {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let server_task = tokio::spawn(async move {
        rmcp::serve_server(server, tokio::io::split(server_io))
            .await
            .expect("serve Monitor MCP")
            .waiting()
            .await
            .expect("server clean close");
    });
    let mut client_info = ClientConfig::default();
    client_info.capabilities = ClientCapabilities::builder().enable_tasks().build();
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

async fn start(
    client: &rmcp::service::RunningService<rmcp::service::RoleClient, ClientConfig>,
    owner: &str,
    command: &str,
) -> String {
    let mut request = CallToolRequestParams::new("monitor_start").with_arguments(
        serde_json::json!({ "command": command })
            .as_object()
            .expect("object")
            .clone(),
    );
    request.meta = Some(owner_meta(owner));
    let CallToolResponse::Task(created) = client
        .peer()
        .call_tool_once(request)
        .await
        .expect("monitor_start returns a task")
    else {
        panic!("monitor_start must return an MCP task");
    };
    created.task.task_id
}

async fn get_task(
    client: &rmcp::service::RunningService<rmcp::service::RoleClient, ClientConfig>,
    owner: &str,
    task_id: &str,
) -> Result<rmcp::model::GetTaskResult, rmcp::ServiceError> {
    let mut params = GetTaskParams::new(task_id);
    params.meta = Some(owner_meta(owner));
    client.peer().get_task(params).await
}

async fn terminal_task(
    client: &rmcp::service::RunningService<rmcp::service::RoleClient, ClientConfig>,
    owner: &str,
    task_id: &str,
) -> rmcp::model::GetTaskResult {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let task = get_task(client, owner, task_id).await.expect("tasks/get");
        if task.task.status().is_terminal() {
            return task;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "task never reached a terminal state"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn read_events(
    client: &rmcp::service::RunningService<rmcp::service::RoleClient, ClientConfig>,
    owner: &str,
    task_id: &str,
) -> serde_json::Value {
    let mut request = CallToolRequestParams::new("monitor_read").with_arguments(
        serde_json::json!({ "task_id": task_id })
            .as_object()
            .expect("object")
            .clone(),
    );
    request.meta = Some(owner_meta(owner));
    let CallToolResponse::Complete(result) = client
        .peer()
        .call_tool_once(request)
        .await
        .expect("monitor_read")
    else {
        panic!("monitor_read must return a completion");
    };
    assert_ne!(result.is_error, Some(true));
    if let Some(value) = result.structured_content {
        return value;
    }
    let text = result
        .content
        .first()
        .and_then(|block| block.as_text())
        .expect("read content text");
    serde_json::from_str(&text.text).expect("structured read result")
}

#[tokio::test]
async fn start_returns_task_read_returns_events_and_owner_isolation_holds() {
    let server = MonitorMcpServer::new(std::env::temp_dir().to_string_lossy().into_owned());
    let (client, server_task) = connect(server).await;

    let task_id = start(&client, "session-a", "printf 'one\\ntwo\\n'").await;
    let task = terminal_task(&client, "session-a", &task_id).await;
    assert_eq!(task.task.status(), TaskStatus::Completed);

    let page = read_events(&client, "session-a", &task_id).await;
    let texts: Vec<&str> = page["events"]
        .as_array()
        .expect("events")
        .iter()
        .filter_map(|event| event["text"].as_str())
        .collect();
    assert_eq!(texts, vec!["one", "two"]);
    assert_eq!(page["nextSeq"], serde_json::json!(2));
    assert_eq!(page["status"], serde_json::json!("completed"));

    // Another session cannot see the task or its events.
    let error = get_task(&client, "session-b", &task_id)
        .await
        .expect_err("foreign owner must be rejected");
    assert!(matches!(error, rmcp::ServiceError::McpError(error)
        if error.code == rmcp::model::ErrorCode::INVALID_PARAMS));

    let mut foreign_read = CallToolRequestParams::new("monitor_read").with_arguments(
        serde_json::json!({ "task_id": task_id })
            .as_object()
            .expect("object")
            .clone(),
    );
    foreign_read.meta = Some(owner_meta("session-b"));
    let CallToolResponse::Complete(result) = client
        .peer()
        .call_tool_once(foreign_read)
        .await
        .expect("read call")
    else {
        panic!("read completion")
    };
    assert_eq!(result.is_error, Some(true));

    drop(client);
    server_task.await.expect("join server");
}

#[tokio::test]
async fn tasks_cancel_is_acknowledged_and_owner_checked() {
    let server = MonitorMcpServer::new(std::env::temp_dir().to_string_lossy().into_owned());
    let (client, server_task) = connect(server).await;
    let task_id = start(&client, "session-a", "sleep 30").await;

    let mut foreign = CancelTaskParams::new(&task_id);
    foreign.meta = Some(owner_meta("session-b"));
    let error = client
        .peer()
        .send_request(ClientRequest::CancelTaskRequest(
            rmcp::model::CancelTaskRequest::new(foreign),
        ))
        .await
        .expect_err("foreign cancel must be rejected");
    assert!(matches!(error, rmcp::ServiceError::McpError(error)
        if error.code == rmcp::model::ErrorCode::INVALID_PARAMS));

    let mut own = CancelTaskParams::new(&task_id);
    own.meta = Some(owner_meta("session-a"));
    let response = client
        .peer()
        .send_request(ClientRequest::CancelTaskRequest(
            rmcp::model::CancelTaskRequest::new(own),
        ))
        .await
        .expect("cancel ack");
    assert!(matches!(
        response,
        ServerResult::TaskAckResult(_) | ServerResult::EmptyResult(_)
    ));

    let task = terminal_task(&client, "session-a", &task_id).await;
    assert_eq!(task.task.status(), TaskStatus::Cancelled);

    drop(client);
    server_task.await.expect("join server");
}
