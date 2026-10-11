//! Tests for mid_mcp

use super::*;
use crate::mcp::{
    client::{status_change_text, McpClientHandle, OAuthStatus},
    ClientStatus,
};
use peri_agent::session::{MessageKind, MessageQueue};

#[test]
fn test_name_returns_mcp_middleware() {
    let pool = Arc::new(McpClientPool::new_empty());
    let mw = McpMiddleware::new(pool);
    let name = <McpMiddleware as Middleware>::name(&mw);
    assert_eq!(name, "McpMiddleware");
}

#[test]
fn test_collect_tools_empty_pool() {
    let pool = Arc::new(McpClientPool::new_empty());
    let mw = McpMiddleware::new(pool);
    let tools = <McpMiddleware as Middleware>::collect_tools(&mw, "/tmp");
    // Resource reader 与 DiscoverMCP 都是 deferred capability；即使初始为空也注册，
    // 使 session-local projected pool 后续 ready 的 resources 可在同一会话使用。
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].name(), "mcp_read_resource");
    assert_eq!(tools[1].name(), "DiscoverMCP");
}

#[test]
fn static_tool_bridges_use_deployment_pool_not_session_projection() {
    let deployment_pool = Arc::new(McpClientPool::new_empty());
    deployment_pool.clients.write().insert(
        "static".to_string(),
        make_connected_handle_with_tool("static", "instantiate_app"),
    );
    let projected_pool = Arc::new(McpClientPool::new_empty());
    projected_pool.clients.write().insert(
        "dynamic".to_string(),
        make_connected_handle_with_tool("dynamic", "shadow_tool"),
    );

    let mw = McpMiddleware::new(Arc::clone(&projected_pool))
        .with_tool_pool(Arc::clone(&deployment_pool));
    let names = <McpMiddleware as Middleware>::collect_tools(&mw, "/tmp")
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect::<Vec<_>>();

    assert!(names.contains(&"mcp__static__instantiate_app".to_string()));
    assert!(!names.contains(&"mcp__dynamic__shadow_tool".to_string()));
}

// ─── first_turn_reminder：首 turn 概览 ───────────────────────────────────────

/// 空池（无任何服务器配置）→ None（零噪音）
#[test]
fn test_overview_empty_pool_returns_none() {
    let pool = Arc::new(McpClientPool::new_empty());
    let mw = McpMiddleware::new(pool);
    assert!(mw.overview_text().is_none());
}

fn make_connected_handle(name: &str, tools: usize) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        name: name.to_string(),
        version: None,
        connected_at: None,
        protocol_version: None,
        cache_version: None,
        peer: None,
        tools: (0..tools).map(|_| rmcp::model::Tool::default()).collect(),
        resources: vec![],
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::default(),
        source: None,
        url: None,
        skills_capable: false,
    })
}

fn make_connected_handle_with_tool(name: &str, tool_name: &str) -> Arc<McpClientHandle> {
    let tool = rmcp::model::Tool::new(
        tool_name.to_string(),
        "fixture".to_string(),
        serde_json::Map::new(),
    );
    Arc::new(McpClientHandle {
        name: name.to_string(),
        version: None,
        connected_at: None,
        protocol_version: None,
        cache_version: None,
        peer: None,
        tools: vec![tool],
        resources: vec![],
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::default(),
        source: None,
        url: None,
        skills_capable: false,
    })
}

/// 混合状态概览：connected 带工具数、failed 带错误、disabled 计数
#[test]
fn test_overview_mixed_statuses() {
    let pool = Arc::new(McpClientPool::new_empty());
    pool.clients
        .write()
        .insert("github".to_string(), make_connected_handle("github", 1));
    pool.clients.write().insert(
        "chrome".to_string(),
        Arc::new(McpClientHandle {
            name: "chrome".to_string(),
            version: None,
            connected_at: None,
            protocol_version: None,
            cache_version: None,
            peer: None,
            tools: vec![],
            resources: vec![],
            status: ClientStatus::Failed("transport closed".to_string()),
            oauth_status: OAuthStatus::default(),
            source: None,
            url: None,
            skills_capable: false,
        }),
    );
    pool.clients.write().insert(
        "legacy".to_string(),
        Arc::new(McpClientHandle {
            name: "legacy".to_string(),
            version: None,
            connected_at: None,
            protocol_version: None,
            cache_version: None,
            peer: None,
            tools: vec![],
            resources: vec![],
            status: ClientStatus::Disabled,
            oauth_status: OAuthStatus::default(),
            source: None,
            url: None,
            skills_capable: false,
        }),
    );
    let mw = McpMiddleware::new(pool);
    let text = mw.overview_text().expect("非空池应生成概览");
    assert!(
        text.contains("MCP: 1 connected, 1 failed, 1 disabled"),
        "概览汇总行: {text}"
    );
    assert!(
        text.contains("- github (connected, 1 tools)"),
        "connected 行: {text}"
    );
    assert!(
        text.contains("- chrome (failed: transport closed)"),
        "failed 行带错误: {text}"
    );
    assert!(text.contains("- legacy (disabled)"), "disabled 行: {text}");
    assert!(text.contains("tool search"), "应提示 tool search 用法");
    assert!(!text.contains("resources"), "概览不含资源信息: {text}");
}

// ─── record_status_change：状态变化统一出口 ──────────────────────────────────

/// 初始化前（initialized=false）：状态变化不产生通知（首 turn 概览覆盖）
#[test]
fn test_record_change_before_initialized_is_silent() {
    let pool = Arc::new(McpClientPool::new_empty());
    pool.clients
        .write()
        .insert("github".to_string(), make_connected_handle("github", 3));
    pool.record_status_change("github", Some(&ClientStatus::Disconnected));
    assert!(
        pool.drain_pending_changes().is_empty(),
        "初始化前不应有通知"
    );
}

/// 初始化后：Connected→Failed 产生"名字 + 错误"通知，恰好一次
#[test]
fn test_record_change_after_initialized_notifies_once() {
    let pool = Arc::new(McpClientPool::new_empty());
    pool.mark_initialized();
    pool.clients
        .write()
        .insert("chrome".to_string(), make_connected_handle("chrome", 0));
    pool.record_status_change("chrome", Some(&ClientStatus::Connected));
    assert!(pool.drain_pending_changes().is_empty(), "同值变化不应通知");

    // 变化：Connected → Failed
    if let Some(h) = pool.clients.write().get_mut("chrome") {
        Arc::make_mut(h).status = ClientStatus::Failed("boom".to_string());
    }
    pool.record_status_change("chrome", Some(&ClientStatus::Connected));
    let changes = pool.drain_pending_changes();
    assert_eq!(changes.len(), 1);
    assert!(
        changes[0].contains("chrome failed: boom"),
        "失败报名字+错误: {}",
        changes[0]
    );

    // drain 恰好一次：再次 drain 为空
    assert!(pool.drain_pending_changes().is_empty());
}

/// 上线通知带工具数（status_change_text 格式）
#[test]
fn test_status_change_text_formats() {
    assert_eq!(
        status_change_text("github", &ClientStatus::Connected, 23),
        "MCP: github connected (23 tools)"
    );
    assert_eq!(
        status_change_text("chrome", &ClientStatus::Failed("x".to_string()), 0),
        "MCP: chrome failed: x"
    );
    assert_eq!(
        status_change_text("legacy", &ClientStatus::Disconnected, 0),
        "MCP: legacy disconnected"
    );
}

/// 旧状态不存在（首次插入）不通知
#[test]
fn test_record_change_without_old_is_silent() {
    let pool = Arc::new(McpClientPool::new_empty());
    pool.mark_initialized();
    pool.clients
        .write()
        .insert("github".to_string(), make_connected_handle("github", 1));
    pool.record_status_change("github", None);
    assert!(pool.drain_pending_changes().is_empty());
}

// ─── before_model：drain 缓冲 → Info 消息推送 ───────────────────────────────

/// 可测试的 MiddlewareState：仅暴露 v2_queue（before_model 只用到它）
struct TestMiddlewareState {
    queue: MessageQueue,
}

impl TestMiddlewareState {
    fn new() -> Self {
        Self {
            queue: MessageQueue::new(),
        }
    }
}

impl peri_agent::middleware::state::MiddlewareState for TestMiddlewareState {
    fn cwd(&self) -> &str {
        "/tmp"
    }
    fn messages(&self) -> &[peri_agent::messages::BaseMessage] {
        &[]
    }
    fn add_message(&mut self, _message: peri_agent::messages::BaseMessage) {}
    fn replace_message(&mut self, _message: peri_agent::messages::BaseMessage) -> bool {
        false
    }
    fn current_step(&self) -> usize {
        0
    }
    fn push_recall(&mut self, _item: String) {}
    fn drain_recall(&mut self) -> Vec<String> {
        vec![]
    }
    fn v2_queue(&self) -> &MessageQueue {
        &self.queue
    }
}

/// before_model：有缓冲变化时 push Info（SystemInjected source）；空缓冲无操作
#[test]
fn test_before_model_pushes_info_messages() {
    let pool = Arc::new(McpClientPool::new_empty());
    pool.mark_initialized();
    let mw = McpMiddleware::new(Arc::clone(&pool));
    let mut state = TestMiddlewareState::new();

    // 空缓冲：无消息
    mw.push_status_changes(&mut state);
    assert!(state.queue.drain_all().is_empty(), "空缓冲不应推送");

    // 两条变化 + 首条附 tool search 提示
    pool.clients
        .write()
        .insert("github".to_string(), make_connected_handle("github", 2));
    pool.record_status_change("github", Some(&ClientStatus::Disconnected));
    if let Some(h) = pool.clients.write().get_mut("github") {
        Arc::make_mut(h).status = ClientStatus::Failed("boom".to_string());
    }
    pool.record_status_change("github", Some(&ClientStatus::Connected));

    mw.push_status_changes(&mut state);
    let drained = state.queue.drain_all();
    let texts: Vec<String> = drained
        .iter()
        .map(|m| match &m.payload {
            peri_agent::session::QueuedPayload::SystemReminder(reminder) => {
                reminder.as_reminder().body.clone()
            }
            other => panic!("expected canonical reminder, got {other:?}"),
        })
        .collect();
    assert_eq!(texts.len(), 3, "提示 + 2 条变化: {texts:?}");
    assert!(
        texts[0].contains("tool search"),
        "首条应附 tool search 提示: {}",
        texts[0]
    );
    assert!(
        texts[1].contains("github connected (2 tools)"),
        "上线行: {}",
        texts[1]
    );
    assert!(
        texts[2].contains("github failed: boom"),
        "失败行: {}",
        texts[2]
    );

    // 缓冲已 drain：再次调用无操作
    mw.push_status_changes(&mut state);
    assert!(state.queue.drain_all().is_empty(), "缓冲恰好一次");

    // 队列内消息均为 canonical Info + Lifecycle/MCP mapping
    for msg in &drained {
        assert_eq!(msg.kind, MessageKind::Info, "必须为 Info（不唤醒循环）");
        assert!(
            matches!(
                msg.source,
                peri_agent::session::MessageSource::SystemInjected
            ),
            "source 应为 SystemInjected"
        );
        let reminder = match &msg.payload {
            peri_agent::session::QueuedPayload::SystemReminder(reminder) => reminder.as_reminder(),
            other => panic!("expected canonical reminder, got {other:?}"),
        };
        assert_eq!(reminder.category, ReminderCategory::Lifecycle);
        assert_eq!(reminder.source.0, "mcp");
        assert_eq!(reminder.kind, "connection_status_changed");
        assert_eq!(reminder.delivery, ReminderDelivery::Configurable);
        assert!(reminder.audiences.contains(ReminderAudience::Model));
    }
}

/// 同一会话实例：tool search 提示仅首条附带
#[test]
fn test_tool_search_hint_once_per_instance() {
    let pool = Arc::new(McpClientPool::new_empty());
    pool.mark_initialized();
    let mw = McpMiddleware::new(Arc::clone(&pool));
    let mut state = TestMiddlewareState::new();

    for round in 0..2 {
        pool.clients
            .write()
            .insert("github".to_string(), make_connected_handle("github", 1));
        pool.record_status_change("github", Some(&ClientStatus::Disconnected));
        mw.push_status_changes(&mut state);
        let texts: Vec<String> = state
            .queue
            .drain_all()
            .iter()
            .map(|m| match &m.payload {
                peri_agent::session::QueuedPayload::SystemReminder(reminder) => {
                    reminder.as_reminder().body.clone()
                }
                other => panic!("expected canonical reminder, got {other:?}"),
            })
            .collect();
        let hint_count = texts.iter().filter(|t| t.contains("tool search")).count();
        assert_eq!(
            hint_count,
            if round == 0 { 1 } else { 0 },
            "第 {} 轮提示次数: {texts:?}",
            round + 1
        );
    }
}

// ─── before_agent：MCP skill 发现投映（验收 7/13/14）────────────────────────

use peri_acp_types::command::command_route::{
    CommandEntryKind, CommandLifecycle, CommandProvenance, CommandSource,
};
use peri_acp_types::mcp_skills::{HandleToken, McpSkillRegistry, ServerDiscoveryState};
use peri_acp_types::skills::SkillMetadata;
use peri_agent::{agent::state::AgentState, agent::AgentCancellationToken};
use rmcp::model::Resource;

fn insert_skill_handle(
    pool: &McpClientPool,
    name: &str,
    resources: Vec<Resource>,
) -> Arc<McpClientHandle> {
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let (reader, mut writer) = tokio::io::split(server_io);
        let mut lines = BufReader::new(reader).lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            if let Some(id) = request.get("id") {
                let result = match request["method"].as_str().unwrap() {
                    "skills/list" => serde_json::json!({"skills": []}),
                    "resources/templates/list" => serde_json::json!({"resourceTemplates": []}),
                    method => panic!("unexpected empty skill fixture request: {method}"),
                };
                let reply = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
                writer
                    .write_all(serde_json::to_string(&reply).unwrap().as_bytes())
                    .await
                    .unwrap();
                writer.write_all(b"\n").await.unwrap();
            }
        }
    });
    let running = rmcp::service::serve_directly::<rmcp::RoleClient, _, _, _, _>(
        rmcp::model::InitializeRequestParams::new(
            rmcp::model::ClientCapabilities::default(),
            rmcp::model::Implementation::from_build_env(),
        ),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let service = crate::mcp::client::McpServiceWrapper::Default(running);
    let service = pool.retain_service(service);
    let peer = service.peer().clone();
    peer.set_peer_info(rmcp::model::ServerPeerInfo::new(
        rmcp::model::ProtocolVersion::default(),
        rmcp::model::ServerCapabilities::default(),
    ));
    pool.services.lock().insert(name.to_string(), service);
    let handle = Arc::new(McpClientHandle {
        name: name.to_string(),
        version: None,
        connected_at: None,
        protocol_version: None,
        cache_version: None,
        peer: Some(peer),
        tools: vec![],
        resources,
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::default(),
        source: None,
        url: None,
        skills_capable: true,
    });
    pool.clients
        .write()
        .insert(name.to_string(), Arc::clone(&handle));
    handle
}

/// 轮询等待真实空清单 peer 的发现任务完成。
async fn wait_discovered(reg: &McpSkillRegistry, server: &str) {
    for _ in 0..200 {
        if matches!(
            reg.discovery_state(server),
            Some(ServerDiscoveryState::Discovered { .. })
        ) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("等待 Discovered 超时: {:?}", reg.discovery_state(server));
}

/// 投影 → Started 置位；同 handle 第二轮不重复 spawn；真实空清单任务完成后
/// 变 Discovered{[]}；全程 state 无消息推送（验收 13 半边）。
#[tokio::test]
async fn before_agent_marks_started_then_completes_silently() {
    let pool = Arc::new(McpClientPool::new_empty());
    let handle = insert_skill_handle(
        &pool,
        "srv",
        vec![Resource::new("skill://demo/SKILL.md", "d")],
    );
    let reg = Arc::new(McpSkillRegistry::new());
    let mw = McpMiddleware::new(Arc::clone(&pool))
        .with_skill_discovery(Some(Arc::clone(&reg)), AgentCancellationToken::new());
    let mut state = AgentState::new("/tmp");

    // 第一轮：同步置 Started（同 handle）
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    let token: HandleToken = handle.clone();
    match reg.discovery_state("srv") {
        Some(ServerDiscoveryState::Started { handle: h }) => {
            assert!(Arc::ptr_eq(&h, &token), "Started 应持 pool 中的 handle");
        }
        other => panic!("应 Started: {other:?}"),
    }
    assert_eq!(state.messages().len(), 0, "before_agent 静默（验收 13）");

    // 第二轮（current_thread runtime：spawn 任务尚未被调度，投影仍见 Started）：
    // 不重复 spawn——状态仍 Started 且 handle 不变
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    match reg.discovery_state("srv") {
        Some(ServerDiscoveryState::Started { handle: h }) => {
            assert!(
                Arc::ptr_eq(&h, &token),
                "不重复 spawn：仍 Started 同 handle"
            );
        }
        other => panic!("应仍 Started: {other:?}"),
    }
    assert_eq!(state.messages().len(), 0);

    // 真实 peer 返回 skills:[] → 发现任务完成后 Discovered{[]}
    wait_discovered(&reg, "srv").await;
    match reg.discovery_state("srv") {
        Some(ServerDiscoveryState::Discovered { entries, .. }) => {
            assert!(entries.is_empty(), "真实 peer 返回合法空清单");
        }
        other => panic!("应 Discovered(空): {other:?}"),
    }
    assert_eq!(state.messages().len(), 0, "发现完成仍静默");
}

/// 断连：pool 条目移除 → before_agent 投影移除 registry 条目并触发
/// on_change（恰好一次）。
#[tokio::test]
async fn before_agent_disconnect_removes_entry_and_fires_on_change() {
    let pool = Arc::new(McpClientPool::new_empty());
    insert_skill_handle(&pool, "srv", vec![]);
    let reg = Arc::new(McpSkillRegistry::new());
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let cb_counter = Arc::clone(&counter);
    reg.set_on_change(Some(Arc::new(move || {
        cb_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    })));
    let mw = McpMiddleware::new(Arc::clone(&pool))
        .with_skill_discovery(Some(Arc::clone(&reg)), AgentCancellationToken::new());
    let mut state = AgentState::new("/tmp");

    Middleware::before_agent(&mw, &mut state).await.unwrap();
    assert!(reg.discovery_state("srv").is_some(), "首轮投影应置位");
    assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 0);

    // 断连：移除 pool 条目 → 投影清理 registry
    pool.clients.write().remove("srv");
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    assert!(
        reg.discovery_state("srv").is_none(),
        "断连后 registry 条目应移除"
    );
    assert_eq!(
        counter.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "断连移除应触发 on_change 恰一次"
    );
    assert_eq!(state.messages().len(), 0, "断连清理静默");
}

/// 重连（新 Arc handle → token 变化）：before_agent 重新置 Started。
#[tokio::test]
async fn before_agent_reconnect_new_handle_rescans() {
    let pool = Arc::new(McpClientPool::new_empty());
    let reg = Arc::new(McpSkillRegistry::new());
    let mw = McpMiddleware::new(Arc::clone(&pool))
        .with_skill_discovery(Some(Arc::clone(&reg)), AgentCancellationToken::new());
    let mut state = AgentState::new("/tmp");

    let h1 = insert_skill_handle(&pool, "srv", vec![]);
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    let h1_token: HandleToken = h1.clone();
    match reg.discovery_state("srv") {
        Some(ServerDiscoveryState::Started { handle }) => {
            assert!(Arc::ptr_eq(&handle, &h1_token));
        }
        other => panic!("应 Started: {other:?}"),
    }

    // 断连移除
    pool.clients.write().remove("srv");
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    assert!(reg.discovery_state("srv").is_none());

    // 重连：新 Arc handle（token 变）→ 重新 Started
    let h2 = insert_skill_handle(&pool, "srv", vec![]);
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    let h2_token: HandleToken = h2.clone();
    match reg.discovery_state("srv") {
        Some(ServerDiscoveryState::Started { handle }) => {
            assert!(Arc::ptr_eq(&handle, &h2_token), "重连后应持新 handle");
            assert!(
                !Arc::ptr_eq(&handle, &h1_token),
                "新 handle 不应与旧 handle 同址"
            );
        }
        other => panic!("应重新 Started: {other:?}"),
    }
    assert_eq!(state.messages().len(), 0);
}

/// cancel token 已触发 → before_agent 零动作（不投影、不置位）。
#[tokio::test]
async fn before_agent_cancelled_token_noop() {
    let pool = Arc::new(McpClientPool::new_empty());
    insert_skill_handle(&pool, "srv", vec![]);
    let reg = Arc::new(McpSkillRegistry::new());
    let cancel = AgentCancellationToken::new();
    cancel.cancel();
    let mw =
        McpMiddleware::new(Arc::clone(&pool)).with_skill_discovery(Some(Arc::clone(&reg)), cancel);
    let mut state = AgentState::new("/tmp");

    Middleware::before_agent(&mw, &mut state).await.unwrap();
    assert!(
        reg.discovery_state("srv").is_none(),
        "cancel 已触发不应置位"
    );
    assert_eq!(state.messages().len(), 0);
}

/// registry 未装配（默认 new()）→ before_agent 直接返回（无发现行为）。
#[tokio::test]
async fn before_agent_without_registry_noop() {
    let pool = Arc::new(McpClientPool::new_empty());
    insert_skill_handle(&pool, "srv", vec![]);
    let mw = McpMiddleware::new(pool);
    let mut state = AgentState::new("/tmp");
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    assert_eq!(state.messages().len(), 0);
}

mod command_tests {
    include!("middleware_command_test.rs");
}

mod system_tests {
    include!("middleware_system_test.rs");
}

mod discovery_state_tests {
    include!("middleware_discovery_state_test.rs");
}

// ─── F11/W5：DiscoverMCP 的 agent 投影随链槽关闭位（真实线路，管道级差分） ──────

/// 经生产管道入口取 DiscoverMCP 工具（`McpMiddleware::collect_tools`，不是直接
/// 构造工具）：关闭位只有经这条路径才会落到 `DiscoverMCPTool` 的 agent registry。
fn discover_tool_from(mw: &McpMiddleware, cwd: &str) -> Box<dyn peri_agent::tools::BaseTool> {
    let mut tools = <McpMiddleware as Middleware>::collect_tools(mw, cwd);
    let index = match tools.iter().position(|tool| tool.name() == "DiscoverMCP") {
        Some(index) => index,
        None => panic!(
            "collect_tools 必须提供 DiscoverMCP: {:?}",
            tools.iter().map(|tool| tool.name()).collect::<Vec<_>>()
        ),
    };
    tools.remove(index)
}

/// 调用 DiscoverMCP 的 JSON-RPC 风格接口（与 `discover_tool_test.rs` 的 invoke 同口径）。
async fn discover_invoke(
    tool: &dyn peri_agent::tools::BaseTool,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    use peri_agent::tools::ToolContext;
    let raw = tool
        .invoke(
            serde_json::json!({ "method": method, "params": params }),
            ToolContext::new(&[], "/tmp"),
        )
        .await
        .expect("invoke 恒 Ok");
    serde_json::from_str(&raw).expect("invoke 输出应为合法 JSON")
}

/// F11/W5 管道级差分：`McpMiddleware::collect_tools` 产出的 DiscoverMCP 的 agent
/// 投影必须与 `SubAgentMiddleware` 链槽关闭位同源（装配面把同一份
/// `meta_harness_disabled` 派生进 `with_sub_agent_face_closed`）。
///
/// - 关闭 ⇒ 本地 agent **不出现**：`list` 的 agents 域为空（无裸名 id），且
///   `search` 不产生 `type: "agent"` 条目、无 `mcp__workspace__*` 远端投影
///   （F8：宿主绑定的 builtin workspace 句柄不得被投影成远端来源）；
/// - 正对照（同一池、同一份磁盘定义，只差关闭位）⇒ 裸名 id 出现——证明真实
///   `workspace` 资源面确实搭上了线路，关闭用例不是「夹具根本没接线」的假绿。
#[tokio::test]
async fn discover_mcp_agent_face_follows_sub_agent_face_closed_bit() {
    use crate::mcp::agent_face_fixture::AgentFaceFixture;

    /// 本地 agent 标识（`.claude/agents/<id>.md` 的 id 段 = 裸名）。
    const LOCAL_AGENT: &str = "pipeline-probe-agent";
    /// 会话标识：绑定后按 ACP 归属过滤（夹具句柄无归属 ⇒ 本会话可见）。
    const SESSION: &str = "pipeline-face-closed";

    let dir = tempfile::tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join(format!("{LOCAL_AGENT}.md")),
        format!("---\nname: {LOCAL_AGENT}\ndescription: Pipeline face probe\n---\n\nProbe.\n"),
    )
    .unwrap();
    // 夹具在 `connect` 时快照 `resources/list`，定义必须先落盘。
    let fixture = AgentFaceFixture::connect(dir.path()).await;
    let cwd = dir.path().to_string_lossy().into_owned();
    let list_agents = serde_json::json!({ "server": "workspace", "domain": "agents" });

    // ── 关闭位（= `meta_harness_disabled` 含 `SUB_AGENT_FACE_CLOSED_KEY`） ──
    let closed = McpMiddleware::new(Arc::clone(&fixture.pool))
        .with_session_id(SESSION)
        .with_sub_agent_face_closed(true);
    let closed_tool = discover_tool_from(&closed, &cwd);
    assert_eq!(
        discover_invoke(&*closed_tool, "list", list_agents.clone()).await,
        serde_json::json!([]),
        "关闭位 ⇒ DiscoverMCP 不得列出任何本地 agent"
    );
    let closed_hits = discover_invoke(
        &*closed_tool,
        "search",
        serde_json::json!({ "query": LOCAL_AGENT }),
    )
    .await;
    assert!(
        !closed_hits
            .as_array()
            .expect("search 应返回数组")
            .iter()
            .any(|entry| entry["type"] == "agent"),
        "关闭位 ⇒ search 不得命中本地 agent（裸名/远端投影皆无）: {closed_hits}"
    );
    assert!(
        !closed_hits.to_string().contains("mcp__workspace__"),
        "关闭位 ⇒ 不得把本地来源投影成远端 agent（F8）: {closed_hits}"
    );

    // ── 正对照：同一个池、同一份磁盘定义，只差关闭位 ──
    let open = McpMiddleware::new(Arc::clone(&fixture.pool))
        .with_session_id(SESSION)
        .with_sub_agent_face_closed(false);
    let open_tool = discover_tool_from(&open, &cwd);
    let open_agents = discover_invoke(&*open_tool, "list", list_agents).await;
    let open_ids: Vec<&str> = open_agents
        .as_array()
        .expect("list 应返回数组")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert!(
        open_ids.contains(&LOCAL_AGENT),
        "正对照：不关闭时必须命中本地 agent（否则关闭用例是假绿）: {open_ids:?}"
    );
    assert!(
        !open_ids.contains(&format!("mcp__workspace__{LOCAL_AGENT}").as_str()),
        "本地来源的 id 保持裸名（F8：builtin workspace 不投影为远端）: {open_ids:?}"
    );
    let open_hits = discover_invoke(
        &*open_tool,
        "search",
        serde_json::json!({ "query": LOCAL_AGENT }),
    )
    .await;
    let open_agent = open_hits
        .as_array()
        .expect("search 应返回数组")
        .iter()
        .find(|entry| entry["type"] == "agent")
        .unwrap_or_else(|| panic!("正对照：search 必须命中 agent 条目: {open_hits}"));
    assert_eq!(open_agent["id"], LOCAL_AGENT, "本地 id 保持裸名");
    assert_eq!(
        open_agent["server"], "workspace",
        "origin = 宿主绑定的实例名"
    );
}
