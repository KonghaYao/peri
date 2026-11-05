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
use crate::TASK_OWNER_META_KEY;

fn owner_meta(owner: &str) -> rmcp::model::RequestMetaObject {
    let mut fields = serde_json::Map::new();
    fields.insert(TASK_OWNER_META_KEY.into(), owner.into());
    rmcp::model::RequestMetaObject(rmcp::model::MetaObject(fields))
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

/// 通用 MCP 客户端不发送任何 `peri/*` 元数据时，基础工具照常工作：
/// 没有 owner 就没有 session 隔离，但能力面不因缺少 Peri 元数据而缩小。
#[tokio::test]
async fn generic_client_without_peri_metadata_calls_write_and_bash() {
    let dir = tempfile::tempdir().expect("workspace");
    let target = dir.path().join("plain.txt");
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy(), false);
    let (client, server_task) = connect(server.clone(), ClientCapabilities::default()).await;

    let write = CallToolRequestParams::new("Write").with_arguments(
        serde_json::json!({
            "file_path": target.to_string_lossy(),
            "content": "plain",
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    client
        .peer()
        .call_tool_once(write)
        .await
        .expect("generic client call without task metadata");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "plain");

    let bash = CallToolRequestParams::new("Bash").with_arguments(
        serde_json::json!({"command": "printf generic-bash"})
            .as_object()
            .unwrap()
            .clone(),
    );
    let CallToolResponse::Complete(result) = client
        .peer()
        .call_tool_once(bash)
        .await
        .expect("generic Bash call without task metadata")
    else {
        panic!("foreground Bash must return a completion")
    };
    let text = result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.clone()))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("generic-bash"), "{text}");

    let error = client
        .peer()
        .send_request(rmcp::model::ClientRequest::CustomRequest(
            rmcp::model::CustomRequest::new("workspace/taskSnapshot", Some(serde_json::json!({}))),
        ))
        .await
        .expect_err("removed custom lifecycle must be method-not-found");
    assert!(matches!(error, rmcp::ServiceError::McpError(error)
        if error.code == rmcp::model::ErrorCode::METHOD_NOT_FOUND));

    drop(client);
    server_task.await.expect("join server");
    assert!(server.shutdown_shell_tasks().await.is_some());
}

/// 两个 owner 的后台任务互不可见、不可取消；owner A 自己的任务照常可用。
#[tokio::test]
async fn task_owner_isolation_hides_and_protects_other_sessions_tasks() {
    let dir = tempfile::tempdir().expect("workspace");
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy(), false);
    let (client, server_task) = connect(
        server.clone(),
        ClientCapabilities::builder().enable_tasks().build(),
    )
    .await;
    let mut request = CallToolRequestParams::new("Bash").with_arguments(
        serde_json::json!({"command": "sleep 30", "run_in_background": true})
            .as_object()
            .unwrap()
            .clone(),
    );
    request.meta = Some(owner_meta("session-a"));
    let CallToolResponse::Task(created) = client
        .peer()
        .call_tool_once(request)
        .await
        .expect("owned background task")
    else {
        panic!("task handle")
    };
    let task_id = created.task.task_id;

    let mut owner_get = GetTaskParams::new(&task_id);
    owner_get.meta = Some(owner_meta("session-a"));
    client
        .peer()
        .get_task(owner_get)
        .await
        .expect("owner reads its own task");

    let mut other_get = GetTaskParams::new(&task_id);
    other_get.meta = Some(owner_meta("session-b"));
    assert!(
        client.peer().get_task(other_get).await.is_err(),
        "owner B must not read owner A's task"
    );

    let mut other_cancel = CancelTaskParams::new(&task_id);
    other_cancel.meta = Some(owner_meta("session-b"));
    assert!(
        client.peer().cancel_task(other_cancel).await.is_err(),
        "owner B must not cancel owner A's task"
    );

    let mut owner_cancel = CancelTaskParams::new(&task_id);
    owner_cancel.meta = Some(owner_meta("session-a"));
    client
        .peer()
        .cancel_task(owner_cancel)
        .await
        .expect("owner cancels its task");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let mut query = GetTaskParams::new(&task_id);
            query.meta = Some(owner_meta("session-a"));
            if client
                .peer()
                .get_task(query)
                .await
                .expect("task status")
                .task
                .status()
                == TaskStatus::Cancelled
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("cancelled after cleanup");

    drop(client);
    server_task.await.expect("join server");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), server.shutdown_shell_tasks())
            .await
            .expect("process cleanup deadline"),
        Some(peri_acp_types::tasks::TaskShutdownReport::Complete)
    );
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

/// [回归测试] 后台 shell 被并发上限拒绝时，wire 上的错误必须保留上限事实，而不是
/// 折叠成不透明的 `shell task launch failed`；命令文本不进入错误文本，上限不变。
#[tokio::test]
async fn concurrent_limit_rejection_keeps_limit_facts_on_the_wire() {
    use peri_agent::agent::async_tasks::BackgroundTaskRegistry;

    let dir = tempfile::tempdir().expect("workspace");
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy(), false);
    let capabilities = ClientCapabilities::builder().enable_tasks().build();
    let (client, server_task) = connect(server.clone(), capabilities).await;
    let mut task_ids = Vec::new();
    for index in 0..BackgroundTaskRegistry::SHELL_LIMIT {
        let request = CallToolRequestParams::new("Bash").with_arguments(
            serde_json::json!({"command": format!("sleep 30 # {index}"), "run_in_background": true})
                .as_object()
                .expect("object")
                .clone(),
        );
        let CallToolResponse::Task(created) = client
            .peer()
            .call_tool_once(request)
            .await
            .expect("capacity shell")
        else {
            panic!("expected task handle")
        };
        task_ids.push(created.task.task_id);
    }
    let request = CallToolRequestParams::new("Bash").with_arguments(
        serde_json::json!({"command": "sleep 30 # over-limit", "run_in_background": true})
            .as_object()
            .expect("object")
            .clone(),
    );
    let error = client
        .peer()
        .call_tool_once(request)
        .await
        .expect_err("over-limit background shell must be rejected");
    let text = error.to_string();
    assert!(
        text.contains(&format!(
            "shell {}/{}",
            BackgroundTaskRegistry::SHELL_LIMIT,
            BackgroundTaskRegistry::SHELL_LIMIT
        )),
        "wire error must keep limit facts: {text}"
    );
    assert!(
        !text.contains("sleep 30"),
        "wire error must not carry the command: {text}"
    );

    for task_id in &task_ids {
        client
            .peer()
            .cancel_task(CancelTaskParams::new(task_id))
            .await
            .expect("cancel ack");
    }
    drop(client);
    server_task.await.expect("server closed");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), server.shutdown_shell_tasks())
            .await
            .expect("process cleanup deadline"),
        Some(peri_acp_types::tasks::TaskShutdownReport::Complete)
    );
}

/// [回归测试] owned 后台路径曾把整数值浮点 timeout 当作未设置（无限运行）。
/// `timeout: 100.0` 必须在 wire 上按 100ms 生效，命令来不及写出 sentinel。
#[tokio::test]
async fn background_integral_float_timeout_is_normalized_on_the_wire() {
    let dir = tempfile::tempdir().expect("workspace");
    let finished = dir.path().join("finished");
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy(), false);
    let (client, server_task) = connect(
        server.clone(),
        ClientCapabilities::builder().enable_tasks().build(),
    )
    .await;
    let request = CallToolRequestParams::new("Bash").with_arguments(
        serde_json::json!({
            "command": "sleep 30; printf done > finished",
            "run_in_background": true,
            "timeout": 100.0
        })
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
    .expect("integral float timeout must terminate the background command");
    assert_eq!(terminal.task.status(), TaskStatus::Completed);
    assert!(
        !finished.exists(),
        "timeout must stop the command before it writes the sentinel"
    );

    drop(client);
    server_task.await.expect("server closed");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), server.shutdown_shell_tasks())
            .await
            .expect("process cleanup deadline"),
        Some(peri_acp_types::tasks::TaskShutdownReport::Complete)
    );
}

/// [回归测试] 非整数 timeout 必须在 wire 上以 invalid_params 拒绝，且进程未启动。
#[tokio::test]
async fn background_invalid_timeout_is_rejected_before_spawn() {
    let dir = tempfile::tempdir().expect("workspace");
    let sentinel = dir.path().join("spawned");
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy(), false);
    let (client, server_task) = connect(
        server.clone(),
        ClientCapabilities::builder().enable_tasks().build(),
    )
    .await;
    for invalid in [
        serde_json::json!("15000"),
        serde_json::json!(1.5),
        serde_json::json!(-1),
    ] {
        let request = CallToolRequestParams::new("Bash").with_arguments(
            serde_json::json!({
                "command": "printf spawned > spawned",
                "run_in_background": true,
                "timeout": invalid
            })
            .as_object()
            .expect("object")
            .clone(),
        );
        let error = client
            .peer()
            .call_tool_once(request)
            .await
            .expect_err("invalid timeout must fail");
        let rmcp::ServiceError::McpError(error) = error else {
            panic!("expected MCP error, got {error}")
        };
        assert_eq!(error.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        let message = error.message.to_string();
        assert!(
            message.contains("timeout")
                && message.contains("integer")
                && message.contains(&invalid.to_string()),
            "错误应包含参数名、期望格式与收到值: {message}"
        );
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!sentinel.exists(), "参数校验必须先于进程启动");

    drop(client);
    server_task.await.expect("server closed");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), server.shutdown_shell_tasks())
            .await
            .expect("process cleanup deadline"),
        Some(peri_acp_types::tasks::TaskShutdownReport::Complete)
    );
}

// ── 阶段 4：按直接 owner 查询存活任务（host-only `shell_task_list`）────────────

fn scope_meta(owner: &str, thread_owner: &str) -> rmcp::model::RequestMetaObject {
    let mut fields = serde_json::Map::new();
    fields.insert(TASK_OWNER_META_KEY.into(), owner.into());
    fields.insert(crate::THREAD_OWNER_META_KEY.into(), thread_owner.into());
    rmcp::model::RequestMetaObject(rmcp::model::MetaObject(fields))
}

/// 调用 host-only 查询面并解析 JSON 文本；调用本身不得是工具错误。
async fn shell_task_list(
    client: &rmcp::service::RunningService<rmcp::service::RoleClient, ClientConfig>,
    owner: Option<&str>,
) -> serde_json::Value {
    let mut arguments = serde_json::Map::new();
    if let Some(owner) = owner {
        arguments.insert("owner".into(), owner.into());
    }
    let request = CallToolRequestParams::new("shell_task_list").with_arguments(arguments);
    let CallToolResponse::Complete(result) = client
        .peer()
        .call_tool_once(request)
        .await
        .expect("shell_task_list 必须可调用")
    else {
        panic!("查询必须返回 complete 结果")
    };
    assert_ne!(result.is_error, Some(true), "查询失败不得伪装成空列表");
    let text = result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.clone()))
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("查询结果必须是 JSON: {error}: {text}"))
}

/// 按 owner 过滤只返回该 owner 的存活任务；`threadOwner` 来自 `peri/threadOwner`。
#[tokio::test]
async fn shell_task_list_filters_live_tasks_by_direct_owner() {
    let dir = tempfile::tempdir().expect("workspace");
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy(), false);
    let (client, server_task) = connect(
        server.clone(),
        ClientCapabilities::builder().enable_tasks().build(),
    )
    .await;

    // 查询面不进模型工具清单：七工具面保持不变。
    let tools = client.peer().list_all_tools().await.expect("tools/list");
    assert_eq!(tools.len(), 7, "host-only 查询不得进入模型工具面");

    let mut first = CallToolRequestParams::new("Bash").with_arguments(
        serde_json::json!({"command": "sleep 30", "run_in_background": true})
            .as_object()
            .unwrap()
            .clone(),
    );
    first.meta = Some(scope_meta("session-a", "thread-a"));
    let CallToolResponse::Task(first_task) = client
        .peer()
        .call_tool_once(first)
        .await
        .expect("owner A task")
    else {
        panic!("task handle")
    };

    let mut second = CallToolRequestParams::new("Bash").with_arguments(
        serde_json::json!({"command": "sleep 30", "run_in_background": true})
            .as_object()
            .unwrap()
            .clone(),
    );
    second.meta = Some(scope_meta("session-b", "thread-b"));
    let CallToolResponse::Task(second_task) = client
        .peer()
        .call_tool_once(second)
        .await
        .expect("owner B task")
    else {
        panic!("task handle")
    };

    let listed = shell_task_list(&client, Some("session-a")).await;
    assert_eq!(listed["unknown"], serde_json::json!(false));
    let tasks = listed["tasks"].as_array().expect("tasks array");
    assert_eq!(tasks.len(), 1, "只返回该 owner 的存活任务: {listed}");
    assert_eq!(tasks[0]["taskId"], first_task.task.task_id);
    assert_eq!(tasks[0]["owner"], "session-a");
    assert_eq!(tasks[0]["threadOwner"], "thread-a");
    assert_eq!(tasks[0]["state"], "working");
    assert!(tasks[0]["startedAt"]
        .as_str()
        .is_some_and(|v| !v.is_empty()));

    let listed = shell_task_list(&client, Some("session-b")).await;
    let tasks = listed["tasks"].as_array().expect("tasks array");
    assert_eq!(tasks.len(), 1, "owner B 只看得到自己的任务: {listed}");
    assert_eq!(tasks[0]["taskId"], second_task.task.task_id);

    let listed = shell_task_list(&client, Some("session-c")).await;
    assert_eq!(
        listed["unknown"],
        serde_json::json!(false),
        "已确认没有任务必须是 known-empty，而不是 unknown"
    );
    assert_eq!(listed["tasks"], serde_json::json!([]));

    for (owner, task) in [("session-a", &first_task), ("session-b", &second_task)] {
        let mut cancel = CancelTaskParams::new(&task.task.task_id);
        cancel.meta = Some(owner_meta(owner));
        client
            .peer()
            .cancel_task(cancel)
            .await
            .expect("cancel owned task");
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let a = shell_task_list(&client, Some("session-a")).await;
            let b = shell_task_list(&client, Some("session-b")).await;
            if a["tasks"] == serde_json::json!([]) && b["tasks"] == serde_json::json!([]) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("terminal tasks leave the live list");

    drop(client);
    server_task.await.expect("join server");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), server.shutdown_shell_tasks())
            .await
            .expect("process cleanup deadline"),
        Some(peri_acp_types::tasks::TaskShutdownReport::Complete)
    );
}

/// 没有 Shell 任务状态的实例：unknown + detail，且 `tasks` 为空；不是 MCP 错误。
#[tokio::test]
async fn shell_task_list_without_task_state_reports_unknown() {
    let dir = tempfile::tempdir().expect("workspace");
    let server = WorkspaceMcpServer::new(dir.path().to_string_lossy(), None);
    let (client, server_task) = connect(server, ClientCapabilities::default()).await;

    let listed = shell_task_list(&client, None).await;
    assert_eq!(listed["unknown"], serde_json::json!(true));
    assert_eq!(listed["tasks"], serde_json::json!([]));
    assert!(
        listed["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("not enabled")),
        "未知必须携带可定位的原因: {listed}"
    );

    drop(client);
    server_task.await.expect("join server");
}
