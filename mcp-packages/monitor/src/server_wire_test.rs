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
    start_with(client, owner, serde_json::json!({ "command": command })).await
}

async fn start_with(
    client: &rmcp::service::RunningService<rmcp::service::RoleClient, ClientConfig>,
    owner: &str,
    arguments: serde_json::Value,
) -> String {
    let mut request = CallToolRequestParams::new("monitor_start")
        .with_arguments(arguments.as_object().cloned().expect("object"));
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
    read_with(client, owner, serde_json::json!({ "task_id": task_id })).await
}

async fn read_events_after(
    client: &rmcp::service::RunningService<rmcp::service::RoleClient, ClientConfig>,
    owner: &str,
    task_id: &str,
    after_seq: serde_json::Value,
) -> serde_json::Value {
    read_with(
        client,
        owner,
        serde_json::json!({ "task_id": task_id, "after_seq": after_seq }),
    )
    .await
}

async fn read_with(
    client: &rmcp::service::RunningService<rmcp::service::RoleClient, ClientConfig>,
    owner: &str,
    arguments: serde_json::Value,
) -> serde_json::Value {
    let mut request = CallToolRequestParams::new("monitor_read")
        .with_arguments(arguments.as_object().cloned().expect("object"));
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

async fn stop_task(
    client: &rmcp::service::RunningService<rmcp::service::RoleClient, ClientConfig>,
    owner: &str,
    task_id: &str,
) -> serde_json::Value {
    let mut request = CallToolRequestParams::new("monitor_stop").with_arguments(
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
        .expect("monitor_stop")
    else {
        panic!("monitor_stop must return a completion");
    };
    assert_ne!(result.is_error, Some(true));
    if let Some(value) = result.structured_content {
        return value;
    }
    let text = result
        .content
        .first()
        .and_then(|block| block.as_text())
        .expect("stop content text");
    serde_json::from_str(&text.text).expect("structured stop result")
}

#[tokio::test]
async fn start_returns_task_read_returns_events_and_owner_isolation_holds() {
    let server = MonitorMcpServer::new(std::env::temp_dir().to_string_lossy().into_owned());
    let (client, server_task) = connect(server).await;

    // `echo a; echo b` 在 bash 与 PowerShell（echo = Write-Output）下都输出两行，
    // 不依赖 Unix 才有的 printf。
    let task_id = start(&client, "session-a", "echo one; echo two").await;
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

/// Wire keys are camelCase: the model-facing contract uses `nextSeq` and
/// `processTerminated`. This pins the serialized names and proves the
/// deserialized cursor round-trips back into `after_seq`.
#[tokio::test]
async fn read_and_stop_wire_keys_use_camel_case() {
    let server = MonitorMcpServer::new(std::env::temp_dir().to_string_lossy().into_owned());
    let (client, server_task) = connect(server).await;
    let task_id = start(&client, "session-a", "echo one; echo two").await;
    terminal_task(&client, "session-a", &task_id).await;

    let page = read_events(&client, "session-a", &task_id).await;
    assert_eq!(page["nextSeq"], serde_json::json!(2));
    assert!(
        page.get("next_seq").is_none(),
        "the wire cursor must be camelCase: {page}"
    );
    assert_eq!(page["truncated"], serde_json::json!(false));

    // Deserialize the wire cursor and pass it back as the next request cursor.
    let rest = read_events_after(&client, "session-a", &task_id, page["nextSeq"].clone()).await;
    assert_eq!(rest["events"], serde_json::json!([]));
    assert_eq!(rest["nextSeq"], serde_json::json!(2));

    let stopped = stop_task(&client, "session-a", &task_id).await;
    assert_eq!(stopped["status"], serde_json::json!("completed"));
    assert_eq!(
        stopped["cleanup"]["requested"],
        serde_json::json!(false),
        "stopping a terminal task must not claim a new cancellation: {stopped}"
    );
    assert_eq!(
        stopped["cleanup"]["processTerminated"],
        serde_json::json!(true),
        "cleanup facts are camelCase: {stopped}"
    );
    assert!(
        stopped["cleanup"].get("process_terminated").is_none(),
        "the snake_case cleanup key must not leak: {stopped}"
    );

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

#[tokio::test]
async fn get_task_meta_projects_camel_case_alert_counters_and_no_event_text() {
    let server = MonitorMcpServer::new(std::env::temp_dir().to_string_lossy().into_owned());
    let (client, server_task) = connect(server).await;
    let task_id = start_with(
        &client,
        "session-a",
        serde_json::json!({
            "command": "echo ok; echo ERROR first; echo ERROR second",
            "alert": "ERROR",
            "label": "build"
        }),
    )
    .await;

    // Poll `tasks/get` until both matches are projected. The first observation
    // of `alertMatched > 0` is what lets the host deliver without a baseline.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let meta = loop {
        let result = get_task(&client, "session-a", &task_id)
            .await
            .expect("tasks/get");
        let projected = result
            .meta
            .as_ref()
            .and_then(|meta| meta.0.get("peri/monitor"))
            .cloned();
        if let Some(meta) = projected {
            if meta["alertMatched"] == serde_json::json!(2) {
                break meta;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "alert counters never reached 2"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    // Shells differ in how many lines one command produces (Windows PowerShell
    // emits an extra event here), so derive the expected `lastAlertSeq` from
    // the events that actually matched the alert instead of pinning a line
    // number. Events are buffered before the alert counters advance, so both
    // matches are readable once `alertMatched` reaches 2.
    let page = read_events(&client, "session-a", &task_id).await;
    let alert_seqs: Vec<u64> = page["events"]
        .as_array()
        .expect("events")
        .iter()
        .filter(|event| {
            event["text"]
                .as_str()
                .is_some_and(|text| text.contains("ERROR"))
        })
        .map(|event| event["seq"].as_u64().expect("alert event seq"))
        .collect();
    assert_eq!(
        alert_seqs.len(),
        2,
        "every alert match must be one matching event: {page}"
    );
    let last_alert_seq = alert_seqs.iter().copied().max().expect("alert events");
    assert_eq!(meta["alertMatched"], serde_json::json!(2));
    assert_eq!(meta["lastAlertSeq"], serde_json::json!(last_alert_seq));
    assert_eq!(meta["label"], serde_json::json!("build"));
    let object = meta.as_object().expect("object");
    assert!(!object.contains_key("pendingWakes"));
    assert!(!object.contains_key("wakeDropped"));
    assert!(
        !meta.to_string().contains("ERROR first"),
        "tasks/get meta must not carry event text"
    );

    // Removing the outbox also removes the two host-only tools.
    let mut removed = CallToolRequestParams::new("monitor_wakes")
        .with_arguments(serde_json::json!({}).as_object().cloned().expect("object"));
    removed.meta = Some(owner_meta("session-a"));
    let error = client
        .peer()
        .call_tool_once(removed)
        .await
        .expect_err("monitor_wakes must no longer be callable");
    assert!(matches!(error, rmcp::ServiceError::McpError(error)
        if error.code == rmcp::model::ErrorCode::INVALID_PARAMS));

    // Owner scoping still hides the task from foreign sessions.
    let foreign = get_task(&client, "session-b", &task_id)
        .await
        .expect_err("foreign tasks/get must be rejected");
    assert!(matches!(foreign, rmcp::ServiceError::McpError(error)
        if error.code == rmcp::model::ErrorCode::INVALID_PARAMS));

    // Terminal projection retains the final counters (the host caches the
    // terminal meta instead of re-fetching it).
    let terminal = terminal_task(&client, "session-a", &task_id).await;
    assert_eq!(terminal.task.status(), TaskStatus::Completed);
    let terminal_meta = terminal
        .meta
        .and_then(|meta| meta.0.get("peri/monitor").cloned())
        .expect("terminal meta");
    assert_eq!(terminal_meta["alertMatched"], serde_json::json!(2));
    assert_eq!(
        terminal_meta["lastAlertSeq"],
        serde_json::json!(last_alert_seq)
    );

    drop(client);
    server_task.await.expect("join server");
}
