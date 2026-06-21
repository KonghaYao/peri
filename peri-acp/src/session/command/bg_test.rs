//! BgCommand 单元测试。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_agent::agent::events::AgentEvent as ExecutorEvent;

use super::super::{AgentCommand, CommandContext, CommandKind};
use super::BgCommand;
use crate::session::executor::PromptStopReason;

// ── Mock EventSink ────────────────────────────────────────────────────────

struct MockEventSink {
    events: Mutex<Vec<(String, String)>>,
    push_done_count: Mutex<usize>,
}

impl MockEventSink {
    fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            push_done_count: Mutex::new(0),
        }
    }

    fn events(&self) -> Vec<(String, String)> {
        self.events.lock().unwrap().clone()
    }

    fn push_done_count(&self) -> usize {
        *self.push_done_count.lock().unwrap()
    }
}

#[async_trait]
impl crate::session::event_sink::EventSink for MockEventSink {
    async fn push_event(&self, session_id: &str, event: &ExecutorEvent, _context_window: u32) {
        let json = serde_json::to_string(event).unwrap_or_default();
        self.events
            .lock()
            .unwrap()
            .push((session_id.to_string(), json));
    }

    async fn push_done(&self, _session_id: &str) {
        *self.push_done_count.lock().unwrap() += 1;
    }
}

fn make_ctx(sink: Arc<dyn crate::session::event_sink::EventSink>, args: &str) -> CommandContext {
    CommandContext {
        session_id: "test-session".to_string(),
        history: vec![],
        cwd: "/tmp".to_string(),
        peri_config: Arc::new(Default::default()),
        auxiliary_model: None,
        event_sink: sink,
        args: args.to_string(),
        cancel_token: peri_agent::agent::AgentCancellationToken::new(),
        thread_store: None,
        thread_id: None,
        bg_event_sender: None,
        bg_registry: None,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
    }
}

// ── BgCommand 属性测试 ────────────────────────────────────────────────────

#[test]
fn test_bg_command_name_and_aliases() {
    let cmd = BgCommand;

    assert_eq!(cmd.name(), "bg");
    let aliases = cmd.aliases();
    assert!(aliases.contains(&"background"), "应包含 background 别名");
    assert_eq!(cmd.kind(), CommandKind::Immediate);
    assert!(!cmd.description().is_empty());
}

// ── 空参数测试 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_bg_command_empty_prompt_shows_usage() {
    let sink = Arc::new(MockEventSink::new());
    let ctx = make_ctx(sink.clone(), "");
    let cmd = BgCommand;

    let result = cmd.execute(ctx).await;

    // 应返回空消息 + EndTurn
    assert_eq!(result.messages.len(), 0);
    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);

    // 应推送 TextChunk 事件包含用法信息
    let events = sink.events();
    assert_eq!(events.len(), 1);
    assert!(
        events[0].1.contains("用法"),
        "空参数应推送用法提示，实际: {}",
        events[0].1
    );
    assert!(
        events[0].1.contains("/bg"),
        "用法提示应包含命令名 /bg，实际: {}",
        events[0].1
    );
}

#[tokio::test]
async fn test_bg_command_does_not_call_push_done_itself() {
    let sink = Arc::new(MockEventSink::new());
    let ctx = make_ctx(sink.clone(), "");
    let cmd = BgCommand;

    let _result = cmd.execute(ctx).await;

    // BgCommand 自身不应调用 push_done（由 executor 负责）
    let count = sink.push_done_count();
    assert_eq!(
        count, 0,
        "BgCommand 自身不应调用 push_done，由 executor 负责"
    );
}

// ── 默认注册表测试 ────────────────────────────────────────────────────────

#[test]
fn test_default_registry_contains_bg() {
    let reg = super::super::default_command_registry();
    let names: Vec<&str> = reg.list().iter().map(|(n, _, _)| *n).collect();
    assert!(names.contains(&"bg"), "默认注册表应包含 bg 命令");
}

#[test]
fn test_bg_command_registry_find() {
    let reg = super::super::default_command_registry();

    // 通过名称查找
    let (cmd, args) = reg.find("/bg 帮我搜索 Rust 2026 roadmap").unwrap();
    assert_eq!(cmd.name(), "bg");
    assert_eq!(args, "帮我搜索 Rust 2026 roadmap");

    // 通过别名查找
    let (cmd, args) = reg.find("/background 调研 tokio 最新版本").unwrap();
    assert_eq!(cmd.name(), "bg");
    assert_eq!(args, "调研 tokio 最新版本");
}
