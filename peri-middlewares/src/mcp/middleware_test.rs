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
        cache_version: None,
        peer: None,
        tools: (0..tools).map(|_| rmcp::model::Tool::default()).collect(),
        resources: vec![],
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::default(),
        source: None,
        url: None,
        skills_capable: false,
        channel_capable: false,
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
        cache_version: None,
        peer: None,
        tools: vec![tool],
        resources: vec![],
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::default(),
        source: None,
        url: None,
        skills_capable: false,
        channel_capable: false,
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
            cache_version: None,
            peer: None,
            tools: vec![],
            resources: vec![],
            status: ClientStatus::Failed("transport closed".to_string()),
            oauth_status: OAuthStatus::default(),
            source: None,
            url: None,
            skills_capable: false,
            channel_capable: false,
        }),
    );
    pool.clients.write().insert(
        "legacy".to_string(),
        Arc::new(McpClientHandle {
            name: "legacy".to_string(),
            version: None,
            cache_version: None,
            peer: None,
            tools: vec![],
            resources: vec![],
            status: ClientStatus::Disabled,
            oauth_status: OAuthStatus::default(),
            source: None,
            url: None,
            skills_capable: false,
            channel_capable: false,
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
    let handle = Arc::new(McpClientHandle {
        name: name.to_string(),
        version: None,
        cache_version: None,
        peer: None,
        tools: vec![],
        resources,
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::default(),
        source: None,
        url: None,
        skills_capable: false,
        channel_capable: false,
    });
    pool.clients
        .write()
        .insert(name.to_string(), Arc::clone(&handle));
    handle
}

/// 轮询等待发现任务完成（peer=None 时任务体无 await，一旦被调度立即完成）。
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

/// 投影 → Started 置位；同 handle 第二轮不重复 spawn；peer=None 任务完成后
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

    // peer=None → 发现任务完成后 Discovered{[]}（失败=空条目，不重试）
    wait_discovered(&reg, "srv").await;
    match reg.discovery_state("srv") {
        Some(ServerDiscoveryState::Discovered { entries, .. }) => {
            assert!(entries.is_empty(), "peer 缺失 → 空条目");
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

// ─── 命令面投影（决策 1：双注册表）───────────────────────────────────────

/// with_command_registry 装配 → before_agent 以 `{server}` 来源键置 Started；
/// 断连 → 命令面按前缀批量注销（removed_any → on_change）。
#[tokio::test]
async fn before_agent_command_registry_projection_and_disconnect() {
    let pool = Arc::new(McpClientPool::new_empty());
    insert_skill_handle(&pool, "srv", vec![]);
    let reg = Arc::new(McpSkillRegistry::new());
    let cmd_reg = Arc::new(CommandRegistry::new());
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let cb_counter = Arc::clone(&counter);
    cmd_reg.set_on_change(Some(Arc::new(move || {
        cb_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    })));
    let mw = McpMiddleware::new(Arc::clone(&pool))
        .with_skill_discovery(Some(Arc::clone(&reg)), AgentCancellationToken::new())
        .with_command_registry(Some(Arc::clone(&cmd_reg)));
    let mut state = AgentState::new("/tmp");

    // 第一轮：命令面 Started（srv 来源登记；注册表无公开 sources 查询，
    // 以断连清理行为 + on_change 侧证接线生效）。
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    assert_eq!(
        counter.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "Started 不触发"
    );

    // 断连：pool 条目移除 → 下轮投影按 srv 前缀清理 → on_change 恰一次
    pool.clients.write().remove("srv");
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    assert_eq!(
        counter.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "断连移除应触发 on_change 恰一次"
    );
    assert!(
        cmd_reg.snapshot().is_empty(),
        "无条目注册（peer 缺失空结果）"
    );
}

/// with_command_registry 未装配（默认 new()）→ 命令面零动作（兼容既有行为）。
#[tokio::test]
async fn before_agent_without_command_registry_noop() {
    let pool = Arc::new(McpClientPool::new_empty());
    insert_skill_handle(&pool, "srv", vec![]);
    let cmd_reg = Arc::new(CommandRegistry::new());
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let cb_counter = Arc::clone(&counter);
    cmd_reg.set_on_change(Some(Arc::new(move || {
        cb_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    })));
    let mw = McpMiddleware::new(Arc::clone(&pool)).with_skill_discovery(
        Some(Arc::new(McpSkillRegistry::new())),
        AgentCancellationToken::new(),
    );
    let mut state = AgentState::new("/tmp");
    Middleware::before_agent(&mw, &mut state).await.unwrap();

    pool.clients.write().remove("srv");
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    assert_eq!(
        counter.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "命令面未装配：注册表零写入"
    );
}

/// 命令面等价断言（对齐 :431 断连清理用例，Phase 6 A5）：断连 →
/// `{server}:` 前缀条目从 snapshot 消失 + on_change 恰一次（决策 1：
/// server 名即词法首段域，无 `mcp:` 域前缀）。
///
/// 发现回写模拟说明：测试 handle 无 rmcp peer（`run_discovery` 立即以空
/// 条目完成），非空条目经 `mcp_route_entries` 转换后手动
/// `mark_source_completed`——与 A3 生产回写同构；先 `wait_discovered` 让
/// spawn 的空回写落定再手动回写，避免空回写注销覆盖。
#[tokio::test]
async fn before_agent_command_registry_disconnect_removes_namespace_and_fires_on_change() {
    let pool = Arc::new(McpClientPool::new_empty());
    let h1 = insert_skill_handle(&pool, "srv", vec![]);
    let reg = Arc::new(McpSkillRegistry::new());
    let cmd_reg = Arc::new(CommandRegistry::new());
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let cb_counter = Arc::clone(&counter);
    cmd_reg.set_on_change(Some(Arc::new(move || {
        cb_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    })));
    let mw = McpMiddleware::new(Arc::clone(&pool))
        .with_skill_discovery(Some(Arc::clone(&reg)), AgentCancellationToken::new())
        .with_command_registry(Some(Arc::clone(&cmd_reg)));
    let mut state = AgentState::new("/tmp");

    // 连接 → 命令面 Started；发现任务（peer=None）空回写落定
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    wait_discovered(&reg, "srv").await;

    // 发现完成回写（A3 转换点同构）：srv:hello 入投影
    let token: HandleToken = h1.clone();
    let added = cmd_reg.mark_source_completed(
        "srv",
        token,
        crate::mcp::skill_discovery::mcp_route_entries(
            &reg,
            "srv",
            &[SkillMetadata {
                name: "mcp__srv__hello".into(),
                aliases: Vec::new(),
                description: "hello skill".into(),
                ..SkillMetadata::default()
            }],
        ),
    );
    assert_eq!(added, 1, "完成回写应注册 1 条");
    assert!(
        cmd_reg.snapshot().iter().any(|e| e.fullname == "srv:hello"),
        "完成回写后 snapshot 应含 srv:hello"
    );
    let before_disconnect = counter.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(before_disconnect, 1, "完成回写应触发 on_change 一次");

    // 断连：pool 条目移除 → 下轮投影按 srv 前缀批量注销 → on_change 恰一次
    pool.clients.write().remove("srv");
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    assert!(
        !cmd_reg.snapshot().iter().any(|e| e.fullname == "srv:hello"),
        "断连后 srv:hello 应从 snapshot 消失"
    );
    assert_eq!(
        counter.load(std::sync::atomic::Ordering::SeqCst),
        before_disconnect + 1,
        "断连注销应触发 on_change 恰一次"
    );
    assert_eq!(state.messages().len(), 0, "断连清理静默");
}

/// 会话预热（决策 B 扩展，审查会话生命周期）：`prewarm_discovery` 不装配
/// chain 即触发幂等发现——新会话（/clear）在首 turn 前命令面即可获得
/// 条目；重复预热（Started/Discovered 去重）零动作、不触发 on_change。
#[tokio::test]
async fn prewarm_discovery_triggers_idempotent_discovery() {
    let pool = Arc::new(McpClientPool::new_empty());
    let h1 = insert_skill_handle(&pool, "srv", vec![]);
    let reg = Arc::new(McpSkillRegistry::new());
    let cmd_reg = Arc::new(CommandRegistry::new());
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let cb_counter = Arc::clone(&counter);
    cmd_reg.set_on_change(Some(Arc::new(move || {
        cb_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    })));

    // 模拟 session/new 路径（无 middleware 实例）：预热 → 发现任务
    // （peer=None）空回写落定。
    prewarm_discovery(&pool, &reg, &cmd_reg, &AgentCancellationToken::new());
    wait_discovered(&reg, "srv").await;

    // 完成回写（A3 转换点同构）：srv:hello 入投影
    let token: HandleToken = h1.clone();
    let added = cmd_reg.mark_source_completed(
        "srv",
        token,
        crate::mcp::skill_discovery::mcp_route_entries(
            &reg,
            "srv",
            &[SkillMetadata {
                name: "mcp__srv__hello".into(),
                aliases: Vec::new(),
                description: "hello skill".into(),
                ..SkillMetadata::default()
            }],
        ),
    );
    assert_eq!(added, 1, "完成回写应注册 1 条");
    assert!(
        cmd_reg.snapshot().iter().any(|e| e.fullname == "srv:hello"),
        "预热后命令面应含 srv:hello（无需 before_agent）"
    );
    let after_first = counter.load(std::sync::atomic::Ordering::SeqCst);

    // 重复预热幂等：已 Discovered → 不重扫、不触发 on_change。
    prewarm_discovery(&pool, &reg, &cmd_reg, &AgentCancellationToken::new());
    prewarm_discovery(&pool, &reg, &cmd_reg, &AgentCancellationToken::new());
    assert_eq!(
        counter.load(std::sync::atomic::Ordering::SeqCst),
        after_first,
        "重复预热零动作"
    );
}

/// 连接完成事件（决策 B，session/new 窗口期）：notifier 挂接后，Connected
/// 状态变化即触发幂等发现——「装配时连接未完成」场景在首 turn 前即被
/// 补偿（无 chain、无 before_agent）。notify_tx=None 仅发现触发。
#[tokio::test]
async fn attach_connection_notifier_triggers_discovery_on_connected() {
    let pool = Arc::new(McpClientPool::new_empty());
    let reg = Arc::new(McpSkillRegistry::new());
    let cmd_reg = Arc::new(CommandRegistry::new());
    attach_connection_notifier(
        &pool,
        Some(&reg),
        Some(&cmd_reg),
        &AgentCancellationToken::new(),
        None,
    );

    // 模拟 session/new 后连接完成：pool 初始化完成 + 状态变更广播
    // （record_status_change → notifier → run_ensure_discovery）。
    // 注意 record_status_change 要求 initialized + old 为 Some 且状态
    // 确实变化（client.rs:854-869），故插入 Connected 前先置旧态。
    pool.mark_initialized();
    let h1 = insert_skill_handle(&pool, "srv", vec![]);
    pool.record_status_change("srv", Some(&ClientStatus::Disconnected));
    wait_discovered(&reg, "srv").await;

    // 完成回写（A3 转换点同构）：连接事件补偿路径下命令面直接可得。
    let token: HandleToken = h1.clone();
    let added = cmd_reg.mark_source_completed(
        "srv",
        token,
        crate::mcp::skill_discovery::mcp_route_entries(
            &reg,
            "srv",
            &[SkillMetadata {
                name: "mcp__srv__hello".into(),
                aliases: Vec::new(),
                description: "hello skill".into(),
                ..SkillMetadata::default()
            }],
        ),
    );
    assert_eq!(added, 1, "连接事件补偿应注册 1 条");
    assert!(
        cmd_reg.snapshot().iter().any(|e| e.fullname == "srv:hello"),
        "连接完成后面板即可路由 srv:hello（无需首 turn）"
    );
}

#[test]
fn test_connection_notifier_does_not_strongly_retain_pool() {
    let pool = Arc::new(McpClientPool::new_empty());
    let pool_weak = Arc::downgrade(&pool);
    let registry = Arc::new(McpSkillRegistry::new());
    let registry_weak = Arc::downgrade(&registry);
    let commands = Arc::new(CommandRegistry::new());
    attach_connection_notifier(
        &pool,
        Some(&registry),
        Some(&commands),
        &AgentCancellationToken::new(),
        None,
    );
    drop(registry);
    drop(commands);

    drop(pool);

    assert!(pool_weak.upgrade().is_none());
    assert!(registry_weak.upgrade().is_none());
}

/// 初始连接补发（决策 B 扩展）：`run_initialize` 收口时
/// `notify_initial_connections` 为每个 Connected server 补发一次连接
/// 通知——初始化期间的连接事件不经过 `record_status_change`
/// （`run_initialize` 直接插入 Connected handle），连接事件 notifier
/// 需靠收口补发驱动「刚进入、未说话」时的 skill 发现。
#[tokio::test]
async fn notify_initial_connections_triggers_discovery_on_startup() {
    let pool = Arc::new(McpClientPool::new_empty());
    let reg = Arc::new(McpSkillRegistry::new());
    let cmd_reg = Arc::new(CommandRegistry::new());
    attach_connection_notifier(
        &pool,
        Some(&reg),
        Some(&cmd_reg),
        &AgentCancellationToken::new(),
        None,
    );

    // 模拟 run_initialize：直接插入 Connected handle（不调用
    // record_status_change——初始连接事件不产生通知）。
    let h1 = insert_skill_handle(&pool, "srv", vec![]);
    pool.notify_initial_connections();
    wait_discovered(&reg, "srv").await;

    // 完成回写（A3 转换点同构）：补发路径下命令面直接可得。
    let token: HandleToken = h1.clone();
    let added = cmd_reg.mark_source_completed(
        "srv",
        token,
        crate::mcp::skill_discovery::mcp_route_entries(
            &reg,
            "srv",
            &[SkillMetadata {
                name: "mcp__srv__hello".into(),
                aliases: Vec::new(),
                description: "hello skill".into(),
                ..SkillMetadata::default()
            }],
        ),
    );
    assert_eq!(added, 1, "初始化补发应注册 1 条");
    assert!(
        cmd_reg.snapshot().iter().any(|e| e.fullname == "srv:hello"),
        "补发后面板即可路由 srv:hello（无需首 turn）"
    );
}

/// 重连顺序性（Phase 6 A5 验收核心）：连接 → 发现 → 注册（投影含
/// `demo:hello`）→ 断连（投影收缩）→ 重连（新 handle）→ 重扫完成前
/// 投影**不含**新条目（`Started → Discovered` 不占位）→ 完成回写（投影
/// 复现 + `resolve` 路由一致）；旧任务回写（旧 handle）被 ptr_eq 拒绝
/// （无 ABA）。
///
/// 驱动形态：连接 / 断连 / 重连经 `before_agent` 投影（`project_sources` →
/// `mark_source_started`），发现回写经 `mcp_route_entries` 转换后手动
/// `mark_source_completed`（测试 handle 无 peer，spawn 任务只能产出空
/// 条目）；每轮先 `wait_discovered` 让 spawn 空回写落定再手动回写。
#[tokio::test]
async fn before_agent_command_registry_reconnect_sequence_no_aba() {
    let pool = Arc::new(McpClientPool::new_empty());
    let reg = Arc::new(McpSkillRegistry::new());
    let cmd_reg = Arc::new(CommandRegistry::new());
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let cb_counter = Arc::clone(&counter);
    cmd_reg.set_on_change(Some(Arc::new(move || {
        cb_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    })));
    let mw = McpMiddleware::new(Arc::clone(&pool))
        .with_skill_discovery(Some(Arc::clone(&reg)), AgentCancellationToken::new())
        .with_command_registry(Some(Arc::clone(&cmd_reg)));
    let mut state = AgentState::new("/tmp");

    // A3 转换点同构的回写载荷（skill 名剥 `mcp__demo__` 前缀 → demo:hello）
    let route_entries = || {
        crate::mcp::skill_discovery::mcp_route_entries(
            &reg,
            "demo",
            &[SkillMetadata {
                name: "mcp__demo__hello".into(),
                aliases: Vec::new(),
                description: "hello skill".into(),
                ..SkillMetadata::default()
            }],
        )
    };
    let snapshot_has_hello =
        |reg: &CommandRegistry| reg.snapshot().iter().any(|e| e.fullname == "demo:hello");

    // 1) 连接 → 命令面 Started；发现任务（peer=None）空回写落定
    let h1 = insert_skill_handle(&pool, "demo", vec![]);
    let token1: HandleToken = h1.clone();
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    wait_discovered(&reg, "demo").await;

    // 2) 发现完成回写 → 投影含 demo:hello；resolve 路由一致
    assert_eq!(
        cmd_reg.mark_source_completed("demo", token1.clone(), route_entries()),
        1,
        "首次完成回写应注册 1 条"
    );
    assert!(
        snapshot_has_hello(&cmd_reg),
        "完成回写后投影应含 demo:hello"
    );
    let r1 = cmd_reg.resolve("demo:hello").expect("resolve 应命中");
    assert_eq!(r1.entry.fullname, "demo:hello");
    assert_eq!(r1.entry.kind, CommandEntryKind::McpSkill);
    assert_eq!(r1.entry.description, "hello skill");
    assert_eq!(
        r1.entry.provenance,
        CommandProvenance {
            source: CommandSource::Mcp {
                server: "demo".into()
            },
            lifecycle: CommandLifecycle::Discovered,
        },
        "路由条目 provenance 应与 mcp_route_entries 产出一致"
    );
    assert_eq!(r1.args, "");

    // 3) 断连 → 投影收缩（demo:hello 从 snapshot 消失）
    pool.clients.write().remove("demo");
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    assert!(!snapshot_has_hello(&cmd_reg), "断连后投影应收缩");

    // 4) 重连（新 Arc handle → token 变化）→ 重扫完成前投影不含新条目
    //    （Started → Discovered 不占位；current_thread：spawn 尚未调度）
    let h2 = insert_skill_handle(&pool, "demo", vec![]);
    let token2: HandleToken = h2.clone();
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    assert!(
        !Arc::ptr_eq(&token1, &token2),
        "重连 handle 必须新址（防 ABA 前提；旧 token 由测试持有保持分配存活）"
    );
    assert!(
        !snapshot_has_hello(&cmd_reg),
        "重扫完成前不得占位注册（Started → Discovered 不占位）"
    );

    // 5) 重扫（peer=None 空回写）落定后，新 handle 回写 → 投影复现 + 路由一致
    wait_discovered(&reg, "demo").await;
    assert_eq!(
        cmd_reg.mark_source_completed("demo", token2.clone(), route_entries()),
        1,
        "重连完成回写应注册 1 条"
    );
    assert!(
        snapshot_has_hello(&cmd_reg),
        "重连完成回写后投影应复现 demo:hello"
    );
    let r2 = cmd_reg
        .resolve("demo:hello")
        .expect("重连后 resolve 应命中");
    assert_eq!(r2.entry.fullname, "demo:hello");
    assert_eq!(r2.entry.kind, CommandEntryKind::McpSkill);

    // 6) 旧任务回写（旧 handle token1）被 ptr_eq 拒绝：不注册、不覆盖、不触发
    let before_stale = counter.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        cmd_reg.mark_source_completed("demo", token1, route_entries()),
        0,
        "旧 handle 回写应被 ptr_eq 拒绝（无 ABA）"
    );
    assert!(snapshot_has_hello(&cmd_reg), "旧回写不得清除/替换新条目");
    assert_eq!(
        counter.load(std::sync::atomic::Ordering::SeqCst),
        before_stale,
        "旧任务回写不得触发 on_change"
    );
    assert_eq!(state.messages().len(), 0);
}

/// P1-1 回归：plugin 提供的 MCP server key（`plugin:p1:demosrv`，含冒号）
/// 命令面来源键统一为末段 `demosrv`（决策 1：与 fullname 首段同构）——
/// 断连按 `demosrv:` 前缀批量注销（幽灵条目不残留），重连复现无
/// Conflict（验收 :414/:415 在 plugin server 形态下成立）。
#[tokio::test]
async fn before_agent_command_registry_plugin_server_disconnect_reconnect() {
    let pool = Arc::new(McpClientPool::new_empty());
    let reg = Arc::new(McpSkillRegistry::new());
    let cmd_reg = Arc::new(CommandRegistry::new());
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let cb_counter = Arc::clone(&counter);
    cmd_reg.set_on_change(Some(Arc::new(move || {
        cb_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    })));
    let mw = McpMiddleware::new(Arc::clone(&pool))
        .with_skill_discovery(Some(Arc::clone(&reg)), AgentCancellationToken::new())
        .with_command_registry(Some(Arc::clone(&cmd_reg)));
    let mut state = AgentState::new("/tmp");
    let server = "plugin:p1:demosrv";

    // 来源键派生断言：三处键（来源登记 / 注销前缀 / fullname 首段）必须
    // 同构（旧实现用 `mcp:plugin:p1:demosrv`，注销前缀匹配不到 fullname
    // `demosrv:beta` → 幽灵条目）。
    assert_eq!(
        crate::mcp::skill_discovery::mcp_source_key(server),
        "demosrv",
        "plugin server 来源键必须取末段（P1-1）"
    );
    let route_entries = || {
        crate::mcp::skill_discovery::mcp_route_entries(
            &reg,
            server,
            &[SkillMetadata {
                name: "mcp__plugin:p1:demosrv__beta".into(),
                aliases: Vec::new(),
                description: "beta skill".into(),
                ..SkillMetadata::default()
            }],
        )
    };
    assert_eq!(route_entries()[0].fullname, "demosrv:beta");
    let snapshot_has_beta =
        |reg: &CommandRegistry| reg.snapshot().iter().any(|e| e.fullname == "demosrv:beta");

    // 1) 连接 → 命令面以末段来源键置 Started；发现任务（peer=None）空回写落定
    let h1 = insert_skill_handle(&pool, server, vec![]);
    let token1: HandleToken = h1.clone();
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    wait_discovered(&reg, server).await;

    // 2) 完成回写（A3 转换点同构，来源键 = demosrv）→ 投影含条目
    assert_eq!(
        cmd_reg.mark_source_completed(
            &crate::mcp::skill_discovery::mcp_source_key(server),
            token1.clone(),
            route_entries(),
        ),
        1,
        "完成回写应注册 1 条（末段来源键）"
    );
    assert!(
        snapshot_has_beta(&cmd_reg),
        "完成回写后投影应含 demosrv:beta"
    );

    // 3) 断连 → 按 demosrv: 前缀批量注销，幽灵条目不残留
    pool.clients.write().remove(server);
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    assert!(
        !snapshot_has_beta(&cmd_reg),
        "断连后 demosrv:beta 必须收缩（P1-1 幽灵条目回归）"
    );

    // 4) 重连（新 Arc handle → token 变化）→ 重扫落定 → 新 token 回写复现
    //    （幽灵条目残留时同键重注册 → Conflict 纯拒绝 → added=0）
    let h2 = insert_skill_handle(&pool, server, vec![]);
    let token2: HandleToken = h2.clone();
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    wait_discovered(&reg, server).await;
    assert_eq!(
        cmd_reg.mark_source_completed(
            &crate::mcp::skill_discovery::mcp_source_key(server),
            token2.clone(),
            route_entries(),
        ),
        1,
        "重连完成回写应注册 1 条（幽灵条目残留时此处 Conflict → 0）"
    );
    assert!(snapshot_has_beta(&cmd_reg), "重连后投影复现 demosrv:beta");
    let r = cmd_reg
        .resolve("demosrv:beta")
        .expect("重连后 resolve 应命中");
    assert_eq!(r.entry.kind, CommandEntryKind::McpSkill);
    assert_eq!(state.messages().len(), 0, "断连/重连清理静默");
}

// ─── System MCP 启动闸门（before_react_start，IF-M3 / IF-M4）──────────────────
//
// 夹具策略：只把外部对端换成内存 JSON-RPC 假 server，客户端侧走真实
// `serve_client_auto`（真实 rmcp lifecycle / peer_info / transport 关闭语义）；
// 配置清单与本代发现证据由测试显式发布，等价于 B-02 在 initialize / reconnect /
// OAuth 路径上的提交点。既有 `peer: None` + 手工 `Connected` 的夹具在闸门用例里
// **不构成 ready 证据**，只用于负向断言。
//
// 断言范围：本文件覆盖 crate 内可观察层（闸门返回值、候选、bridge 分类、收集
// 结果）。首个 LLM 请求的 tools 入参与宿主终态由 B-07 在 `peri-acp` host seam
// 承担（主 plan §5 R9）。

use crate::mcp::apps::McpCapabilityProfile;
use crate::mcp::client::DiscoveryEvidence;
use peri_acp_types::plugin::McpServerConfig;
use rmcp::{service::RoleClient, transport::async_rw::AsyncRwTransport};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

/// 闸门状态探针：候选只经 `StartupState` 传递，不落 middleware 内部字段。
#[derive(Default)]
struct StartupGateProbe {
    staged: Option<StartupToolUpdate>,
}

impl hook_state::StartupState for StartupGateProbe {
    fn set_active_middleware(&mut self, _middleware_name: &str) {}

    fn stage_startup_tools(
        &mut self,
        update: StartupToolUpdate,
    ) -> peri_agent::error::AgentResult<()> {
        assert!(self.staged.is_none(), "一次准入只允许登记一个候选");
        self.staged = Some(update);
        Ok(())
    }

    fn take_startup_tools(&mut self) -> Option<StartupToolUpdate> {
        self.staged.take()
    }
}

type GateTransport = AsyncRwTransport<RoleClient, ReadHalf<DuplexStream>, WriteHalf<DuplexStream>>;

fn gate_transport(client: DuplexStream) -> GateTransport {
    let (read, write) = tokio::io::split(client);
    AsyncRwTransport::new(read, write)
}

/// 假 MCP 对端：initialize 成功；其余请求（含 `server/discover`）回
/// Method not found，驱动 Auto 生命周期回退 legacy initialize。通知无 id，不回响应。
fn spawn_gate_peer(server: DuplexStream) -> tokio::task::JoinHandle<()> {
    let (server_read, mut server_write) = tokio::io::split(server);
    tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(request) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            let response = match request["method"].as_str() {
                Some("initialize") => serde_json::json!({
                    "jsonrpc": "2.0", "id": request["id"], "result": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "serverInfo": { "name": "mcp-gate-fixture", "version": "1" }
                    }
                }),
                _ if request["id"].is_null() => continue,
                _ => serde_json::json!({
                    "jsonrpc": "2.0", "id": request["id"],
                    "error": { "code": -32601, "message": "Method not found" }
                }),
            };
            if server_write
                .write_all(format!("{response}\n").as_bytes())
                .await
                .is_err()
            {
                break;
            }
            if server_write.flush().await.is_err() {
                break;
            }
        }
    })
}

fn system_config(required_tools: Option<Vec<String>>, timeout_ms: Option<u64>) -> McpServerConfig {
    McpServerConfig {
        command: Some("mcp-gate-fixture".to_string()),
        args: None,
        env: None,
        url: None,
        headers: None,
        oauth: None,
        disabled: None,
        protocol_version: None,
        subscriptions: None,
        system_mcp: Some(true),
        system_mcp_tools: required_tools,
        system_mcp_timeout: timeout_ms,
        source: None,
    }
}

fn ordinary_config() -> McpServerConfig {
    McpServerConfig {
        system_mcp: None,
        system_mcp_tools: None,
        system_mcp_timeout: None,
        ..system_config(None, None)
    }
}

fn fixture_tool(name: &str, input_schema: serde_json::Value) -> rmcp::model::Tool {
    serde_json::from_value(serde_json::json!({
        "name": name,
        "description": "fixture",
        "inputSchema": input_schema
    }))
    .expect("fixture tool 必须能被 rmcp Tool 接收")
}

fn read_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": { "path": { "type": "string" } },
        "required": ["path"]
    })
}

struct GateFixture {
    pool: Arc<McpClientPool>,
    servers: Vec<tokio::task::JoinHandle<()>>,
}

impl GateFixture {
    fn new() -> Self {
        Self {
            pool: Arc::new(McpClientPool::new_pending()),
            servers: Vec::new(),
        }
    }

    fn pool(&self) -> &Arc<McpClientPool> {
        &self.pool
    }

    fn config(&self, name: &str, config: McpServerConfig) {
        self.pool.configs.write().insert(name.to_string(), config);
    }

    fn publish_loaded(&self) {
        self.pool.publish_system_manifest(SystemMcpManifest::Loaded);
    }

    /// 真实握手并提交连接；返回（句柄，已登记代际）。
    async fn connect(
        &mut self,
        name: &str,
        tools: Vec<rmcp::model::Tool>,
    ) -> (Arc<McpClientHandle>, u64) {
        let (client, server) = tokio::io::duplex(4096);
        self.servers.push(spawn_gate_peer(server));
        let service = crate::mcp::client::serve_client_auto(
            gate_transport(client),
            None,
            None,
            &McpCapabilityProfile::default(),
            Duration::from_secs(5),
        )
        .await
        .expect("fixture 握手不得超时")
        .expect("fixture 握手不得失败");
        let service = self.pool.retain_service(service);
        let peer = service.peer().clone();
        let handle = Arc::new(McpClientHandle {
            name: name.to_string(),
            version: None,
            cache_version: None,
            peer: Some(peer),
            tools,
            resources: vec![],
            status: ClientStatus::Connected,
            oauth_status: OAuthStatus::default(),
            source: None,
            url: None,
            skills_capable: false,
            channel_capable: false,
        });
        assert!(
            handle
                .peer
                .as_ref()
                .and_then(|peer| peer.peer_info())
                .is_some(),
            "fixture 必须完成真实 peer_info 协商"
        );
        assert!(
            self.pool
                .try_commit_connection(name.to_string(), Arc::clone(&handle), service)
                .is_ok(),
            "fixture 连接必须被 pool 接受"
        );
        let generation = self.pool.handle_generation(&handle);
        (handle, generation)
    }

    /// 使某台 server 成为「本代发现完成」：清单已完整发布 + 本代成功证据。
    fn ready(&self, name: &str, generation: u64) {
        self.publish_loaded();
        self.pool
            .commit_discovery_evidence(name, DiscoveryEvidence::discovered(generation));
    }

    async fn shutdown(self) {
        self.pool.begin_shutdown();
        let _ = self.pool.shutdown().await;
        for task in self.servers {
            tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .expect("假 server 必须随连接关闭退出")
                .expect("假 server 任务不得 panic");
        }
    }
}

fn bridge_names(tools: &[std::sync::Arc<dyn BaseTool>]) -> Vec<(String, bool)> {
    let mut names: Vec<(String, bool)> = tools
        .iter()
        .map(|tool| (tool.name().to_string(), tool.is_direct()))
        .collect();
    names.sort();
    names
}

/// 就绪后闸门放行并暂存**整批**静态 bridge：必需项 direct、其余 deferred，
/// 且候选只取自 deployment `tool_pool`（session projection 的伪 Connected 不参与）。
#[tokio::test]
async fn system_mcp_ready_stages_candidate_with_direct_required_tool() {
    let mut fixture = GateFixture::new();
    fixture.config("sys", system_config(Some(vec!["Read".to_string()]), None));
    let (_, generation) = fixture
        .connect(
            "sys",
            vec![
                fixture_tool("Read", read_schema()),
                fixture_tool("Glob", read_schema()),
            ],
        )
        .await;
    fixture.ready("sys", generation);

    let projection = Arc::new(McpClientPool::new_empty());
    projection.clients.write().insert(
        "dyn".to_string(),
        make_connected_handle_with_tool("dyn", "shadow"),
    );
    let mw = McpMiddleware::new(Arc::clone(&projection)).with_tool_pool(Arc::clone(fixture.pool()));

    let mut probe = StartupGateProbe::default();
    Middleware::before_react_start(&mw, &mut probe)
        .await
        .expect("ready 后闸门必须放行");

    let update = probe.staged.expect("System 依赖就绪必须暂存候选");
    assert_eq!(
        bridge_names(&update.tools),
        vec![
            ("mcp__sys__Glob".to_string(), false),
            ("mcp__sys__Read".to_string(), true),
        ],
        "整批静态 bridge：仅必需项 direct，投影池工具不得进入候选"
    );
    assert_eq!(
        update.required,
        vec![StartupRequiredTool {
            server_name: "sys".to_string(),
            original_tool_name: "Read".to_string(),
            effective_tool_name: "mcp__sys__Read".to_string(),
        }]
    );

    fixture.shutdown().await;
}

/// 必需工具缺失：闸门 fatal，不暂存候选（不发布 ready、不注入部分 direct）。
#[tokio::test]
async fn system_mcp_missing_required_tool_blocks_startup() {
    let mut fixture = GateFixture::new();
    fixture.config("sys", system_config(Some(vec!["Read".to_string()]), None));
    let (_, generation) = fixture
        .connect("sys", vec![fixture_tool("Write", read_schema())])
        .await;
    fixture.ready("sys", generation);

    let mw = McpMiddleware::new(Arc::clone(fixture.pool()));
    let mut probe = StartupGateProbe::default();
    let error = Middleware::before_react_start(&mw, &mut probe)
        .await
        .expect_err("缺必需工具必须阻止启动");

    match error {
        peri_agent::error::AgentError::MiddlewareError {
            ref middleware,
            ref reason,
        } => {
            assert_eq!(middleware, "McpMiddleware");
            assert!(
                reason.contains("未提供必需工具") && reason.contains("Read"),
                "错误需明确工具类别: {reason}"
            );
        }
        other => panic!("必须是 fatal MiddlewareError，实际 {other:?}"),
    }
    assert!(probe.staged.is_none(), "失败不得暂存候选");

    fixture.shutdown().await;
}

/// 必需工具 schema 结构非法：闸门 fatal，不暂存候选。
#[tokio::test]
async fn system_mcp_invalid_schema_blocks_startup() {
    let mut fixture = GateFixture::new();
    fixture.config("sys", system_config(Some(vec!["Read".to_string()]), None));
    let (_, generation) = fixture
        .connect(
            "sys",
            vec![fixture_tool(
                "Read",
                serde_json::json!({ "type": "object", "properties": 42 }),
            )],
        )
        .await;
    fixture.ready("sys", generation);

    let mw = McpMiddleware::new(Arc::clone(fixture.pool()));
    let mut probe = StartupGateProbe::default();
    let error = Middleware::before_react_start(&mw, &mut probe)
        .await
        .expect_err("非法 schema 必须阻止启动");

    assert!(
        matches!(
            &error,
            peri_agent::error::AgentError::MiddlewareError { reason, .. }
                if reason.contains("input schema 结构非法")
        ),
        "期望 InvalidSchema 投影: {error:?}"
    );
    assert!(probe.staged.is_none(), "失败不得暂存候选");

    fixture.shutdown().await;
}

/// 空数组契约 4：只验证 ready，不新增 direct 工具（普通工具仍 deferred）。
#[tokio::test]
async fn system_mcp_empty_required_array_adds_no_direct_tools() {
    let mut fixture = GateFixture::new();
    fixture.config("sys", system_config(Some(vec![]), None));
    let (_, generation) = fixture
        .connect("sys", vec![fixture_tool("Read", read_schema())])
        .await;
    fixture.ready("sys", generation);

    let mw = McpMiddleware::new(Arc::clone(fixture.pool()));
    let mut probe = StartupGateProbe::default();
    Middleware::before_react_start(&mw, &mut probe)
        .await
        .expect("空数组仍应等待并放行");

    let update = probe.staged.expect("System 依赖就绪必须暂存候选");
    assert!(update.required.is_empty(), "空数组不得产生必需工具身份");
    assert_eq!(
        bridge_names(&update.tools),
        vec![("mcp__sys__Read".to_string(), false)],
        "direct 增量为 0，且普通 deferred 工具不被删除"
    );

    fixture.shutdown().await;
}

/// 空数组仍必须完成 initialize / tools/list：从未连接 → timeout fatal。
#[tokio::test]
async fn system_mcp_empty_required_array_still_requires_discovery() {
    let fixture = GateFixture::new();
    fixture.config("sys", system_config(Some(vec![]), Some(1)));
    fixture.publish_loaded();

    let mw = McpMiddleware::new(Arc::clone(fixture.pool()));
    let mut probe = StartupGateProbe::default();
    let error = Middleware::before_react_start(&mw, &mut probe)
        .await
        .expect_err("未经 discovery 的 System server 不得放行");

    assert!(
        matches!(
            &error,
            peri_agent::error::AgentError::MiddlewareError { reason, .. }
                if reason.contains("启动超时") && reason.contains("sys")
        ),
        "期望 timeout fatal: {error:?}"
    );
    assert!(probe.staged.is_none());

    fixture.shutdown().await;
}

/// 无 System 依赖（缺省 / 显式 false）：零动作、不等待、不产生 startup update。
#[tokio::test]
async fn system_mcp_absent_or_false_does_not_block_startup() {
    for config in [
        ordinary_config(),
        McpServerConfig {
            system_mcp: Some(false),
            ..system_config(None, None)
        },
    ] {
        let fixture = GateFixture::new();
        fixture.config("plain", config);
        fixture.publish_loaded();

        let mw = McpMiddleware::new(Arc::clone(fixture.pool()));
        let mut probe = StartupGateProbe::default();
        Middleware::before_react_start(&mw, &mut probe)
            .await
            .expect("普通 MCP 的 pending/failed 永不阻塞启动");
        assert!(
            probe.staged.is_none(),
            "无 System 依赖不产生 startup update"
        );

        fixture.shutdown().await;
    }
}

/// 取消 → `Interrupted`（不是 fatal）；不暂存候选。
#[tokio::test]
async fn system_mcp_cancelled_startup_is_interrupted() {
    let fixture = GateFixture::new();
    fixture.config("sys", system_config(Some(vec!["Read".to_string()]), None));
    fixture.publish_loaded();
    let cancel = AgentCancellationToken::new();
    cancel.cancel();
    let mw = McpMiddleware::new(Arc::clone(fixture.pool())).with_skill_discovery(None, cancel);

    let mut probe = StartupGateProbe::default();
    let error = Middleware::before_react_start(&mw, &mut probe)
        .await
        .expect_err("取消必须中断本次启动");

    assert!(
        matches!(error, peri_agent::error::AgentError::Interrupted),
        "Cancelled 必须映射 Interrupted: {error:?}"
    );
    assert!(probe.staged.is_none());

    fixture.shutdown().await;
}

/// timeout **不是**取消：映射 fatal `MiddlewareError`，不映射 `Interrupted`。
#[tokio::test]
async fn system_mcp_timeout_is_fatal() {
    let fixture = GateFixture::new();
    fixture.config(
        "sys",
        system_config(Some(vec!["Read".to_string()]), Some(1)),
    );
    fixture.publish_loaded();
    let mw = McpMiddleware::new(Arc::clone(fixture.pool()));

    let mut probe = StartupGateProbe::default();
    let error = Middleware::before_react_start(&mw, &mut probe)
        .await
        .expect_err("超时必须阻止启动");

    assert!(
        !matches!(error, peri_agent::error::AgentError::Interrupted),
        "timeout 不得映射 Interrupted"
    );
    assert!(
        matches!(
            &error,
            peri_agent::error::AgentError::MiddlewareError { reason, .. }
                if reason.contains("启动超时（1ms）")
        ),
        "期望超时固定文案: {error:?}"
    );

    fixture.shutdown().await;
}

/// 准入事实源是 deployment `tool_pool`：session projection 的连接证据不得放行。
///
/// 投影池刻意用**同名** server 且伪 `Connected`：若闸门误读投影池，得到的会是
/// `NegotiationIncomplete`（无真实协议证据）而不是 deployment 侧的 Timeout。
#[tokio::test]
async fn system_mcp_gate_uses_deployment_pool_not_session_projection() {
    let fixture = GateFixture::new();
    fixture.config(
        "sys",
        system_config(Some(vec!["Read".to_string()]), Some(1)),
    );
    fixture.publish_loaded();

    let projection = Arc::new(McpClientPool::new_empty());
    projection.clients.write().insert(
        "sys".to_string(),
        make_connected_handle_with_tool("sys", "Read"),
    );
    let mw = McpMiddleware::new(Arc::clone(&projection)).with_tool_pool(Arc::clone(fixture.pool()));

    let mut probe = StartupGateProbe::default();
    let error = Middleware::before_react_start(&mw, &mut probe)
        .await
        .expect_err("投影池的伪 Connected 不得作为准入证据");

    assert!(
        matches!(
            &error,
            peri_agent::error::AgentError::MiddlewareError { reason, .. }
                if reason.contains("启动超时（1ms）")
        ),
        "必须按 deployment 侧事实判定（无连接 → 超时）: {error:?}"
    );
    assert!(probe.staged.is_none());

    fixture.shutdown().await;
}

/// 错误文案安全：控制字符折叠为空格、凭据形态遮蔽（危险形态只用非真实凭据形状）。
#[tokio::test]
async fn startup_error_text_folds_control_chars_and_redacts_credentials() {
    let mut fixture = GateFixture::new();
    fixture.config(
        "sys",
        system_config(
            Some(vec!["Re\u{7}ad?token=FAKE-SHAPE-ONLY".to_string()]),
            None,
        ),
    );
    let (_, generation) = fixture
        .connect("sys", vec![fixture_tool("Read", read_schema())])
        .await;
    fixture.ready("sys", generation);

    let mw = McpMiddleware::new(Arc::clone(fixture.pool()));
    let mut probe = StartupGateProbe::default();
    let error = Middleware::before_react_start(&mw, &mut probe)
        .await
        .expect_err("必需工具缺失");

    let peri_agent::error::AgentError::MiddlewareError { reason, .. } = &error else {
        panic!("必须是 MiddlewareError: {error:?}");
    };
    assert!(
        !reason.chars().any(char::is_control),
        "控制字符必须折叠: {reason}"
    );
    assert!(
        !reason.contains("FAKE-SHAPE-ONLY"),
        "凭据形态必须遮蔽: {reason}"
    );
    assert!(
        reason.contains("未提供必需工具"),
        "错误类别仍需可见: {reason}"
    );

    fixture.shutdown().await;
}

// ─── collect_tools 的整批替换与 deferred 回退 ────────────────────────────────

/// 就绪后收集：prepared 整批**替换**初始 Vec，必需项 direct 且只注册一次；
/// resource / discover 仍原样追加。
#[tokio::test]
async fn collect_tools_replaces_initial_bridges_without_duplicate_registration() {
    let mut fixture = GateFixture::new();
    fixture.config("sys", system_config(Some(vec!["Read".to_string()]), None));
    fixture.config("aux", ordinary_config());
    let (_, generation) = fixture
        .connect(
            "sys",
            vec![
                fixture_tool("Read", read_schema()),
                fixture_tool("Glob", read_schema()),
            ],
        )
        .await;
    fixture
        .connect("aux", vec![fixture_tool("Write", read_schema())])
        .await;
    fixture.ready("sys", generation);

    let mw = McpMiddleware::new(Arc::clone(fixture.pool()));
    let collected = <McpMiddleware as Middleware>::collect_tools(&mw, "/tmp");
    let names: Vec<String> = collected
        .iter()
        .map(|tool| tool.name().to_string())
        .collect();

    assert_eq!(
        &names[names.len() - 2..],
        ["mcp_read_resource", "DiscoverMCP"],
        "resource / discover 仍原样追加: {names:?}"
    );
    let bridges = &collected[..names.len() - 2];
    assert_eq!(bridges.len(), 3, "整批静态 bridge 恰好一次: {names:?}");
    for name in ["mcp__sys__Read", "mcp__sys__Glob", "mcp__aux__Write"] {
        assert_eq!(
            bridges.iter().filter(|tool| tool.name() == name).count(),
            1,
            "{name} 不得重复注册: {names:?}"
        );
    }
    let read = bridges
        .iter()
        .find(|tool| tool.name() == "mcp__sys__Read")
        .expect("必需 bridge 必须在集合内");
    assert!(read.is_direct(), "必需项 direct");
    assert!(
        bridges
            .iter()
            .filter(|tool| tool.name() != "mcp__sys__Read")
            .all(|tool| !tool.is_direct()),
        "非必需项保持 deferred"
    );

    fixture.shutdown().await;
}

/// 配置清单未发布（Pending）：`configs` 不是可信依赖事实源，不得提升 direct。
#[tokio::test]
async fn collect_tools_keeps_deferred_bridges_until_manifest_loaded() {
    let mut fixture = GateFixture::new();
    fixture.config("sys", system_config(Some(vec!["Read".to_string()]), None));
    fixture
        .connect("sys", vec![fixture_tool("Read", read_schema())])
        .await;

    let mw = McpMiddleware::new(Arc::clone(fixture.pool()));
    let collected = <McpMiddleware as Middleware>::collect_tools(&mw, "/tmp");
    let read = collected
        .iter()
        .find(|tool| tool.name() == "mcp__sys__Read")
        .expect("deferred bridge 仍应存在");
    assert!(!read.is_direct(), "清单未发布不得提升 direct");
    assert_eq!(collected.len(), 3, "1 static bridge + resource + discover");

    fixture.shutdown().await;
}

/// 已 ready 但必需工具缺失：收集退回 deferred（不产生半成品 direct），
/// 该结果不构成 ready，闸门仍会在进入 Compact 前 fatal。
#[tokio::test]
async fn collect_tools_falls_back_to_deferred_when_required_tool_missing() {
    let mut fixture = GateFixture::new();
    fixture.config("sys", system_config(Some(vec!["Read".to_string()]), None));
    let (_, generation) = fixture
        .connect("sys", vec![fixture_tool("Write", read_schema())])
        .await;
    fixture.ready("sys", generation);

    let mw = McpMiddleware::new(Arc::clone(fixture.pool()));
    let collected = <McpMiddleware as Middleware>::collect_tools(&mw, "/tmp");
    let write = collected
        .iter()
        .find(|tool| tool.name() == "mcp__sys__Write")
        .expect("普通工具仍应收集");
    assert!(!write.is_direct(), "校验失败不得留下部分 direct");
    assert_eq!(collected.len(), 3);

    fixture.shutdown().await;
}

/// 闸门通过不改变既有 discovery 与状态通知行为：既不消费状态变化缓冲，
/// 也不自行触发发现；`before_agent` 仍按原语义触发（幂等增量挂点保留）。
#[tokio::test]
async fn successful_gate_preserves_discovery_and_status_notifications() {
    let pool = Arc::new(McpClientPool::new_empty());
    insert_skill_handle(
        &pool,
        "srv",
        vec![Resource::new("skill://demo/SKILL.md", "d")],
    );
    insert_skill_handle(&pool, "status", vec![]);
    pool.publish_system_manifest(SystemMcpManifest::Loaded);
    let reg = Arc::new(McpSkillRegistry::new());
    let mw = McpMiddleware::new(Arc::clone(&pool))
        .with_skill_discovery(Some(Arc::clone(&reg)), AgentCancellationToken::new());

    pool.mark_initialized();
    pool.record_status_change("status", Some(&ClientStatus::Connected));
    if let Some(handle) = pool.clients.write().get_mut("status") {
        Arc::make_mut(handle).status = ClientStatus::Failed("boom".to_string());
    }
    pool.record_status_change("status", Some(&ClientStatus::Connected));

    let mut probe = StartupGateProbe::default();
    Middleware::before_react_start(&mw, &mut probe)
        .await
        .expect("无 System 依赖的闸门必须放行");
    assert!(
        probe.staged.is_none(),
        "无 System 依赖不产生 startup update"
    );
    assert!(reg.discovery_state("srv").is_none(), "闸门自身不得触发发现");
    assert_eq!(
        pool.drain_pending_changes().len(),
        1,
        "闸门不得消费状态变化缓冲"
    );

    let mut state = AgentState::new("/tmp");
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    assert!(
        matches!(
            reg.discovery_state("srv"),
            Some(ServerDiscoveryState::Started { .. })
        ),
        "既有 before_agent 发现行为保留"
    );
}

/// 失败不留下可复用的半成品：同一 middleware 连续两次准入，第二次按当次句柄
/// 重新构建（不把上次的 direct 标记套到新代，也不复用失败的候选）。
#[tokio::test]
async fn failed_gate_leaves_no_reusable_candidate() {
    let mut fixture = GateFixture::new();
    fixture.config("sys", system_config(Some(vec!["Read".to_string()]), None));
    let (_, generation) = fixture
        .connect("sys", vec![fixture_tool("Write", read_schema())])
        .await;
    fixture.ready("sys", generation);

    let mw = McpMiddleware::new(Arc::clone(fixture.pool()));

    let mut probe = StartupGateProbe::default();
    Middleware::before_react_start(&mw, &mut probe)
        .await
        .expect_err("首次准入缺必需工具");
    assert!(probe.staged.is_none(), "失败不得留下候选");

    // 第二代句柄补齐必需工具：证据必须重新按新代提交。
    let (_, generation) = fixture
        .connect(
            "sys",
            vec![
                fixture_tool("Read", read_schema()),
                fixture_tool("Write", read_schema()),
            ],
        )
        .await;
    fixture.ready("sys", generation);

    Middleware::before_react_start(&mw, &mut probe)
        .await
        .expect("第二代就绪后必须放行");
    let update = probe.staged.expect("成功准入必须暂存候选");
    assert_eq!(
        bridge_names(&update.tools),
        vec![
            ("mcp__sys__Read".to_string(), true),
            ("mcp__sys__Write".to_string(), false),
        ],
        "候选按当次句柄重建，不残留失败批次"
    );

    fixture.shutdown().await;
}
