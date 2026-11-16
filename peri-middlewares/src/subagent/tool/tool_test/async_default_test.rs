//! v5 `Agent` 工具契约：后台是唯一执行模式。
//!
//! 覆盖四条边界：
//! - 省略 `run_in_background`：后台启动（schema 声明 `default: true`）；
//! - 显式 `false`：仍后台启动，不唤回同步路径；
//! - 无任务管理器：显式报错，不静默落回同步；
//! - MCP Agent：审批门仍在后台启动之前执行，后台 loader 不能成为绕过入口。

use std::sync::Arc;

use super::*;

/// 准备一个带后台任务管理器的宿主与本地 agent 定义（定义来自会话绑定的资源面）。
async fn setup_background_host(
    label: &str,
) -> (
    tempfile::TempDir,
    HostFixture,
    Arc<peri_agent::agent::async_tasks::TaskManager>,
) {
    let dir = tempfile::tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("default-bg-agent.md"),
        "---\nname: default-bg-agent\ndescription: Default background agent\n---\n\nDefault background.\n",
    )
    .unwrap();
    let (bg_tx, _bg_rx) =
        tokio::sync::mpsc::unbounded_channel::<peri_agent::agent::events::ExecutorEvent>();
    let manager = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let host =
        HostFixture::open_in_with_background(dir.path(), label, Arc::clone(&manager), bg_tx).await;
    (dir, host, manager)
}

/// 同一宿主上的工具。
async fn bind_tool(host: &HostFixture, dir: &std::path::Path) -> SubAgentTool {
    let tool = SubAgentTool::new(
        Arc::new(Vec::new()),
        None,
        Arc::new(|_| {
            crate::subagent::test_support::fixture_source(Arc::new(EchoLLM), "fixture-scripted")
        }),
        dir.to_str().unwrap().to_string(),
    );
    host.bind(with_agent_face(tool, dir).await)
}

/// 默认模式：schema `default: true`，省略字段的调用走后台。
#[tokio::test]
async fn async_default_omitted_field_starts_background_subagent() {
    let (dir, host, manager) = setup_background_host("async-bg-default").await;
    let tool = bind_tool(&host, dir.path()).await;

    let params = tool.parameters();
    assert_eq!(
        params["properties"]["run_in_background"]["default"],
        serde_json::json!(true),
        "schema default 必须声明后台为默认"
    );
    assert!(
        params["properties"]["run_in_background"]["description"]
            .as_str()
            .unwrap_or_default()
            .contains("mandatory"),
        "描述必须说明后台是强制模式"
    );

    let result = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "default-bg-agent",
                "prompt": "bg-by-default",
                "cwd": dir.path().to_str().unwrap(),
            }),
            host.context(&[]),
        )
        .await
        .expect("缺省后台必须成功发起");
    assert!(
        result.contains("Background task"),
        "省略 run_in_background 必须走后台：{result}"
    );
    peri_acp_types::tasks::TaskManager::shutdown(manager.as_ref()).await;
}

/// 显式 `false` 不再唤醒同步路径：仍后台启动，不返回同步结果。
#[tokio::test]
async fn async_default_explicit_false_never_waits_synchronously() {
    let (dir, host, manager) = setup_background_host("async-bg-explicit-false").await;
    let tool = bind_tool(&host, dir.path()).await;

    let result = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "default-bg-agent",
                "prompt": "must-not-run-sync",
                "run_in_background": false,
                "cwd": dir.path().to_str().unwrap(),
            }),
            host.context(&[]),
        )
        .await
        .expect("显式 false 必须仍走后台路径");
    assert!(
        result.contains("Background task"),
        "显式 false 不得落回同步：{result}"
    );
    assert!(
        !result.contains("echo: must-not-run-sync"),
        "同步执行路径不得被唤醒：{result}"
    );
    peri_acp_types::tasks::TaskManager::shutdown(manager.as_ref()).await;
}

/// 无任务管理器：后台能力缺失必须显式报错，不得静默执行同步路径。
#[tokio::test]
async fn async_default_without_task_manager_fails_instead_of_sync_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("default-bg-agent.md"),
        "---\nname: default-bg-agent\ndescription: Default background agent\n---\n\nDefault background.\n",
    )
    .unwrap();
    let tool = SubAgentTool::new(
        Arc::new(Vec::new()),
        None,
        Arc::new(|_| {
            crate::subagent::test_support::fixture_source(Arc::new(EchoLLM), "fixture-scripted")
        }),
        dir.path().to_str().unwrap().to_string(),
    );
    // 定义面可用（只缺后台能力）：确保错误来自能力门而不是定义加载。
    let tool = super::with_agent_face(tool, dir.path()).await;

    let error = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "default-bg-agent",
                "prompt": "no-manager",
                "cwd": dir.path().to_str().unwrap(),
            }),
            peri_agent::tools::ToolContext::new(&[], dir.path().to_str().unwrap()),
        )
        .await
        .expect_err("无任务管理器时不得执行同步路径")
        .to_string();
    assert!(
        error.contains("requires a background task manager"),
        "错误必须点明后台管理器缺失：{error}"
    );
    assert!(
        !error.contains("echo: no-manager"),
        "不得静默落回同步执行：{error}"
    );
}

/// MCP Agent：后台启动前必须先过审批门；显式 `true` 不再是错误。
///
/// 后台 loader（`load_agent_def` → `registry.activate`）不经过内容绑定审批，
/// 因此审批必须在进入后台路径之前完成。无 registry / broker 时审批路径显式
/// 拒绝，且不登记任何后台任务。
#[tokio::test]
async fn async_default_mcp_agent_keeps_approval_gate_before_background() {
    use crate::mcp::client::McpClientPool;
    use crate::mcp::McpAgentRegistry;

    let dir = tempfile::tempdir().unwrap();
    let (bg_tx, _bg_rx) =
        tokio::sync::mpsc::unbounded_channel::<peri_agent::agent::events::ExecutorEvent>();
    let manager = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let host = HostFixture::open_in_with_background(
        dir.path(),
        "async-bg-mcp-approval",
        Arc::clone(&manager),
        bg_tx,
    )
    .await;
    let empty_registry = Arc::new(McpAgentRegistry::new(Arc::new(McpClientPool::new_empty())));
    let tool = host.bind(
        SubAgentTool::new(
            Arc::new(Vec::new()),
            None,
            Arc::new(|_| {
                crate::subagent::test_support::fixture_source(Arc::new(EchoLLM), "fixture-scripted")
            }),
            dir.path().to_str().unwrap().to_string(),
        )
        .with_mcp_agents(Some(empty_registry), None),
    );

    for (label, input) in [
        (
            "omitted",
            serde_json::json!({"subagent_type": "mcp__offline__review", "prompt": "remote"}),
        ),
        (
            "explicit-true",
            serde_json::json!({
                "subagent_type": "mcp__offline__review",
                "prompt": "remote",
                "run_in_background": true,
            }),
        ),
    ] {
        let error = tool
            .invoke(input, host.context(&[]))
            .await
            .expect_err("未激活的 MCP Agent 仍必须报定义不可得")
            .to_string();
        assert!(
            !error.contains("synchronous activation only"),
            "{label}: 同步激活限制已随 v5 移除：{error}"
        );
        assert!(
            error.contains("MCP agent definition"),
            "{label}: 必须走审批门报「MCP agent definition」不可得：{error}"
        );
        assert!(
            !error.contains("Check .claude/agents/"),
            "{label}: 不得进入后台/本地加载路径：{error}"
        );
        assert_eq!(
            manager.active_count(),
            0,
            "{label}: 审批失败后不得登记后台任务"
        );
    }
}
