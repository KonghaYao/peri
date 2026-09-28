//! builtin 实例的**运行时 / 关闭面** crate 内验收。
//!
//! owner 传递：W3 由 I-02 建挂载点并落地关闭面（IF-D10 面②：目录收集路径）断言；
//! W4 由 **V-01** 在本文件内扩展为：启动路径（modern 握手 + `tools/list` 提交）、
//! 审批 approve/reject 双断言、wire 计数、关闭矩阵四面、无 orphan task、
//! 大 payload（A16）、隔离可观察四项（A13）。
//!
//! 本文件断言只落在**可观察能力面**（`Middleware::collect_tools` 产出，即链工具集合），
//! 不依赖中间量；测试只用注入的本地假 handle / 假 token，不含真实凭据，也不打印 env。

use std::sync::Arc;

use peri_agent::middleware::r#trait::Middleware;

use crate::mcp::builtin::closed_instances;
use crate::mcp::{ClientStatus, McpClientHandle, McpClientPool, McpMiddleware, Tool};

/// 构造一个「已连接」的假 MCP client（工具名按传入列表）。
fn connected_handle(server: &str, tools: &[&str]) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        name: server.to_string(),
        version: None,
        cache_version: None,
        peer: None,
        tools: tools.iter().map(|tool| make_tool(tool)).collect(),
        resources: vec![],
        status: ClientStatus::Connected,
        oauth_status: Default::default(),
        source: peri_acp_types::builtin_mcp::find(server).map(|_| {
            crate::mcp::config::ConfigSource::Builtin {
                instance: server.to_string(),
            }
        }),
        url: None,
        skills_capable: false,
        channel_capable: false,
    })
}

fn make_tool(name: &str) -> Tool {
    serde_json::from_value(serde_json::json!({
        "name": name,
        "description": "builtin tool",
        "inputSchema": { "type": "object", "properties": {} }
    }))
    .unwrap()
}

/// 一个含**五个** builtin 实例（web / artifact / cron / lsp / workspace）+ 一个外部
/// server 的 deployment pool。
///
/// 工具清单与 builtin 注册表一致：`web` 提供 WebSearch / WebFetch，`artifact` 提供
/// artifact，`cron` 提供三个 cron 工具，`lsp` 提供 `LSP`（生效配置非空形态——空配置形态
/// 的「可见但空表」由 `mcp::builtin::lsp` 的用例覆盖），`workspace` 提供其 7 项（wave 3
/// 落地，AW3-03；名字从注册表派生，不在此处第二份硬编码），外部 server 提供一个 deferred
/// 工具，用于验证关闭过滤**只**作用于 builtin 实例。
///
/// 五处 builtin 条目都是**假 handle**（不经 transport）：本夹具服务于可观察能力面
/// （`collect_tools` / 关闭集），真实链路的落地事实由 [`StartupFixture`] 承担。
fn pool_with_builtin_instances() -> Arc<McpClientPool> {
    let pool = Arc::new(McpClientPool::new_empty());
    pool.clients.write().insert(
        "web".to_string(),
        connected_handle("web", &["WebSearch", "WebFetch"]),
    );
    pool.clients.write().insert(
        "artifact".to_string(),
        connected_handle("artifact", &["artifact"]),
    );
    pool.clients.write().insert(
        "cron".to_string(),
        connected_handle("cron", &["cron_register", "cron_list", "cron_remove"]),
    );
    pool.clients
        .write()
        .insert("lsp".to_string(), connected_handle("lsp", &["LSP"]));
    let workspace_tools: Vec<&str> = peri_acp_types::builtin_mcp::find("workspace")
        .expect("workspace 已实现（wave 3）")
        .tools
        .iter()
        .map(|tool| tool.original_name)
        .collect();
    pool.clients.write().insert(
        "workspace".to_string(),
        connected_handle("workspace", &workspace_tools),
    );
    pool.clients.write().insert(
        "external".to_string(),
        connected_handle("external", &["Read"]),
    );
    pool
}

/// 链工具集合（可观察能力面）：`McpMiddleware::collect_tools` 的产出。
fn collected_tool_names(pool: &Arc<McpClientPool>, disabled: &[&str]) -> Vec<String> {
    let disabled: std::collections::HashSet<String> =
        disabled.iter().map(|name| name.to_string()).collect();
    let middleware = McpMiddleware::new(Arc::clone(pool))
        .with_tool_pool(Arc::clone(pool))
        .with_builtin_closures(closed_instances(&disabled));
    middleware
        .collect_tools("/tmp/contract-test")
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect()
}

/// 未关闭任何实例：注册表声明的**全部** builtin 工具都在目录里（web 两个、artifact 一个、
/// cron 三个、lsp 一个），且声明的 direct 逐项生效（IF-D13：cron / lsp 一律 deferred），
/// 外部 server 的工具照旧 deferred 存在。
#[test]
fn builtin_tools_are_collected_with_declared_direct() {
    let pool = pool_with_builtin_instances();
    let disabled: std::collections::HashSet<String> = std::collections::HashSet::new();
    let middleware = McpMiddleware::new(Arc::clone(&pool))
        .with_tool_pool(Arc::clone(&pool))
        .with_builtin_closures(closed_instances(&disabled));
    let tools = middleware.collect_tools("/tmp/contract-test");

    let direct_of = |name: &str| -> Option<bool> {
        tools
            .iter()
            .find(|tool| tool.name() == name)
            .map(|tool| tool.is_direct())
    };
    for instance in peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES {
        for declaration in instance.tools {
            assert_eq!(
                direct_of(declaration.effective_name),
                Some(declaration.direct),
                "{} 的 direct 必须等于注册表声明（IF-D13）",
                declaration.effective_name
            );
        }
    }
    assert_eq!(
        direct_of("mcp__external__Read"),
        Some(false),
        "外部 server 的工具保持 deferred（类型化构造只对注册表声明的 builtin 提升）"
    );
}

/// IF-D10 面②：关闭实例的 bridge 不得进入目录；未关闭实例与外部 server 不受影响。
#[test]
fn closed_builtin_instance_disappears_from_collected_tools() {
    // 关闭 web：两个 web 工具消失，artifact 与外部 server 保留。
    let web_closed = collected_tool_names(&pool_with_builtin_instances(), &["WebMiddleware"]);
    assert!(
        !web_closed
            .iter()
            .any(|name| matches!(name.as_str(), "WebSearch" | "WebFetch")),
        "关闭 WebMiddleware 后 web 实例的工具必须归零: {web_closed:?}"
    );
    assert!(
        web_closed.iter().any(|name| name == "artifact"),
        "另一个实例不受影响: {web_closed:?}"
    );
    assert!(
        web_closed.iter().any(|name| name == "mcp__external__Read"),
        "外部 server 不受关闭影响: {web_closed:?}"
    );

    // 关闭 artifact：只有 artifact 消失。
    let artifact_closed =
        collected_tool_names(&pool_with_builtin_instances(), &["ArtifactMiddleware"]);
    assert!(artifact_closed.iter().any(|name| name == "WebSearch"));
    assert!(
        !artifact_closed.iter().any(|name| name == "artifact"),
        "关闭 ArtifactMiddleware 后 artifact 必须归零: {artifact_closed:?}"
    );

    // 两者都关闭：builtin 工具一个不剩。
    let both_closed = collected_tool_names(
        &pool_with_builtin_instances(),
        &["WebMiddleware", "ArtifactMiddleware"],
    );
    assert!(
        !both_closed
            .iter()
            .any(|name| matches!(name.as_str(), "WebSearch" | "WebFetch" | "artifact")),
        "两个实例都关闭后 builtin 工具必须归零: {both_closed:?}"
    );
    assert!(both_closed.iter().any(|name| name == "mcp__external__Read"));
}

/// 关闭语义必须**只**由注册表 `policy_key` 驱动：未知键不起作用（也不 panic）。
#[test]
fn unknown_policy_key_does_not_close_any_instance() {
    let tools = collected_tool_names(&pool_with_builtin_instances(), &["NotABuiltinPolicyKey"]);
    assert!(tools.iter().any(|name| name == "WebFetch"));
    assert!(tools.iter().any(|name| name == "artifact"));
}

/// 空关闭集与不注入关闭集等价（既有调用点语义逐位不变）。
#[test]
fn empty_closures_keep_every_builtin_tool() {
    let pool = pool_with_builtin_instances();
    let with_empty_closures = collected_tool_names(&pool, &[]);
    let middleware = McpMiddleware::new(Arc::clone(&pool)).with_tool_pool(Arc::clone(&pool));
    let without_closures: Vec<String> = middleware
        .collect_tools("/tmp/contract-test")
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect();
    assert_eq!(with_empty_closures, without_closures);
}

// ══════════════════════════════════════════════════════════════════════════════════
