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
    fn set_cwd(&mut self, _cwd: String) {}
    fn messages(&self) -> &[peri_agent::messages::BaseMessage] {
        &[]
    }
    fn add_message(&mut self, _message: peri_agent::messages::BaseMessage) {}
    fn prepend_message(&mut self, _message: peri_agent::messages::BaseMessage) {}
    fn messages_mut(&mut self) -> &mut Vec<peri_agent::messages::BaseMessage> {
        unreachable!()
    }
    fn current_step(&self) -> usize {
        0
    }
    fn set_current_step(&mut self, _step: usize) {}
    fn get_context(&self, _key: &str) -> Option<&str> {
        None
    }
    fn set_context(&mut self, _key: String, _value: String) {}
    fn token_tracker(&self) -> &peri_agent::agent::token::TokenTracker {
        unreachable!()
    }
    fn token_tracker_mut(&mut self) -> &mut peri_agent::agent::token::TokenTracker {
        unreachable!()
    }
    fn push_recall(&mut self, _item: String) {}
    fn drain_recall(&mut self) -> Vec<String> {
        vec![]
    }
    fn ancestor_len(&self) -> usize {
        0
    }
    fn store(&self) -> Option<&Arc<dyn peri_agent::thread::ThreadStore>> {
        None
    }
    fn own_thread_id(&self) -> Option<&peri_agent::thread::ThreadId> {
        None
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
        .map(|m| m.message.content().to_string())
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

    // 队列内消息均为 Info + SystemInjected
    for msg in &drained {
        assert_eq!(msg.kind, MessageKind::Info, "必须为 Info（不唤醒循环）");
        assert!(
            matches!(
                msg.source,
                peri_agent::session::MessageSource::SystemInjected
            ),
            "source 应为 SystemInjected"
        );
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
            .map(|m| m.message.content().to_string())
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
