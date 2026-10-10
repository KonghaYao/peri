//! executor_helpers.rs 单元测试（L5：自 ACP `executor_test.rs` 随迁）。
//!
//! 重点覆盖 [`intercept_immediate_command`]——命令拦截是 execute_prompt 的
//! 前置短路逻辑，任何回归（如忘记 `push_done`）都会导致 TUI 永久 loading
//! （issue_2026-05-29-immediate-command-missing-push-done）。
//!
//! 随迁适配（R4，断言语义不重写）：`peri_config` 已移出拦截契约——命令
//! 注册表查找经注入的 `command_lookup` 闭包 mock（ACP 协议面注册表语义
//! 由装配面承载，返回 `ResolvedCommand`，假 handler 执行）；
//! compact 配置经注入闭包返回默认值。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_acp_types::{
    command::{
        command_handler::CommandHandler, command_route::RouteEntry, CommandContext,
        CommandFeedback, CommandOutcome, CommandResult, FeedbackChannel, FeedbackLevel,
        PromptStopReason, ResolvedCommand,
    },
    compact::CompactConfig,
    event::{EventSink, ExecutorEvent},
    messages::{BaseMessage, MessageContent},
};
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use super::{
    emit_command_feedback, intercept_immediate_command, InterceptOutcome, InterceptRequest,
};

// ── Mock EventSink ─────────────────────────────────────────────────────────

/// Mock EventSink，记录所有 push_done 调用（含 request_id）。
struct MockEventSink {
    push_done_count: Mutex<usize>,
    push_done_request_ids: Mutex<Vec<Option<String>>>,
    push_done_stop_reasons: Mutex<Vec<String>>,
    pushed_events: Mutex<Vec<String>>,
}

impl MockEventSink {
    fn new() -> Self {
        Self {
            push_done_count: Mutex::new(0),
            push_done_request_ids: Mutex::new(Vec::new()),
            push_done_stop_reasons: Mutex::new(Vec::new()),
            pushed_events: Mutex::new(Vec::new()),
        }
    }

    fn push_done_count(&self) -> usize {
        *self.push_done_count.lock().unwrap()
    }
}

#[async_trait]
impl EventSink for MockEventSink {
    async fn push_event(&self, _session_id: &str, event: &ExecutorEvent, _context_window: u32) {
        let json = serde_json::to_string(event).unwrap_or_default();
        self.pushed_events.lock().unwrap().push(json);
    }

    async fn push_done(&self, _session_id: &str, stop_reason: &str, request_id: Option<&str>) {
        *self.push_done_count.lock().unwrap() += 1;
        self.push_done_request_ids
            .lock()
            .unwrap()
            .push(request_id.map(String::from));
        self.push_done_stop_reasons
            .lock()
            .unwrap()
            .push(stop_reason.to_string());
    }
}

// ── Helper 工厂函数 ─────────────────────────────────────────────────────────

/// 构造最小 InterceptRequest（auxiliary_model / thread_store / frozen 等均为 None）。
///
/// `command_lookup` 为注入的注册表查找 mock（None = 未注册，走 agent 管线；
/// Some = 命中，执行由 CommandOutcome 承载）。
#[allow(clippy::too_many_arguments)]
fn make_intercept_request<'a>(
    content: &'a MessageContent,
    history: &'a [BaseMessage],
    session_id: &'a str,
    cancel: &'a AgentCancellationToken,
    event_sink: &'a Arc<dyn EventSink>,
    _bg_event_tx: &'a tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
    task_manager: &'a Arc<dyn peri_acp_types::tasks::TaskManager>,
    command_lookup: super::super::CommandLookupFn,
) -> InterceptRequest<'a> {
    let compact_config_loader: Arc<dyn Fn() -> CompactConfig + Send + Sync> =
        Arc::new(CompactConfig::default);
    InterceptRequest {
        mcp_pool: None,
        content,
        history,
        history_payloads: history
            .iter()
            .cloned()
            .map(peri_acp_types::store::PersistedPayload::Message)
            .collect(),
        cwd: "/tmp",
        session_id,
        cancel,
        session_resources: None,
        thread_id: None,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        frozen_system_prompt: None,
        event_sink,
        auxiliary_model: &None,
        task_manager,
        command_lookup,
        compact_config_loader,
    }
}

/// 构造共享的 bg registry + bg channel（拦截测试不实际触发 bg，但需要传入句柄）。
fn make_bg_infra() -> (
    tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
    Arc<dyn peri_acp_types::tasks::TaskManager>,
) {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<ExecutorEvent>();
    let registry = Arc::new(crate::agent::async_tasks::TaskManager::new())
        as Arc<dyn peri_acp_types::tasks::TaskManager>;
    (tx, registry)
}

// ── intercept_immediate_command: 路径分支测试 ─────────────────────────────

/// 普通 slash 命令（非 Immediate 注册）：不在注入注册表中 → PassThrough
#[tokio::test]
async fn test_intercept_args_parse_failure_returns_error_feedback() {
    // Arrange：rewind 形态 schema（required positional + flag）；handler 为
    // 哨兵——若被调用即 panic（解析失败必须不进入 handler）。
    struct SentryHandler;
    #[async_trait]
    impl CommandHandler for SentryHandler {
        async fn execute(&self, _ctx: CommandContext) -> CommandOutcome {
            panic!("解析失败路径不得进入 handler");
        }
    }
    let schema = peri_acp_types::command::ArgsSchema {
        positionals: vec![peri_acp_types::command::ArgSpec {
            name: "target_message_id".into(),
            kind: peri_acp_types::command::ArgKind::String,
            required: true,
            description: None,
        }],
        named: vec![],
        flags: vec![peri_acp_types::command::FlagSpec {
            name: "no-revert-files".into(),
            short: None,
            description: None,
        }],
    };
    let lookup: super::super::CommandLookupFn = Arc::new(move |text: &str| {
        if text == "rewind" {
            Some(ResolvedCommand {
                entry: Arc::new(RouteEntry {
                    fullname: "core:rewind".to_string(),
                    aliases: vec![],
                    description: "rewind for args-parse test".to_string(),
                    kind: peri_acp_types::command::command_route::CommandEntryKind::Command,
                    category: None,
                    args_schema: Some(schema.clone()),
                    handler: Arc::new(SentryHandler),
                    provenance: peri_acp_types::command::command_route::CommandProvenance {
                        source: peri_acp_types::command::command_route::CommandSource::Core,
                        lifecycle:
                            peri_acp_types::command::command_route::CommandLifecycle::Connected,
                    },
                }),
                args: String::new(),
            })
        } else {
            None
        }
    });

    let content = MessageContent::text("/rewind");
    let history: Vec<BaseMessage> = vec![BaseMessage::human("hello")];
    let cancel = AgentCancellationToken::new();
    let mock_sink = Arc::new(MockEventSink::new());
    let sink: Arc<dyn EventSink> = Arc::clone(&mock_sink) as Arc<dyn EventSink>;
    let (bg_tx, bg_reg) = make_bg_infra();
    let req = make_intercept_request(
        &content,
        &history,
        "test-session",
        &cancel,
        &sink,
        &bg_tx,
        &bg_reg,
        lookup,
    );

    // Act
    let result = intercept_immediate_command(req).await;

    // Assert：Handled + feedback(Error, 参数解析失败) + history 原样 + push_done
    let InterceptOutcome::Handled(prompt_result) = result else {
        panic!("解析失败应返回 Handled");
    };
    assert!(prompt_result.ok);
    assert_eq!(prompt_result.stop_reason, PromptStopReason::EndTurn);
    assert_eq!(
        prompt_result.messages.len(),
        1,
        "解析失败应返回原样 history"
    );
    // feedback 经 emit_command_feedback 发射为 CommandFeedback 事件
    let events = mock_sink.pushed_events.lock().unwrap();
    let fb_event = events.iter().find(|json| json.contains("command_feedback"));
    assert!(fb_event.is_some(), "解析失败应发射 CommandFeedback 事件");
    assert!(
        fb_event.unwrap().contains("rewind 参数解析失败"),
        "错误消息应含 'rewind 参数解析失败'，实际: {events:?}"
    );
    drop(events);
    assert_eq!(
        mock_sink.push_done_count(),
        1,
        "解析失败路径必须调用 push_done（TRAP 守护）"
    );
}

/// args 解析通过：schema 声明 required positional，args 提供 → 正常进入
/// handler（SentryHandler 替换为正常 handler）。
#[tokio::test]
async fn test_intercept_args_parse_ok_passes_into_handler() {
    // Arrange：rewind 形态 schema + 正常 handler（Done + history 原样）
    struct OkHandler;
    #[async_trait]
    impl CommandHandler for OkHandler {
        async fn execute(&self, ctx: CommandContext) -> CommandOutcome {
            assert_eq!(
                ctx.args, "abc123 --no-revert-files",
                "ctx.args 应为 resolve 切分原文"
            );
            // P1-1：统一解析结果经 ctx.parsed_args 传入——handler 不再自研解析
            let parsed = ctx
                .parsed_args
                .as_ref()
                .expect("解析通过路径应携带 parsed_args");
            assert_eq!(
                parsed.positionals,
                vec!["abc123".to_string()],
                "positionals[0] 应为 target_message_id"
            );
            assert_eq!(
                parsed.flags,
                vec!["no-revert-files".to_string()],
                "flags 应命中 no-revert-files"
            );
            CommandOutcome::Done(CommandResult {
                messages: ctx.history,
                stop_reason: PromptStopReason::EndTurn,
                feedback: None,
            })
        }
    }
    let schema = peri_acp_types::command::ArgsSchema {
        positionals: vec![peri_acp_types::command::ArgSpec {
            name: "target_message_id".into(),
            kind: peri_acp_types::command::ArgKind::String,
            required: true,
            description: None,
        }],
        named: vec![],
        flags: vec![peri_acp_types::command::FlagSpec {
            name: "no-revert-files".into(),
            short: None,
            description: None,
        }],
    };
    let lookup: super::super::CommandLookupFn = Arc::new(move |text: &str| {
        if text.starts_with("rewind") {
            Some(ResolvedCommand {
                entry: Arc::new(RouteEntry {
                    fullname: "core:rewind".to_string(),
                    aliases: vec![],
                    description: "rewind for args-parse test".to_string(),
                    kind: peri_acp_types::command::command_route::CommandEntryKind::Command,
                    category: None,
                    args_schema: Some(schema.clone()),
                    handler: Arc::new(OkHandler),
                    provenance: peri_acp_types::command::command_route::CommandProvenance {
                        source: peri_acp_types::command::command_route::CommandSource::Core,
                        lifecycle:
                            peri_acp_types::command::command_route::CommandLifecycle::Connected,
                    },
                }),
                // resolve 词法切分（不变式 3）：命令名后的参数原样
                args: "abc123 --no-revert-files".to_string(),
            })
        } else {
            None
        }
    });

    let content = MessageContent::text("/rewind abc123 --no-revert-files");
    let history: Vec<BaseMessage> = vec![];
    let cancel = AgentCancellationToken::new();
    let mock_sink = Arc::new(MockEventSink::new());
    let sink: Arc<dyn EventSink> = Arc::clone(&mock_sink) as Arc<dyn EventSink>;
    let (bg_tx, bg_reg) = make_bg_infra();
    let req = make_intercept_request(
        &content,
        &history,
        "test-session",
        &cancel,
        &sink,
        &bg_tx,
        &bg_reg,
        lookup,
    );

    // Act
    let result = intercept_immediate_command(req).await;

    // Assert：Handled + handler 已执行（OkHandler 内断言 ctx.args）
    let InterceptOutcome::Handled(prompt_result) = result else {
        panic!("解析通过应返回 Handled");
    };
    assert!(prompt_result.ok);
    assert_eq!(mock_sink.push_done_count(), 1, "解析通过路径必须 push_done");
}

// ── emit_command_feedback: 反馈双通道验证 ───────────────────────────────────

/// 构造带 feedback 的 CommandResult（messages 预置一条 human 消息）。
fn result_with_feedback(channel: FeedbackChannel) -> CommandResult {
    CommandResult {
        messages: vec![BaseMessage::human("你好")],
        stop_reason: PromptStopReason::EndTurn,
        feedback: Some(CommandFeedback {
            level: FeedbackLevel::Info,
            message: "命令已完成".to_string(),
            channel,
        }),
    }
}

/// channel=Session：message 以系统消息追加进 messages 尾部，事件发射一次
/// （Step 1：编排层统一反馈出口；Session 仅命令显式 opt-in，设计 §79）。
#[tokio::test]
async fn test_emit_command_feedback_session_appends_system_message() {
    // Arrange
    let mock_sink = Arc::new(MockEventSink::new());
    let sink: Arc<dyn EventSink> = Arc::clone(&mock_sink) as Arc<dyn EventSink>;
    let mut result = result_with_feedback(FeedbackChannel::Session);

    // Act
    emit_command_feedback(&sink, "test-session", &mut result).await;

    // Assert：尾部为系统消息（内容同 feedback.message）
    let messages = &result.messages;
    assert_eq!(messages.len(), 2, "Session 通道应追加一条系统消息");
    let last = messages.last().unwrap();
    assert!(
        matches!(last, BaseMessage::System { .. }),
        "尾元素应为系统消息"
    );
    assert_eq!(last.content(), "命令已完成");
    // feedback 已被 take（发射唯一归属本 helper），事件发射一次
    assert!(result.feedback.is_none(), "feedback 应被 take 出");
    assert_eq!(
        mock_sink.pushed_events.lock().unwrap().len(),
        1,
        "Session 通道也应发射 CommandFeedback 事件"
    );
}

/// channel=UiOnly：messages 不变（不追加系统消息），事件仍发射
#[tokio::test]
async fn test_emit_command_feedback_ui_only_keeps_messages() {
    // Arrange
    let mock_sink = Arc::new(MockEventSink::new());
    let sink: Arc<dyn EventSink> = Arc::clone(&mock_sink) as Arc<dyn EventSink>;
    let mut result = result_with_feedback(FeedbackChannel::UiOnly);

    // Act
    emit_command_feedback(&sink, "test-session", &mut result).await;

    // Assert：messages 不变（UiOnly 不进会话，设计 §79）
    assert_eq!(result.messages.len(), 1, "UiOnly 不应追加消息");
    assert!(result.feedback.is_none(), "feedback 应被 take 出");
    assert_eq!(
        mock_sink.pushed_events.lock().unwrap().len(),
        1,
        "UiOnly 仍应发射 CommandFeedback 事件"
    );
}
