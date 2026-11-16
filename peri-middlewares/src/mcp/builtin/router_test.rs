//! 单出口 router 的行为测试：聚合、开关矩阵、任务归属、panic 隔离与代理接线。

use std::time::Duration;

use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CancelTaskParams, ClientCapabilities,
        ContentBlock, GetTaskParams, Implementation, InitializeRequestParams, ReadResourceResponse,
        SubscriptionFilter,
    },
    service::{serve_client, RunningService},
    ClientHandler, ErrorData as McpError, RoleClient, ServiceError,
};
use tokio::io::DuplexStream;

use super::router_test_support::{TestCapability, TestMode};
use super::{
    BuiltinRouter, CapabilityReadiness, CapabilityStatus, RouterBuildError, RouterTemplate,
};
use crate::mcp::upstream_proxy::UpstreamProxy;

const IO_BUF: usize = 64 * 1024;
const TEST_TIMEOUT: Duration = Duration::from_secs(5);

fn pair() -> (DuplexStream, DuplexStream) {
    tokio::io::duplex(IO_BUF)
}

#[derive(Clone)]
struct Recorder;

impl ClientHandler for Recorder {
    fn get_info(&self) -> InitializeRequestParams {
        InitializeRequestParams::new(
            ClientCapabilities::builder().enable_tasks().build(),
            Implementation::new("router-test", "0.1.0"),
        )
    }
}

async fn serve_router(router: BuiltinRouter) -> RunningService<RoleClient, Recorder> {
    let (server_io, client_io) = pair();
    tokio::spawn(async move {
        if let Ok(service) = rmcp::serve_server(router, server_io).await {
            let _ = service.waiting().await;
        }
    });
    serve_client(Recorder, client_io)
        .await
        .expect("router handshake")
}

fn tool_names(result: &rmcp::model::ListToolsResult) -> Vec<String> {
    result
        .tools
        .iter()
        .map(|tool| tool.name.to_string())
        .collect()
}

fn mcp_error(error: ServiceError) -> McpError {
    match error {
        ServiceError::McpError(error) => error,
        other => panic!("expected an MCP error, got {other:?}"),
    }
}

fn text_of(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect()
}

fn call_arguments(value: serde_json::Value) -> rmcp::model::JsonObject {
    value.as_object().cloned().unwrap_or_default()
}

fn router_with(
    handlers: Vec<(&'static str, TestCapability)>,
    disabled: &[&'static str],
) -> BuiltinRouter {
    BuiltinRouter::for_test(handlers, disabled).expect("router builds")
}

#[tokio::test]
async fn tools_aggregate_with_v5_names_and_direct_names() {
    let (workspace, _) = TestCapability::new("workspace", TestMode::Normal);
    let (cron, cron_probe) = TestCapability::new("cron", TestMode::Normal);
    let router = router_with(
        vec![("workspace", workspace), ("cron", cron)],
        &["web", "artifact", "monitor", "workflow"],
    );
    let client = serve_router(router).await;
    let tools = client.peer().list_tools(None).await.expect("tools/list");
    let names = tool_names(&tools);
    assert!(names.contains(&"Read".to_owned()), "{names:?}");
    assert!(names.contains(&"Bash".to_owned()), "{names:?}");
    assert!(
        names.contains(&"mcp__peri__cron_list".to_owned()),
        "{names:?}"
    );
    assert!(!names.contains(&"cron_list".to_owned()), "{names:?}");

    let response = client
        .peer()
        .call_tool_once(
            CallToolRequestParams::new("mcp__peri__cron_list")
                .with_arguments(call_arguments(serde_json::json!({}))),
        )
        .await
        .expect("deferred call");
    match response {
        CallToolResponse::Complete(result) => {
            assert_eq!(text_of(&result), "cron:cron_list");
        }
        other => panic!("unexpected response {other:?}"),
    }
    assert_eq!(cron_probe.calls(), 1);
}

#[tokio::test]
async fn disabled_capability_is_absent_and_unauthorized() {
    let (workspace, _) = TestCapability::new("workspace", TestMode::Normal);
    let router = router_with(
        vec![("workspace", workspace)],
        &["web", "artifact", "cron", "monitor", "workflow"],
    );
    let client = serve_router(router).await;
    let names = tool_names(&client.peer().list_tools(None).await.expect("tools/list"));
    assert!(!names.iter().any(|name| name.contains("cron")));

    let error = mcp_error(
        client
            .peer()
            .call_tool_once(CallToolRequestParams::new("mcp__peri__cron_list"))
            .await
            .expect_err("disabled capability must be rejected"),
    );
    assert!(error.message.contains("disabled"), "{}", error.message);

    let legacy = mcp_error(
        client
            .peer()
            .call_tool_once(CallToolRequestParams::new("mcp__cron__cron_list"))
            .await
            .expect_err("legacy name must not execute"),
    );
    assert!(
        legacy.message.contains("display-only") || legacy.message.contains("unknown tool"),
        "{}",
        legacy.message
    );
}

#[tokio::test]
async fn disabled_capability_resources_are_not_readable() {
    let (workspace, _) = TestCapability::new("workspace", TestMode::Normal);
    let router = router_with(
        vec![("workspace", workspace)],
        &["web", "artifact", "cron", "monitor", "workflow"],
    );
    let client = serve_router(router).await;
    let resources = client
        .peer()
        .list_resources(None)
        .await
        .expect("resources/list");
    assert!(resources.resources.iter().all(|resource| {
        [
            "workspace://",
            "skill://",
            "agent://",
            "peri-instruction://",
            "peri-meta://",
        ]
        .iter()
        .any(|prefix| resource.uri.starts_with(prefix))
    }));

    let error = mcp_error(
        client
            .peer()
            .read_resource_once(rmcp::model::ReadResourceRequestParams::new("cron://jobs"))
            .await
            .expect_err("disabled capability resource must be rejected"),
    );
    assert!(error.message.contains("disabled"), "{}", error.message);
}

#[tokio::test]
async fn resources_route_by_uri_prefix() {
    let (workspace, _) = TestCapability::new("workspace", TestMode::Normal);
    let (cron, _) = TestCapability::new("cron", TestMode::Normal);
    let router = router_with(
        vec![("workspace", workspace), ("cron", cron)],
        &["web", "artifact", "monitor", "workflow"],
    );
    let client = serve_router(router).await;
    let workspace_read = client
        .peer()
        .read_resource_once(rmcp::model::ReadResourceRequestParams::new(
            "workspace://notes.md",
        ))
        .await
        .expect("workspace read");
    let ReadResourceResponse::Complete(workspace_read) = workspace_read else {
        panic!("unexpected read response");
    };
    assert!(format!("{workspace_read:?}").contains("workspace:workspace://notes.md"));

    let cron_read = client
        .peer()
        .read_resource_once(rmcp::model::ReadResourceRequestParams::new("cron://jobs"))
        .await
        .expect("cron read");
    let ReadResourceResponse::Complete(cron_read) = cron_read else {
        panic!("unexpected read response");
    };
    assert!(format!("{cron_read:?}").contains("cron:cron://jobs"));
}

#[tokio::test]
async fn capability_panic_does_not_break_other_capabilities() {
    let (workspace, _) = TestCapability::new("workspace", TestMode::PanicOnCall);
    let (web, web_probe) = TestCapability::new("web", TestMode::Normal);
    let router = router_with(
        vec![("workspace", workspace), ("web", web)],
        &["artifact", "cron", "monitor", "workflow"],
    );
    let client = serve_router(router).await;

    let error = mcp_error(
        client
            .peer()
            .call_tool_once(CallToolRequestParams::new("Read"))
            .await
            .expect_err("panic must be isolated"),
    );
    assert!(error.message.contains("panicked"), "{}", error.message);

    let response = client
        .peer()
        .call_tool_once(CallToolRequestParams::new("WebSearch"))
        .await
        .expect("web capability remains available");
    match response {
        CallToolResponse::Complete(result) => assert_eq!(text_of(&result), "web:WebSearch"),
        other => panic!("unexpected response {other:?}"),
    }
    assert_eq!(web_probe.calls(), 1);
    assert!(client.peer().list_tools(None).await.is_ok());
}

#[tokio::test]
async fn failed_capability_list_is_skipped() {
    let (workspace, _) = TestCapability::new("workspace", TestMode::Normal);
    let (cron, cron_probe) = TestCapability::new("cron", TestMode::ErrorOnList);
    let router = router_with(
        vec![("workspace", workspace), ("cron", cron)],
        &["web", "artifact", "monitor", "workflow"],
    );
    let client = serve_router(router).await;
    let names = tool_names(&client.peer().list_tools(None).await.expect("tools/list"));
    assert!(names.contains(&"Read".to_owned()));
    assert!(!names.iter().any(|name| name.contains("cron")));
    assert_eq!(cron_probe.listed(), 1);
}

#[tokio::test]
async fn task_ids_route_by_registry_and_capability_prefix() {
    let (workspace, workspace_probe) =
        TestCapability::new("workspace", TestMode::CreateTask("task-1".to_owned()));
    let (cron, cron_probe) = TestCapability::new("cron", TestMode::Normal);
    let router = router_with(
        vec![("workspace", workspace), ("cron", cron)],
        &["web", "artifact", "monitor", "workflow"],
    );
    let client = serve_router(router).await;

    let response = client
        .peer()
        .call_tool_once(CallToolRequestParams::new("Read"))
        .await
        .expect("task creation");
    match response {
        CallToolResponse::Task(task) => assert_eq!(task.task.task_id, "task-1"),
        other => panic!("expected a task, got {other:?}"),
    }

    client
        .peer()
        .cancel_task(CancelTaskParams::new("task-1"))
        .await
        .expect("registered task id routes");
    assert_eq!(workspace_probe.calls(), 2);

    client
        .peer()
        .cancel_task(CancelTaskParams::new("cron:task:7"))
        .await
        .expect("capability-prefixed task id routes");
    assert_eq!(cron_probe.calls(), 1);

    let error = mcp_error(
        client
            .peer()
            .cancel_task(CancelTaskParams::new("unknown-task"))
            .await
            .expect_err("unknown task id is deterministic"),
    );
    assert!(
        error.message.contains("unknown task id"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn duplicate_task_ids_across_capabilities_are_rejected() {
    let (workspace, _) =
        TestCapability::new("workspace", TestMode::CreateTask("shared".to_owned()));
    let (cron, _) = TestCapability::new("cron", TestMode::CreateTask("shared".to_owned()));
    let router = router_with(
        vec![("workspace", workspace), ("cron", cron)],
        &["web", "artifact", "monitor", "workflow"],
    );
    let client = serve_router(router).await;
    client
        .peer()
        .call_tool_once(CallToolRequestParams::new("Read"))
        .await
        .expect("first task registers");
    let error = mcp_error(
        client
            .peer()
            .call_tool_once(CallToolRequestParams::new("mcp__peri__cron_list"))
            .await
            .expect_err("duplicate task id must fail"),
    );
    assert!(error.message.contains("already owned"), "{}", error.message);
}

#[test]
fn readiness_matches_effective_capability_set() {
    let (workspace, _) = TestCapability::new("workspace", TestMode::Normal);
    let (cron, _) = TestCapability::new("cron", TestMode::Degraded("init failed".to_owned()));
    let router = router_with(
        vec![("workspace", workspace), ("cron", cron)],
        &["web", "artifact", "monitor", "workflow"],
    );
    let readiness = router.readiness();
    assert!(readiness.ready);
    assert_eq!(readiness.effective, vec!["workspace"]);
    assert_eq!(
        readiness.capabilities,
        vec![
            CapabilityReadiness {
                capability: "workspace",
                status: CapabilityStatus::Ready,
                tools: 7,
                resources: 19,
            },
            CapabilityReadiness {
                capability: "web",
                status: CapabilityStatus::Disabled,
                tools: 2,
                resources: 0,
            },
            CapabilityReadiness {
                capability: "artifact",
                status: CapabilityStatus::Disabled,
                tools: 1,
                resources: 0,
            },
            CapabilityReadiness {
                capability: "cron",
                status: CapabilityStatus::Degraded {
                    reason: "init failed".to_owned()
                },
                tools: 4,
                resources: 2,
            },
            CapabilityReadiness {
                capability: "monitor",
                status: CapabilityStatus::Disabled,
                tools: 4,
                resources: 0,
            },
            CapabilityReadiness {
                capability: "workflow",
                status: CapabilityStatus::Disabled,
                tools: 3,
                resources: 6,
            },
        ]
    );

    let info = rmcp::ServerHandler::get_info(&router);
    assert!(info.capabilities.tools.is_some());
    assert!(info.capabilities.resources.is_some());
}

#[test]
fn all_capabilities_degraded_fails_closed() {
    let (workspace, _) = TestCapability::new("workspace", TestMode::Degraded("a".to_owned()));
    let (cron, _) = TestCapability::new("cron", TestMode::Degraded("b".to_owned()));
    let error = BuiltinRouter::for_test(
        vec![("workspace", workspace), ("cron", cron)],
        &["web", "artifact", "monitor", "workflow"],
    )
    .expect_err("all-degraded router must fail closed");
    assert!(matches!(error, RouterBuildError::NoCapabilities { .. }));
}

#[test]
fn subscription_filter_uses_enabled_defaults() {
    let (workspace, _) = TestCapability::new("workspace", TestMode::Normal);
    let (cron, _) = TestCapability::new("cron", TestMode::Normal);
    let router = router_with(
        vec![("workspace", workspace), ("cron", cron)],
        &["web", "artifact", "monitor", "workflow"],
    );
    let requested = SubscriptionFilter::builder()
        .resource_subscriptions(["workspace://git/ref", "cron://jobs"])
        .build();
    let accepted = rmcp::ServerHandler::accepted_subscription_filter(&router, &requested)
        .expect("subscriptions are supported");
    assert_eq!(
        accepted.resource_subscriptions,
        Some(vec!["workspace://git/ref".to_owned()])
    );
}

#[test]
fn unknown_capability_is_a_typed_build_error() {
    let error = match RouterTemplate::new(&["sandbox".to_owned()], std::env::temp_dir()) {
        Ok(_) => panic!("unknown capability must be rejected"),
        Err(error) => error,
    };
    assert!(matches!(error, RouterBuildError::UnknownCapability { .. }));
}

#[tokio::test]
async fn workspace_capability_can_use_upstream_proxy() {
    let temp = tempfile::tempdir().expect("temp dir");
    let file = temp.path().join("notes.md");
    std::fs::write(&file, "proxied workspace content").expect("write fixture");
    let workspace = peri_mcp_workspace::WorkspaceMcpServer::standalone(
        temp.path().to_string_lossy().into_owned(),
    );
    let (server_io, client_io) = pair();
    let mut server = rmcp::service::serve_directly(workspace, server_io, None);
    let proxy = UpstreamProxy::new("workspace");
    let connection = proxy
        .connect_upstream(client_io, TEST_TIMEOUT)
        .await
        .expect("proxy connects workspace upstream");

    let template = RouterTemplate::with_workspace_upstream(
        &["workspace".to_owned()],
        temp.path().to_path_buf(),
        proxy,
    )
    .expect("template");
    let router = template.create().expect("router");
    let client = serve_router(router).await;

    let names = tool_names(&client.peer().list_tools(None).await.expect("tools/list"));
    assert!(names.contains(&"Read".to_owned()), "{names:?}");

    let response = client
        .peer()
        .call_tool_once(
            CallToolRequestParams::new("Read").with_arguments(call_arguments(
                serde_json::json!({ "file_path": file.to_string_lossy() }),
            )),
        )
        .await
        .expect("proxied Read call");
    match response {
        CallToolResponse::Complete(result) => {
            assert!(
                text_of(&result).contains("proxied workspace content"),
                "{}",
                text_of(&result)
            );
        }
        other => panic!("unexpected response {other:?}"),
    }

    let resources = client.peer().list_resources(None).await.expect("resources");
    assert!(resources
        .resources
        .iter()
        .any(|resource| resource.uri.starts_with("workspace://")));

    connection.shutdown();
    let _ = server.close_with_timeout(Duration::from_millis(500)).await;
}

#[tokio::test]
async fn workspace_proxy_degradation_only_removes_workspace() {
    let proxy = UpstreamProxy::new("workspace");
    let template = RouterTemplate::with_workspace_upstream(
        &["workspace".to_owned(), "web".to_owned()],
        std::env::temp_dir(),
        proxy,
    )
    .expect("template");
    // 未连接的代理在会话建立时不可用；web 仍必须构造成功（能力级降级）。
    let router = template.create().expect("web keeps the router ready");
    let readiness = router.readiness();
    assert_eq!(readiness.effective, vec!["web"]);
    assert!(matches!(
        readiness
            .capabilities
            .iter()
            .find(|entry| entry.capability == "workspace")
            .map(|entry| &entry.status),
        Some(CapabilityStatus::Degraded { .. })
    ));

    let (web_again, probe_again) = TestCapability::new("web", TestMode::Normal);
    let router = router_with(
        vec![("web", web_again)],
        &["workspace", "artifact", "cron", "monitor", "workflow"],
    );
    let client = serve_router(router).await;
    let response = client
        .peer()
        .call_tool_once(CallToolRequestParams::new("WebSearch"))
        .await
        .expect("web call");
    match response {
        CallToolResponse::Complete(result) => assert_eq!(text_of(&result), "web:WebSearch"),
        other => panic!("unexpected response {other:?}"),
    }
    assert_eq!(probe_again.calls(), 1);
}

#[tokio::test]
async fn get_task_routes_by_capability_prefix() {
    let (workspace, _) =
        TestCapability::new("workspace", TestMode::CreateTask("task-1".to_owned()));
    let router = router_with(
        vec![("workspace", workspace)],
        &["web", "artifact", "cron", "monitor", "workflow"],
    );
    let client = serve_router(router).await;
    // 未注册但带 capability 前缀的 id 路由到 workspace；该能力未实现 get_task，
    // 因此返回 method-not-found 而不是 router 的 unknown-task 错误。
    let error = mcp_error(
        client
            .peer()
            .get_task(GetTaskParams::new("workspace:task:1"))
            .await
            .expect_err("handler has no get_task"),
    );
    assert_ne!(error.code, rmcp::model::ErrorCode::INVALID_PARAMS);

    let unknown = mcp_error(
        client
            .peer()
            .get_task(GetTaskParams::new("other:task:1"))
            .await
            .expect_err("unknown capability prefix"),
    );
    assert_eq!(unknown.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn router_without_workspace_has_no_cleanup_handle() {
    let template =
        RouterTemplate::new(&["web".to_owned()], std::env::temp_dir()).expect("web-only template");
    assert!(template.workspace_handle().is_none());
    let router = template.create().expect("web-only router");
    assert_eq!(router.effective_capabilities(), vec!["web"]);
}

#[test]
fn initialization_failure_is_reported_as_degraded() {
    let (workspace, _) = TestCapability::new("workspace", TestMode::Normal);
    let mut router = router_with(
        vec![("workspace", workspace)],
        &["web", "artifact", "monitor", "workflow"],
    );
    router.mark_degraded("cron", "cron scheduler failed");
    let readiness = router.readiness();
    assert!(readiness.ready);
    assert_eq!(readiness.effective, vec!["workspace"]);
    let cron = readiness
        .capabilities
        .iter()
        .find(|entry| entry.capability == "cron")
        .expect("degraded capability stays in diagnostics");
    assert_eq!(
        cron.status,
        CapabilityStatus::Degraded {
            reason: "cron scheduler failed".to_owned()
        }
    );
    assert_eq!(cron.tools, 4);
    assert_eq!(cron.resources, 2);
}

#[tokio::test]
async fn degraded_capability_is_rejected_as_unavailable() {
    let (workspace, _) = TestCapability::new("workspace", TestMode::Normal);
    let mut router = router_with(
        vec![("workspace", workspace)],
        &["web", "artifact", "monitor", "workflow"],
    );
    router.mark_degraded("cron", "cron scheduler failed");
    let client = serve_router(router).await;
    let error = mcp_error(
        client
            .peer()
            .call_tool_once(CallToolRequestParams::new("mcp__peri__cron_list"))
            .await
            .expect_err("degraded capability must be rejected"),
    );
    assert!(error.message.contains("unavailable"), "{}", error.message);
    assert!(
        error.message.contains("cron scheduler failed"),
        "{}",
        error.message
    );
}
