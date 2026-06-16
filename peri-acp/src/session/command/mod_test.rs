//! CommandRegistry 和 ClearCommand 单元测试。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_agent::agent::events::AgentEvent as ExecutorEvent;
use peri_agent::messages::BaseMessage;

use super::clear::ClearCommand;
use super::{AgentCommand, CommandContext, CommandKind, CommandRegistry, CommandResult};
use crate::session::executor::PromptStopReason;

// ── Mock ──────────────────────────────────────────────────────────────────

/// Mock AgentCommand，用于测试 CommandRegistry。
struct MockCommand {
    name: &'static str,
    aliases: Vec<&'static str>,
    description: &'static str,
    kind: CommandKind,
}

impl MockCommand {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            aliases: vec![],
            description: "mock command",
            kind: CommandKind::Immediate,
        }
    }

    fn with_aliases(mut self, aliases: Vec<&'static str>) -> Self {
        self.aliases = aliases;
        self
    }
}

#[async_trait]
impl AgentCommand for MockCommand {
    fn name(&self) -> &str {
        self.name
    }

    fn aliases(&self) -> Vec<&str> {
        self.aliases.clone()
    }

    fn description(&self) -> &str {
        self.description
    }

    fn kind(&self) -> CommandKind {
        self.kind
    }

    async fn execute(&self, _ctx: CommandContext) -> CommandResult {
        CommandResult {
            messages: vec![],
            stop_reason: PromptStopReason::EndTurn,
        }
    }
}

/// Mock EventSink，记录所有推送的事件。
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

/// 构造最小 CommandContext。
fn make_command_context(sink: Arc<dyn crate::session::event_sink::EventSink>) -> CommandContext {
    CommandContext {
        session_id: "test-session".to_string(),
        history: vec![],
        cwd: "/tmp".to_string(),
        peri_config: Arc::new(Default::default()),
        compact_model: None,
        event_sink: sink,
        args: String::new(),
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

// ── CommandRegistry 测试 ──────────────────────────────────────────────────

#[test]
fn test_registry_find_by_exact_name() {
    // Arrange: 注册两个命令
    let mut reg = CommandRegistry::new();
    reg.register(Box::new(MockCommand::new("alpha")));
    reg.register(Box::new(MockCommand::new("beta")));

    // Act & Assert
    let (cmd, args) = reg.find("/alpha").unwrap();
    assert_eq!(cmd.name(), "alpha");
    assert_eq!(args, "");

    let (cmd, args) = reg.find("/beta").unwrap();
    assert_eq!(cmd.name(), "beta");
    assert_eq!(args, "");
}

#[test]
fn test_registry_find_by_alias() {
    // Arrange: 注册带别名的命令
    let mut reg = CommandRegistry::new();
    reg.register(Box::new(
        MockCommand::new("compact").with_aliases(vec!["compress", "zip"]),
    ));

    // Act & Assert
    let (cmd, args) = reg.find("/compress").unwrap();
    assert_eq!(cmd.name(), "compact");
    assert_eq!(args, "");

    let (cmd, args) = reg.find("/zip").unwrap();
    assert_eq!(cmd.name(), "compact");
    assert_eq!(args, "");
}

#[test]
fn test_registry_find_with_args() {
    // Arrange
    let mut reg = CommandRegistry::new();
    reg.register(Box::new(MockCommand::new("skill")));

    // Act
    let (cmd, args) = reg.find("/skill tdd").unwrap();

    // Assert
    assert_eq!(cmd.name(), "skill");
    assert_eq!(args, "tdd");
}

#[test]
fn test_registry_find_with_multiple_args() {
    // Arrange
    let mut reg = CommandRegistry::new();
    reg.register(Box::new(MockCommand::new("skill")));

    // Act
    let (cmd, args) = reg.find("/skill tdd --force").unwrap();

    // Assert
    assert_eq!(cmd.name(), "skill");
    assert_eq!(args, "tdd --force");
}

#[test]
fn test_registry_find_returns_none_for_unknown() {
    // Arrange
    let mut reg = CommandRegistry::new();
    reg.register(Box::new(MockCommand::new("compact")));

    // Act & Assert
    assert!(reg.find("/unknown").is_none());
}

#[test]
fn test_registry_find_returns_none_for_empty_string() {
    // Arrange
    let mut reg = CommandRegistry::new();
    reg.register(Box::new(MockCommand::new("compact")));

    // Act & Assert
    assert!(reg.find("").is_none());
}

#[test]
fn test_registry_find_returns_none_for_double_slash() {
    // Arrange
    let mut reg = CommandRegistry::new();
    reg.register(Box::new(MockCommand::new("compact")));

    // Act & Assert
    assert!(reg.find("//").is_none());
}

#[test]
fn test_registry_find_returns_none_for_slash_only() {
    // Arrange
    let mut reg = CommandRegistry::new();
    reg.register(Box::new(MockCommand::new("compact")));

    // Act & Assert: 单个 `/` → trim 后为空字符串
    assert!(reg.find("/").is_none());
}

#[test]
fn test_registry_list_returns_all_commands() {
    // Arrange: 注册 3 个命令
    let mut reg = CommandRegistry::new();
    reg.register(Box::new(MockCommand::new("alpha").with_aliases(vec!["a"])));
    reg.register(Box::new(MockCommand::new("beta")));
    reg.register(Box::new(
        MockCommand::new("gamma").with_aliases(vec!["g1", "g2"]),
    ));

    // Act
    let list = reg.list();

    // Assert: 返回所有命令元组
    assert_eq!(list.len(), 3);
    assert_eq!(list[0].0, "alpha");
    assert_eq!(list[0].2, vec!["a"]);
    assert_eq!(list[1].0, "beta");
    assert_eq!(list[1].2, Vec::<&str>::new());
    assert_eq!(list[2].0, "gamma");
    assert_eq!(list[2].2, vec!["g1", "g2"]);
}

#[test]
fn test_default_registry_contains_compact_and_clear() {
    // Act
    let reg = CommandRegistry::default();

    // Assert: 默认注册表包含 compact 和 clear
    let names: Vec<&str> = reg.list().iter().map(|(n, _, _)| *n).collect();
    assert!(names.contains(&"compact"), "默认注册表应包含 compact");
    assert!(names.contains(&"clear"), "默认注册表应包含 clear");
}

// ── ClearCommand 测试 ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_clear_command_returns_empty_messages() {
    // Arrange
    let sink = Arc::new(MockEventSink::new());
    let ctx = make_command_context(sink.clone());
    let cmd = ClearCommand;

    // Act
    let result = cmd.execute(ctx).await;

    // Assert: 返回空消息列表
    assert_eq!(result.messages.len(), 0);
}

#[tokio::test]
async fn test_clear_command_returns_end_turn() {
    // Arrange
    let sink = Arc::new(MockEventSink::new());
    let ctx = make_command_context(sink.clone());
    let cmd = ClearCommand;

    // Act
    let result = cmd.execute(ctx).await;

    // Assert
    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);
}

#[tokio::test]
async fn test_clear_command_sends_event() {
    // Arrange
    let sink = Arc::new(MockEventSink::new());
    let ctx = make_command_context(sink.clone());
    let cmd = ClearCommand;

    // Act
    cmd.execute(ctx).await;

    // Assert: 应该推送了 CompactCompleted 事件（空 messages）
    let events = sink.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, "test-session");
    // CompactCompleted 的 JSON 应包含 "compact_completed"（serde 序列化为 snake_case）
    assert!(
        events[0].1.contains("compact_completed"),
        "事件应包含 compact_completed，实际: {}",
        events[0].1
    );
    // messages 应为空数组
    assert!(
        events[0].1.contains("\"messages\":[]"),
        "compact_completed.messages 应为空数组，实际: {}",
        events[0].1
    );
}

#[tokio::test]
async fn test_clear_command_ignores_existing_history() {
    // Arrange: 带有历史消息的上下文
    let sink = Arc::new(MockEventSink::new());
    let ctx = CommandContext {
        session_id: "test-session".to_string(),
        history: vec![BaseMessage::human("你好"), BaseMessage::ai("世界")],
        cwd: "/tmp".to_string(),
        peri_config: Arc::new(Default::default()),
        compact_model: None,
        event_sink: sink.clone(),
        args: String::new(),
        cancel_token: peri_agent::agent::AgentCancellationToken::new(),
        thread_store: None,
        thread_id: None,
        bg_event_sender: None,
        bg_registry: None,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
    };
    let cmd = ClearCommand;

    // Act
    let result = cmd.execute(ctx).await;

    // Assert: 无论历史如何，返回空消息
    assert_eq!(result.messages.len(), 0);
    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);
}

#[test]
fn test_clear_command_name_and_aliases() {
    // Arrange
    let cmd = ClearCommand;

    // Assert
    assert_eq!(cmd.name(), "clear");
    let aliases = cmd.aliases();
    assert!(aliases.contains(&"cls"), "应包含 cls 别名");
    assert!(aliases.contains(&"reset"), "应包含 reset 别名");
    assert_eq!(cmd.kind(), CommandKind::Immediate);
    assert!(!cmd.description().is_empty());
}

// ── push_done 验证测试 ──────────────────────────────────────────────────────
// 对应 TRAP: CLAUDE.md issue_2026-05-29-immediate-command-missing-push-done

/// 验证 MockEventSink 记录 push_done 调用
#[test]
fn test_mock_event_sink_push_done_counting() {
    let sink = MockEventSink::new();
    // 新创建的 sink push_done 计数为 0
    assert_eq!(sink.push_done_count(), 0);
}

/// 验证 ClearCommand 执行后不自行调用 push_done（由 executor 负责）
#[tokio::test]
async fn test_clear_command_does_not_call_push_done_itself() {
    let sink = Arc::new(MockEventSink::new());
    let ctx = make_command_context(sink.clone());
    let cmd = ClearCommand;

    cmd.execute(ctx).await;

    // ClearCommand 自身不调用 push_done
    let count = sink.push_done_count();
    assert_eq!(
        count, 0,
        "ClearCommand 自身不应调用 push_done，由 executor 负责"
    );
}
