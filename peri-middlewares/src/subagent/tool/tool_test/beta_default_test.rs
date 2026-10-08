//! beta flag `full-async-tools` 的 `Agent` 工具契约：有效缺省后台。
//!
//! 装配期注入的有效缺省只改「调用未给出 `run_in_background`」时的路径：
//! 显式 `false` 仍走前台；resume 与 MCP Agent 等「后台能力未装配」场景维持既有语义；
//! schema 的 `default` 与执行路径缺省同源（同一字段）。

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

/// 同一宿主上的工具（可选注入有效缺省）。
async fn bind_tool(
    host: &HostFixture,
    dir: &std::path::Path,
    default_run_in_background: bool,
) -> SubAgentTool {
    let tool = SubAgentTool::new(
        Arc::new(Vec::new()),
        None,
        Arc::new(|_| {
            crate::subagent::test_support::fixture_source(Arc::new(EchoLLM), "fixture-scripted")
        }),
        dir.to_str().unwrap().to_string(),
    )
    .with_default_run_in_background(default_run_in_background);
    host.bind(with_agent_face(tool, dir).await)
}

/// 有效缺省 = true：schema `default: true` 且省略字段的调用走后台。
#[tokio::test]
async fn default_background_omitted_field_starts_background_subagent() {
    let (dir, host, manager) = setup_background_host("beta-bg-default").await;
    let tool = bind_tool(&host, dir.path(), true).await;

    let params = tool.parameters();
    assert_eq!(
        params["properties"]["run_in_background"]["default"],
        serde_json::json!(true),
        "schema default 必须跟随装配期有效缺省"
    );
    assert!(
        params["properties"]["run_in_background"]["description"]
            .as_str()
            .unwrap_or_default()
            .contains("the default is true"),
        "描述必须说明该会话缺省为后台"
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

/// 有效缺省 = true：显式 `false` 仍走前台（flag 只改缺省）。
#[tokio::test]
async fn default_background_explicit_false_stays_synchronous() {
    let (dir, host, manager) = setup_background_host("beta-bg-explicit-false").await;
    let tool = bind_tool(&host, dir.path(), true).await;

    let result = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "default-bg-agent",
                "prompt": "sync",
                "run_in_background": false,
                "cwd": dir.path().to_str().unwrap(),
            }),
            host.context(&[]),
        )
        .await
        .expect("显式 false 必须走同步路径");
    assert!(
        !result.contains("Background task"),
        "显式 false 不得走后台：{result}"
    );
    assert!(
        result.contains("echo: sync"),
        "同步路径必须拿到子代理输出：{result}"
    );
    assert_eq!(manager.active_count(), 0, "同步路径不得登记后台任务");
}

/// 未注入缺省（flag 未开启）：schema `default: false`，省略字段仍走前台。
#[tokio::test]
async fn default_background_absent_stays_synchronous() {
    let (dir, host, manager) = setup_background_host("beta-bg-absent").await;
    let tool = bind_tool(&host, dir.path(), false).await;

    let params = tool.parameters();
    assert_eq!(
        params["properties"]["run_in_background"]["default"],
        serde_json::json!(false),
        "缺省注入前的 schema default 必须是 false"
    );

    let result = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "default-bg-agent",
                "prompt": "sync-by-default",
                "cwd": dir.path().to_str().unwrap(),
            }),
            host.context(&[]),
        )
        .await
        .expect("缺省 false 时省略字段必须走同步路径");
    assert!(!result.contains("Background task"), "{result}");
    assert!(result.contains("echo: sync-by-default"), "{result}");
    assert_eq!(manager.active_count(), 0, "同步路径不得登记后台任务");
}

/// 缺省 = true 但 MCP Agent 只支持同步激活：缺省不得把它们推进报错路径
/// （flag 不创造未装配的能力）；显式 `true` 仍按既有语义报错。
#[tokio::test]
async fn default_background_does_not_break_mcp_agents() {
    use crate::mcp::client::McpClientPool;
    use crate::mcp::McpAgentRegistry;

    let dir = tempfile::tempdir().unwrap();
    let empty_registry = Arc::new(McpAgentRegistry::new(Arc::new(McpClientPool::new_empty())));
    let tool = SubAgentTool::new(
        Arc::new(Vec::new()),
        None,
        Arc::new(|_| {
            crate::subagent::test_support::fixture_source(Arc::new(EchoLLM), "fixture-scripted")
        }),
        dir.path().to_str().unwrap().to_string(),
    )
    .with_default_run_in_background(true)
    .with_mcp_agents(Some(empty_registry), None);

    let omitted = tool
        .invoke(
            serde_json::json!({"subagent_type": "mcp__offline__review", "prompt": "remote"}),
            peri_agent::tools::ToolContext::new(&[], dir.path().to_str().unwrap()),
        )
        .await
        .expect_err("未激活的 MCP Agent 仍应报定义不可得")
        .to_string();
    assert!(
        !omitted.contains("synchronous activation only"),
        "缺省后台不得把 MCP Agent 推进「只支持同步」的报错：{omitted}"
    );

    let explicit = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "mcp__offline__review",
                "prompt": "remote",
                "run_in_background": true,
            }),
            peri_agent::tools::ToolContext::new(&[], dir.path().to_str().unwrap()),
        )
        .await
        .expect_err("显式 true 与 MCP Agent 互斥")
        .to_string();
    assert!(
        explicit.contains("synchronous activation only"),
        "显式 true 的既有报错必须保留：{explicit}"
    );
}
