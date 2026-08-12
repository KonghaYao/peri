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
    assert!(tools.is_empty());
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
        peer: None,
        tools: (0..tools).map(|_| rmcp::model::Tool::default()).collect(),
        resources: vec![],
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::default(),
        source: None,
        url: None,
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
            peer: None,
            tools: vec![],
            resources: vec![],
            status: ClientStatus::Failed("transport closed".to_string()),
            oauth_status: OAuthStatus::default(),
            source: None,
            url: None,
            channel_capable: false,
        }),
    );
    pool.clients.write().insert(
        "legacy".to_string(),
        Arc::new(McpClientHandle {
            name: "legacy".to_string(),
            peer: None,
            tools: vec![],
            resources: vec![],
            status: ClientStatus::Disabled,
            oauth_status: OAuthStatus::default(),
            source: None,
            url: None,
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
