//! 命令模块测试：register_builtins 集成（注册表语义本体在契约层
//! `command_registry_test.rs`，本文件不再重复 Vec 时代的前缀匹配 / list 用例）
//! + ClearCommand 行为测试。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_acp_types::command::ArgsSchema;
use peri_acp_types::event::ExecutorEvent;
use peri_acp_types::messages::BaseMessage;

use super::clear::ClearCommand;
use super::{
    register_builtins, CommandContext, CommandHandler, CommandOutcome, CommandRegistry,
    CommandResult, FeedbackChannel, FeedbackLevel,
};
use crate::session::executor::PromptStopReason;

/// 执行并解包：clear 恒 Done，其他变体 panic（与旧 AgentCommand 转发
/// unreachable! 同语义；Phase 5 Step 6 旧契约删除后直接经新契约执行）。
async fn execute_clear(cmd: &ClearCommand, ctx: CommandContext) -> CommandResult {
    match CommandHandler::execute(cmd, ctx).await {
        CommandOutcome::Done(r) => r,
        _ => panic!("clear 应恒 Done"),
    }
}

// ── Mock EventSink ─────────────────────────────────────────────────────────

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

    async fn push_done(&self, _session_id: &str, _stop_reason: &str, _request_id: Option<&str>) {
        *self.push_done_count.lock().unwrap() += 1;
    }
}

/// 构造最小 CommandContext。
fn make_command_context(sink: Arc<dyn crate::session::event_sink::EventSink>) -> CommandContext {
    // Phase 2 拆层：deps 私有化后构造面封闭，core 5 字段经 new() 就位；
    // 旧字段默认值与原字面量一致（compact_config: Default / 其余 None）。
    CommandContext::new(
        "test-session".to_string(),
        vec![],
        "/tmp".to_string(),
        sink,
        tokio_util::sync::CancellationToken::new(),
        peri_acp_types::command::DependencyBag::new(),
    )
}

// ── register_builtins 集成测试 ────────────────────────────────────────────

/// 内置命令注册：裸名 / alias / 全名三种输入均命中同一条目；
/// 严格精确匹配（设计 §55：`/rew` 不解析为 `/rewind`）。
#[test]
fn test_register_builtins_resolves_builtin_commands() {
    let reg = CommandRegistry::new();
    register_builtins(&reg);

    // 裸名（第一等级域条目登记裸名，alias_index）
    let resolved = reg.resolve("/compact").expect("裸名 compact 应命中");
    assert_eq!(resolved.entry.fullname, "core:compact");
    assert_eq!(resolved.args, "");

    // alias（命令实现声明，单一事实源）
    let resolved = reg.resolve("/compress").expect("alias compress 应命中");
    assert_eq!(resolved.entry.fullname, "core:compact");

    // 全名（entries 直接键）
    let resolved = reg.resolve("/core:compact").expect("全名应命中");
    assert_eq!(resolved.entry.fullname, "core:compact");

    // 带参数：词法切分由注册表 resolve 统一完成（不变式 3）
    let resolved = reg
        .resolve("/compact  hello ")
        .expect("compact 带参数应命中");
    assert_eq!(resolved.entry.fullname, "core:compact");
    assert_eq!(resolved.args, "hello");

    // 前缀不再匹配（设计 §55 裁决：模糊只留 UI 搜索层）
    assert!(reg.resolve("/rew").is_none());

    // 其余内置 + loop 占位（P1-7：投影条目不缺失）
    let clear_entry = reg.resolve("/clear").unwrap().entry;
    assert_eq!(clear_entry.fullname, "core:clear");
    // Phase 5 Step 3：clear 无参命令，注册条目挂 ArgsSchema::default()（投影可渲染）
    assert_eq!(clear_entry.args_schema, Some(ArgsSchema::default()));
    assert_eq!(reg.resolve("/cls").unwrap().entry.fullname, "core:clear");
    assert_eq!(
        reg.resolve("/rewind").unwrap().entry.fullname,
        "core:rewind"
    );
    assert_eq!(reg.resolve("/bg").unwrap().entry.fullname, "core:bg");
    assert_eq!(reg.resolve("/loop").unwrap().entry.fullname, "core:loop");
    assert_eq!(reg.snapshot().len(), 5, "内置命令共 5 条（含 loop 占位）");
}

// ── ClearCommand 测试 ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_clear_command_returns_empty_messages() {
    // Arrange
    let sink = Arc::new(MockEventSink::new());
    let ctx = make_command_context(sink.clone());
    let cmd = ClearCommand;

    // Act
    let result = execute_clear(&cmd, ctx).await;

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
    let result = execute_clear(&cmd, ctx).await;

    // Assert
    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);
}

#[tokio::test]
async fn test_clear_command_no_longer_emits_compact_completed() {
    // Arrange
    let sink = Arc::new(MockEventSink::new());
    let ctx = make_command_context(sink.clone());
    let cmd = ClearCommand;

    // Act
    execute_clear(&cmd, ctx).await;

    // Assert: Phase 5 Step 3 迁移后 clear 不再发射 20 字段占位 CompactCompleted
    // （占位事件已删除，通知文案移交 CommandFeedback），命令内零事件代码。
    let events = sink.events();
    assert!(
        events.is_empty(),
        "clear 不应产生任何事件（占位 CompactCompleted 已删除），实际: {:?}",
        events
    );
}

/// Phase 5 Step 3：新契约路径（CommandHandler 主实现）——Done(空 messages +
/// EndTurn + feedback(Info, "对话已清空", UiOnly))。
#[tokio::test]
async fn test_clear_command_handler_feedback() {
    // Arrange
    let sink = Arc::new(MockEventSink::new());
    let ctx = make_command_context(sink.clone());
    let cmd = ClearCommand;

    // Act: 直接经新契约执行（与 LegacyAdapter 转发路径同源）
    let outcome = CommandHandler::execute(&cmd, ctx).await;

    // Assert
    let CommandOutcome::Done(result) = outcome else {
        panic!("clear 恒 Done");
    };
    assert_eq!(result.messages.len(), 0, "清空后会话为空");
    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);
    let feedback = result.feedback.expect("clear 应携带反馈");
    assert_eq!(feedback.level, FeedbackLevel::Info);
    assert_eq!(feedback.message, "对话已清空");
    assert_eq!(feedback.channel, FeedbackChannel::UiOnly, "UiOnly 不进会话");
    // 命令自身不发射事件（编排层 emit_command_feedback 统一发射）
    assert!(sink.events().is_empty());
}

#[tokio::test]
async fn test_clear_command_ignores_existing_history() {
    // Arrange: 带有历史消息的上下文
    let sink = Arc::new(MockEventSink::new());
    // Phase 2 拆层：deps 私有化后构造面封闭，core 5 字段经 new() 就位；
    // 旧字段默认值与原字面量一致（compact_config: Default / 其余 None）。
    let ctx = CommandContext::new(
        "test-session".to_string(),
        vec![BaseMessage::human("你好"), BaseMessage::ai("世界")],
        "/tmp".to_string(),
        sink.clone(),
        tokio_util::sync::CancellationToken::new(),
        peri_acp_types::command::DependencyBag::new(),
    );
    let cmd = ClearCommand;

    // Act
    let result = execute_clear(&cmd, ctx).await;

    // Assert: 无论历史如何，返回空消息
    assert_eq!(result.messages.len(), 0);
    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);
}

#[test]
fn test_clear_command_name_and_aliases() {
    // Phase 5 Step 6：旧 AgentCommand trait 已删，元数据取命令关联常量
    //（注册条目挂载的单一事实源）。
    assert_eq!(ClearCommand::NAME, "clear");
    assert!(ClearCommand::ALIASES.contains(&"cls"), "应包含 cls 别名");
    assert!(
        ClearCommand::ALIASES.contains(&"reset"),
        "应包含 reset 别名"
    );
    assert!(!ClearCommand::DESCRIPTION.is_empty());
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

    execute_clear(&cmd, ctx).await;

    // ClearCommand 自身不调用 push_done
    let count = sink.push_done_count();
    assert_eq!(
        count, 0,
        "ClearCommand 自身不应调用 push_done，由 executor 负责"
    );
}

// ── LoopPlaceholder 占位测试 ──────────────────────────────────────────────
// Phase 5 Step 5.5：产品未裁决执行语义，按 plan 二选一默认保留占位语义——
// 确定性执行（resolve 命中 → Done，杜绝静默 fall through）+ UI-only 反馈
// 「loop 命令尚未实现」；不退役（与 Phase 3 预注册要求一致，投影条目不缺失）。

/// /loop resolve 命中注册表条目（不再是投影幽灵条目，注册表含 handler）。
#[test]
fn test_loop_resolve_hits_registry_entry() {
    let reg = CommandRegistry::new();
    register_builtins(&reg);

    let resolved = reg.resolve("/loop").expect("/loop 应命中 core:loop");
    assert_eq!(resolved.entry.fullname, "core:loop");
    assert_eq!(resolved.entry.description, "Control agent iteration loop");
    assert!(
        Arc::strong_count(&resolved.entry.handler) >= 1,
        "占位 handler 必须挂载，保证路由确定性执行"
    );
    // 注册表共 5 条（compact / bg / clear / rewind / loop），snapshot 断言在
    // test_register_builtins_resolves_builtin_commands 中保持。
}

/// loop 占位执行返回 Done（确定性执行，杜绝 fall through）+ UI-only 反馈。
#[tokio::test]
async fn test_loop_placeholder_executes_done_with_ui_only_feedback() {
    // Arrange: 带历史消息，断言 history 原样返回（占位不改变会话）
    let sink = Arc::new(MockEventSink::new());
    let history = vec![BaseMessage::human("你好"), BaseMessage::ai("世界")];
    let ctx = CommandContext::new(
        "test-session".to_string(),
        history.clone(),
        "/tmp".to_string(),
        sink.clone(),
        tokio_util::sync::CancellationToken::new(),
        peri_acp_types::command::DependencyBag::new(),
    );

    let reg = CommandRegistry::new();
    register_builtins(&reg);
    let resolved = reg.resolve("/loop").expect("/loop 应命中 core:loop");

    // Act: 经注册表条目 handler 直接执行（与拦截层同源）
    let outcome = CommandHandler::execute(resolved.entry.handler.as_ref(), ctx).await;

    // Assert: 恒 Done + EndTurn + history 原样 + feedback(Info, UiOnly)
    let CommandOutcome::Done(result) = outcome else {
        panic!("loop 占位恒 Done");
    };
    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);
    // BaseMessage 无 PartialEq，按条数 + 文本断言 history 原样返回
    assert_eq!(result.messages.len(), history.len(), "占位不改变会话");
    assert_eq!(result.messages[0].content(), "你好");
    assert_eq!(result.messages[1].content(), "世界");
    let feedback = result.feedback.expect("loop 占位应携带反馈");
    assert_eq!(feedback.level, FeedbackLevel::Info);
    assert_eq!(feedback.message, "loop 命令尚未实现");
    assert_eq!(feedback.channel, FeedbackChannel::UiOnly, "UiOnly 不进会话");
    // 命令自身不发射事件（编排层 emit_command_feedback 统一发射）
    assert!(sink.events().is_empty());
}
