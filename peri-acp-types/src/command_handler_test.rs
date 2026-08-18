//! `CommandOutcome` 三态与 `CommandHandler` trait 契约测试。
//!
//! 覆盖：三态构造与模式匹配、假 handler 的调用面（值语义 ctx 传递）、
//! trait 的 `Send + Sync` 约束可满足性。不依赖任何真实命令实现。

use async_trait::async_trait;

use crate::command::command_handler::{CommandHandler, CommandOutcome};
use crate::command::{CommandContext, CommandResult, PromptStopReason};
use crate::event::{EventSink, ExecutorEvent};

/// 假事件 sink：仅实现必须方法，默认方法留空。
struct NoopSink;

#[async_trait]
impl EventSink for NoopSink {
    async fn push_event(&self, _session_id: &str, _event: &ExecutorEvent, _context_window: u32) {}

    async fn push_done(&self, _session_id: &str, _stop_reason: &str, _request_id: Option<&str>) {}
}

/// 构造最小 CommandContext（对齐 `peri-acp/session/command/mod_test.rs`
/// 先例；17 字段全量字面量，Phase 2 拆层后随构造点一并迁移）。
fn make_context(args: &str) -> CommandContext {
    CommandContext {
        session_id: "test-session".to_string(),
        history: vec![],
        cwd: "/tmp".to_string(),
        compact_config: Default::default(),
        auxiliary_model: None,
        event_sink: std::sync::Arc::new(NoopSink),
        raw_text: String::new(),
        supports_inject: false,
        args: args.to_string(),
        parsed_args: None,
        cancel_token: tokio_util::sync::CancellationToken::new(),
        thread_store: None,
        thread_id: None,
        task_manager: None,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        frozen_system_prompt: None,
        // Phase 2 拆层：扩展依赖接口注册表（crate 内测试直接补空表；
        // 消费方构造点迁移形态见 peri-agent/peri-acp 各构造点）。
        deps: crate::command::DependencyBag::new(),
    }
}

/// 构造最小 CommandResult。
///
/// 注意：`feedback` 字段由 Phase 1 步骤 4 加入（一次性替换），本函数
/// 为字面量全字段构造（无 `..` 展开），步骤 4 变更已同步至此。
fn make_result() -> CommandResult {
    CommandResult {
        messages: vec![],
        stop_reason: PromptStopReason::EndTurn,
        feedback: None,
    }
}

/// 假 handler：固定返回 `Done`（验证 trait 可实现性与 Done 路径调用面）。
struct DoneHandler;

#[async_trait]
impl CommandHandler for DoneHandler {
    async fn execute(&self, _ctx: CommandContext) -> CommandOutcome {
        CommandOutcome::Done(make_result())
    }
}

/// 假 handler：把 ctx.args 原样注入（验证值语义 ctx 传递与 Inject 路径）。
struct InjectHandler;

#[async_trait]
impl CommandHandler for InjectHandler {
    async fn execute(&self, ctx: CommandContext) -> CommandOutcome {
        CommandOutcome::Inject(ctx.args)
    }
}

/// 假 handler：固定转发到 ui 域（验证 Delegate 路径）。
struct DelegateHandler;

#[async_trait]
impl CommandHandler for DelegateHandler {
    async fn execute(&self, _ctx: CommandContext) -> CommandOutcome {
        CommandOutcome::Delegate("ui:history".to_string())
    }
}

/// 静态断言：`CommandHandler` 的实现须满足 `Send + Sync`（trait 声明
/// 即 supertrait，编译期校验可满足性）。
fn assert_send_sync<T: Send + Sync + ?Sized>() {}

// ── CommandOutcome 三态构造与模式匹配 ──────────────────────────────────────

#[test]
fn outcome_done_carries_result() {
    // Arrange
    let outcome = CommandOutcome::Done(make_result());

    // Act & Assert：解构出 messages / stop_reason
    match outcome {
        CommandOutcome::Done(result) => {
            assert!(result.messages.is_empty());
            assert_eq!(result.stop_reason, PromptStopReason::EndTurn);
        }
        _ => panic!("expected Done variant"),
    }
}

#[test]
fn outcome_inject_carries_prompt() {
    // Arrange
    let outcome = CommandOutcome::Inject("run skill demo".to_string());

    // Act & Assert
    match outcome {
        CommandOutcome::Inject(prompt) => assert_eq!(prompt, "run skill demo"),
        _ => panic!("expected Inject variant"),
    }
}

#[test]
fn outcome_delegate_carries_target() {
    // Arrange
    let outcome = CommandOutcome::Delegate("ui:history".to_string());

    // Act & Assert
    match outcome {
        CommandOutcome::Delegate(target) => assert_eq!(target, "ui:history"),
        _ => panic!("expected Delegate variant"),
    }
}

#[test]
fn outcome_variants_are_exhaustive_and_disjoint() {
    // 三态穷尽匹配（编译器保证无遗漏）；运行时断言各自落位、互不串扰。
    let outcomes = [
        CommandOutcome::Done(make_result()),
        CommandOutcome::Inject("x".to_string()),
        CommandOutcome::Delegate("y".to_string()),
    ];

    let mut done = 0;
    let mut inject = 0;
    let mut delegate = 0;
    for outcome in outcomes {
        match outcome {
            CommandOutcome::Done(_) => done += 1,
            CommandOutcome::Inject(_) => inject += 1,
            CommandOutcome::Delegate(_) => delegate += 1,
        }
    }
    assert_eq!((done, inject, delegate), (1, 1, 1));
}

// ── CommandHandler 假实现调用面 ────────────────────────────────────────────

#[tokio::test]
async fn handler_done_returns_done() {
    // Arrange
    let handler = DoneHandler;
    let ctx = make_context("");

    // Act
    let outcome = handler.execute(ctx).await;

    // Assert
    match outcome {
        CommandOutcome::Done(result) => assert_eq!(result.stop_reason, PromptStopReason::EndTurn),
        _ => panic!("expected Done variant"),
    }
}

#[tokio::test]
async fn handler_inject_passes_ctx_by_value() {
    // Arrange：ctx 按值传入（handler 独占消费 ctx.args）。
    let handler = InjectHandler;
    let ctx = make_context("demo skill");

    // Act
    let outcome = handler.execute(ctx).await;

    // Assert
    match outcome {
        CommandOutcome::Inject(prompt) => assert_eq!(prompt, "demo skill"),
        _ => panic!("expected Inject variant"),
    }
}

#[tokio::test]
async fn handler_delegate_returns_target() {
    // Arrange
    let handler = DelegateHandler;
    let ctx = make_context("");

    // Act
    let outcome = handler.execute(ctx).await;

    // Assert
    match outcome {
        CommandOutcome::Delegate(target) => assert_eq!(target, "ui:history"),
        _ => panic!("expected Delegate variant"),
    }
}

#[test]
fn handler_implementations_are_send_sync() {
    // 编译期校验：假 handler 与 dyn trait 对象均满足 Send + Sync。
    assert_send_sync::<DoneHandler>();
    assert_send_sync::<InjectHandler>();
    assert_send_sync::<DelegateHandler>();
    assert_send_sync::<dyn CommandHandler>();
}
