//! `mcp::server` 的单元测试（WP-005）。
//!
//! `call_tool`/`list_tools` 这类需要 `RequestContext<RoleServer>`（内含 `Peer`）的路径
//! 无法在本单元里构造，它们由 `tests/transport_stdio.rs` 的真实子进程 raw wire 覆盖。
//! 这里覆盖的是**构造期承诺**：能力声明与实际可达面一致、协议版本显式声明、说明文本
//! 不篡改工具契约，以及协议层自身没有文件系统/进程能力。
//!
//! 关于最后一条：单进程形态（D-003）下协议层与工具执行在**同一个进程**里，工具确实会碰
//! 文件系统与进程；这条不变量约束的是**这一层**——它只能经注入的
//! [`crate::wire::ToolExecutor`] 触达执行，不得自己 `std::fs::read` 或起进程。这是结构性
//! 不变量，因此用针对本模块生产源码的静态断言钉住：实现者若在协议层偷偷加一条直连路径，
//! 该测试立刻失败。

use std::sync::Arc;

use super::SandboxServer;
use crate::mcp::identity::ConnectionIdentity;
use crate::mcp::resources::{TaskStatusSource, TASKS_URI};
use crate::wire::{BoxFuture, TaskSnapshot, ToolExecutor, ToolRequest, ToolResponse};

/// 记录调用的最小执行器：本单元不验证执行语义，只提供一个合法 seam。
struct RecordingExecutor;

impl ToolExecutor for RecordingExecutor {
    fn execute<'a>(
        &'a self,
        request: ToolRequest,
    ) -> BoxFuture<'a, Result<ToolResponse, crate::error::ToolError>> {
        Box::pin(async move {
            Ok(ToolResponse::ok(
                format!("ran {}", request.name),
                crate::wire::StructuredOutput::ok(request.name),
            ))
        })
    }
}

/// 空任务源：只用于声明能力，不返回任何任务。
struct EmptyTasks;

impl TaskStatusSource for EmptyTasks {
    fn snapshots<'a>(
        &'a self,
        _principal: &'a str,
        _client: &'a str,
    ) -> BoxFuture<'a, Vec<TaskSnapshot>> {
        Box::pin(async { Vec::new() })
    }

    fn snapshot<'a>(
        &'a self,
        _principal: &'a str,
        _client: &'a str,
        _task_id: &'a str,
    ) -> BoxFuture<'a, Option<TaskSnapshot>> {
        Box::pin(async { None })
    }
}

fn server(with_tasks: bool) -> SandboxServer {
    let tasks: Option<Arc<dyn TaskStatusSource>> =
        with_tasks.then(|| Arc::new(EmptyTasks) as Arc<dyn TaskStatusSource>);
    SandboxServer::new(
        Arc::new(RecordingExecutor),
        ConnectionIdentity::new("principal-a", "conn-a"),
        tasks,
    )
}

#[test]
fn test_tools_capability_is_always_declared_and_list_is_not_changing() {
    for with_tasks in [false, true] {
        let capabilities = server(with_tasks).capabilities();
        let tools = capabilities.tools.expect("必须声明 tools 能力");
        assert_eq!(
            tools.list_changed,
            Some(false),
            "注册面固定，不发布 list_changed"
        );
    }
}

#[test]
fn test_resources_capability_appears_exactly_when_task_source_is_wired() {
    let without = server(false).capabilities();
    assert!(
        without.resources.is_none(),
        "没有任务来源时不得声明 resources，否则会出现声明了但不可达的能力"
    );

    let with = server(true).capabilities();
    let resources = with.resources.expect("接入了任务来源就必须声明 resources");
    assert_eq!(resources.subscribe, Some(true));
    assert_eq!(resources.list_changed, Some(false));
}

#[test]
fn test_supported_versions_are_declared_explicitly() {
    let versions: Vec<&str> = super::SUPPORTED_VERSIONS
        .iter()
        .map(|version| version.as_str())
        .collect();
    assert_eq!(versions, vec!["2026-07-28", "2025-11-25"]);
    let handler = server(true);
    let declared = rmcp::ServerHandler::supported_protocol_versions(&handler);
    assert_eq!(declared.as_ref(), super::SUPPORTED_VERSIONS);
}

#[test]
fn test_server_identity_is_stable_and_versioned() {
    let implementation = SandboxServer::server_implementation();
    assert_eq!(implementation.name, "local-mcp-server");
    assert_eq!(implementation.version, env!("CARGO_PKG_VERSION"));
}

#[test]
fn test_instructions_document_the_frozen_tool_surface() {
    let instructions = SandboxServer::instructions();
    for needle in [
        "Read",
        "Write",
        "Edit",
        "Glob",
        "Grep",
        "folder_operations",
        "Bash",
        "reading",
        "Shell",
        "command/timeout/run_in_background",
        TASKS_URI,
        "subscriptions/listen",
        "resources/subscribe",
    ] {
        assert!(
            instructions.contains(needle),
            "说明文本必须登记 {needle}，否则接入方无从得知别名与任务状态入口"
        );
    }
    assert!(
        instructions.contains("不会出现在列表里"),
        "必须明确别名不是 tools/list 条目"
    );
}

#[test]
fn test_protocol_layer_never_touches_filesystem_or_processes() {
    let production_sources: [(&str, &str); 5] = [
        ("catalog.rs", include_str!("catalog.rs")),
        ("error_map.rs", include_str!("error_map.rs")),
        ("identity.rs", include_str!("identity.rs")),
        ("resources.rs", include_str!("resources.rs")),
        ("server.rs", include_str!("server.rs")),
    ];
    for (file, source) in production_sources {
        for forbidden in [
            "std::fs",
            "std::process",
            "tokio::process",
            "Command::new",
            "std::path::PathBuf",
        ] {
            assert!(
                !source.contains(forbidden),
                "{file} 出现了 {forbidden}：协议层只能经注入的 ToolExecutor 触达执行，不得自带 FS/进程能力"
            );
        }
    }
}
