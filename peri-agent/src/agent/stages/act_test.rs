//! 从 act.rs 分离的测试模块
use super::*;
use crate::agent::events_v2::{EventBus, EventHandles, RenderEvent};
use crate::agent::react::{AgentOutput, Reasoning, ToolCall};
use crate::agent::stages::{MiddlewareChain, StageContext};
use crate::error::{AgentError, AgentResult};
use crate::middleware::capabilities as hook_state;
use crate::middleware::Middleware;
use crate::session::store::FrozenContext;
use crate::session::Session;
use std::sync::Arc;

fn make_context() -> StageContext {
    let cwd: Arc<str> = Arc::from("/tmp/test");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    StageContext::new(turn, session.transcript(), session.queue().clone())
}

/// 构造带 EventHandles 的 StageContext（测试可订阅 render 事件断言 TurnCompleted）
fn make_context_with_handles() -> (StageContext, EventHandles) {
    let cwd: Arc<str> = Arc::from("/tmp/test");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    let (bus, handles) = EventBus::new(Default::default());
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_event_bus(Arc::new(bus))
        .build();
    (ctx, handles)
}

/// after_agent 恒失败的测试中间件（S5.3：验证失败路径仍 emit TurnCompleted）
struct FailingAfterAgentMiddleware;

#[async_trait::async_trait]
impl Middleware for FailingAfterAgentMiddleware {
    fn name(&self) -> &str {
        "failing-after-agent"
    }

    async fn after_agent(
        &self,
        _state: &mut dyn hook_state::AfterAgentState,
        _output: &AgentOutput,
    ) -> AgentResult<AgentOutput> {
        Err(AgentError::MiddlewareError {
            middleware: "failing-after-agent".to_string(),
            reason: "test failure".to_string(),
        })
    }
}

#[tokio::test]
async fn test_act_no_tool_calls_writes_answer() {
    let ctx = make_context();
    let reasoning = Reasoning::with_answer("thinking", "final answer");
    let input = ActInput {
        context: ctx.clone(),
        reasoning,
        catalog: ctx.runtime.tool_catalog.snapshot(),
        receipt: None,
    };
    let output = run_act(input).await.unwrap();
    assert!(!output.has_tool_calls);
    assert_eq!(output.final_answer.as_deref(), Some("final answer"));

    // transcript 应包含 final_answer 消息
    let messages: Vec<_> = {
        let guard = ctx.session.transcript.read();
        guard.visible_messages().into_iter().cloned().collect()
    };
    assert!(
        messages.iter().any(|m| m.content() == "final answer"),
        "transcript 应写入最终回答"
    );
}

#[tokio::test]
async fn test_act_with_tool_calls_dispatches() {
    let ctx = make_context();
    // 工具不存在，dispatch 会返回 not_found 错误结果
    let tool_call = ToolCall::new("call_1", "Read", serde_json::json!({"path": "/tmp"}));
    let reasoning = Reasoning::with_tools("need to read file", vec![tool_call]);
    let input = ActInput {
        context: ctx.clone(),
        reasoning,
        catalog: ctx.runtime.tool_catalog.snapshot(),
        receipt: None,
    };
    let output = run_act(input).await.unwrap();
    assert!(output.has_tool_calls, "有 tool_calls 时应标记");
    assert!(output.final_answer.is_none());

    // transcript 应包含 AI 消息 + tool_result
    let messages: Vec<_> = {
        let guard = ctx.session.transcript.read();
        guard.visible_messages().into_iter().cloned().collect()
    };
    assert!(
        messages.len() >= 2,
        "应有 AI 消息 + tool_result，实际 {} 条",
        messages.len()
    );
}

/// S5.3：run_after_agent 失败时仍必须 emit TurnCompleted——
/// 最终回答已 append 到 transcript，TUI committed 视图必须与 transcript 一致。
#[tokio::test]
async fn test_act_after_agent_failure_emits_turn_completed() {
    let cwd: Arc<str> = Arc::from("/tmp/test");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    let (bus, mut handles) = EventBus::new(Default::default());
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(FailingAfterAgentMiddleware));
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_event_bus(Arc::new(bus))
        .with_middleware_chain(Arc::new(chain))
        .build();

    let reasoning = Reasoning::with_answer("thinking", "final answer");
    let result = run_act(ActInput {
        context: ctx.clone(),
        reasoning,
        catalog: ctx.runtime.tool_catalog.snapshot(),
        receipt: None,
    })
    .await;
    assert!(result.is_err(), "run_after_agent 失败应传播错误");

    // TurnCompleted 必须已 emit，且快照包含已 append 的最终回答
    let mut turn_completed = None;
    while let Some(ev) = handles.try_render() {
        if let RenderEvent::TurnCompleted {
            finalized_messages, ..
        } = ev
        {
            turn_completed = Some(finalized_messages);
        }
    }
    let msgs = turn_completed.expect("run_after_agent 失败路径必须 emit TurnCompleted");
    assert!(
        msgs.iter().any(|m| m.content() == "final answer"),
        "TurnCompleted 快照应包含已 append 的最终回答"
    );
}

/// S5.3 镜像：工具路径 cancel（dispatch_tools 失败）时仍必须 emit TurnCompleted。
#[tokio::test]
async fn test_act_tool_path_cancel_emits_turn_completed() {
    let (ctx, mut handles) = make_context_with_handles();
    ctx.session.turn.cancel_token.cancel();

    let tool_call = ToolCall::new("call_1", "Read", serde_json::json!({"path": "/tmp"}));
    let reasoning = Reasoning::with_tools("need to read file", vec![tool_call]);
    let result = run_act(ActInput {
        context: ctx.clone(),
        reasoning,
        catalog: ctx.runtime.tool_catalog.snapshot(),
        receipt: None,
    })
    .await;
    assert!(
        matches!(result, Err(AgentError::Interrupted)),
        "cancel 应返回 Interrupted"
    );

    let mut found = false;
    while let Some(ev) = handles.try_render() {
        if matches!(ev, RenderEvent::TurnCompleted { .. }) {
            found = true;
        }
    }
    assert!(found, "工具路径 cancel 必须 emit TurnCompleted");
}

#[tokio::test]
async fn turn_completed_waits_for_capacity_in_production_emitter() {
    let mut ctx = make_context();
    let (bus, mut handles) = EventBus::new(crate::agent::events_v2::EventBusConfig {
        render_capacity: 1,
        ..Default::default()
    });
    ctx.runtime.event_bus = Arc::new(bus);
    ctx.runtime
        .event_bus
        .emit_render(RenderEvent::TurnSuspended {
            turn_id: ctx.turn_id(),
            agent_id: ctx.session.agent_id,
        });
    let send = emit_turn_completed(&ctx);
    tokio::pin!(send);
    assert!(
        std::future::poll_fn(|cx| {
            std::task::Poll::Ready(std::future::Future::poll(send.as_mut(), cx).is_pending())
        })
        .await
    );
    assert!(!ctx.runtime.event_bus.health().dirty);
    handles.render_rx.recv().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), send)
        .await
        .unwrap();
    assert!(matches!(
        handles.try_render(),
        Some(RenderEvent::TurnCompleted { .. })
    ));
    assert!(!ctx.runtime.event_bus.health().dirty);
}

// ─── 有效提交与后台结果覆盖（T01/T07）───────────────────────────────────────────

fn terminal_result(
    mailbox: &crate::session::async_result_mailbox::AsyncResultMailbox,
) -> (
    crate::messages::MessageId,
    crate::session::async_result_mailbox::RequestReceipt,
) {
    use peri_acp_types::session::MessageSource;
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };
    let id = crate::messages::MessageId::new();
    let reminder = TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSource("shell".into()),
            kind: "terminal".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: "task shell-act completed exit_code: 0".into(),
            summary: None,
            metadata: serde_json::json!({ "task_id": "shell-act" }),
        })
        .unwrap();
    mailbox
        .accept(id, &reminder, MessageSource::ShellComplete)
        .unwrap();
    mailbox
        .mark_canonical_ready(id, &reminder, MessageSource::ShellComplete, None)
        .unwrap();
    let receipt = mailbox
        .capture_request("attempt-act", "request-act", &[id])
        .expect("request must capture the ready result");
    (id, receipt)
}

/// [T01] 最终回答有效写入并完成持久化 barrier 后，本次 receipt 才 Covered。
#[tokio::test]
async fn async_result_act_commit_covers_requested_result() {
    let ctx = make_context();
    let (id, receipt) = terminal_result(&ctx.session.async_result_mailbox);
    let reasoning = Reasoning::with_answer("thinking", "final answer");
    run_act(ActInput {
        context: ctx.clone(),
        reasoning,
        catalog: ctx.runtime.tool_catalog.snapshot(),
        receipt: Some(receipt),
    })
    .await
    .unwrap();

    assert_eq!(
        ctx.session.async_result_mailbox.pending_count(),
        0,
        "有效提交后本次请求的结果必须 Covered"
    );
    assert!(
        ctx.session
            .async_result_mailbox
            .protected_snapshot()
            .is_empty(),
        "{id:?} 不应再进入下一次请求"
    );
}

/// [T07] 空最终回答不是有效提交：保留责任并 Blocked，不能提前 Covered。
#[tokio::test]
async fn async_result_act_empty_response_keeps_responsibility() {
    let ctx = make_context();
    let (id, receipt) = terminal_result(&ctx.session.async_result_mailbox);
    let reasoning = Reasoning::with_answer("thinking", "");
    run_act(ActInput {
        context: ctx.clone(),
        reasoning,
        catalog: ctx.runtime.tool_catalog.snapshot(),
        receipt: Some(receipt),
    })
    .await
    .unwrap();

    assert_eq!(
        ctx.session.async_result_mailbox.pending_count(),
        1,
        "空回复不得结清处理责任"
    );
    let snapshot = ctx.session.async_result_mailbox.protected_snapshot();
    assert_eq!(snapshot.items.len(), 1);
    assert_eq!(snapshot.items[0].delivery_id, id);
    assert_eq!(
        snapshot.items[0].state,
        crate::session::async_result_mailbox::AsyncResultState::Blocked
    );
    assert_eq!(
        snapshot.items[0].diagnostic.as_ref().unwrap().stage,
        "act",
        "诊断必须指出失败阶段"
    );
}

/// [T18] 工具调用回答按“AI 消息 + 工具结果配对提交”结算：配对提交后
/// 才算有效处理；工具执行中断时保留责任，恢复只能基于已提交历史。
#[tokio::test]
async fn async_result_act_tool_call_settles_after_pair_commit_or_keeps_on_failure() {
    // 1) 配对提交成功：工具结果已落盘，本次处理责任才算结清。
    let ctx = make_context();
    let (_id, receipt) = terminal_result(&ctx.session.async_result_mailbox);
    let tool_call = ToolCall::new("call_1", "Read", serde_json::json!({"path": "/tmp"}));
    let reasoning = Reasoning::with_tools("need to read file", vec![tool_call]);
    run_act(ActInput {
        context: ctx.clone(),
        reasoning,
        catalog: ctx.runtime.tool_catalog.snapshot(),
        receipt: Some(receipt),
    })
    .await
    .unwrap();

    assert_eq!(
        ctx.session.async_result_mailbox.pending_count(),
        0,
        "配对提交完成后本次责任可以结清"
    );
    let messages: Vec<_> = {
        let guard = ctx.session.transcript.read();
        guard.visible_messages().into_iter().cloned().collect()
    };
    assert!(
        messages.len() >= 2,
        "AI 工具调用消息与工具结果都必须保留，实际 {} 条",
        messages.len()
    );

    // 2) 工具执行中断：没有配对提交，责任保留且不伪造 Covered。
    let ctx = make_context();
    let (id, receipt) = terminal_result(&ctx.session.async_result_mailbox);
    ctx.session.turn.cancel_token.cancel();
    let tool_call = ToolCall::new("call_2", "Read", serde_json::json!({"path": "/tmp"}));
    let reasoning = Reasoning::with_tools("need to read file", vec![tool_call]);
    let outcome = run_act(ActInput {
        context: ctx.clone(),
        reasoning,
        catalog: ctx.runtime.tool_catalog.snapshot(),
        receipt: Some(receipt),
    })
    .await;
    assert!(outcome.is_err(), "中断必须向调用方传播");

    assert_eq!(
        ctx.session.async_result_mailbox.pending_count(),
        1,
        "未完成配对提交时不得清空处理责任"
    );
    assert_eq!(
        ctx.session.async_result_mailbox.state_of(&id),
        Some(crate::session::async_result_mailbox::AsyncResultState::InFlight),
        "结果仍停留在本次请求内，等待后续有效提交"
    );
    assert!(
        ctx.session.async_result_mailbox.retains_body(&id),
        "未覆盖时提醒正文必须保留，供后续请求重投影"
    );
}

/// [T06] 持久化 barrier 失败：正文没有确认落盘时不得 Covered；
/// 本次执行失败、责任转为可诊断的 Blocked，等待显式恢复。
#[tokio::test]
async fn async_result_act_barrier_failure_keeps_blocked() {
    use crate::session::test_resources::mock::MockSessionResources;
    use crate::session::transcript::MessageTranscript;

    let ctx = make_context();
    let (id, receipt) = terminal_result(&ctx.session.async_result_mailbox);

    let store = MockSessionResources::new();
    store.register_bound_session("thread-barrier", "/tmp/test");
    // 后端拒绝写入：canonical append 进入 sticky 失败，barrier 必须报错。
    store.restrict_to_history_read_only();
    *ctx.session.transcript.write() =
        MessageTranscript::new().with_persistence(store, "thread-barrier".to_string());

    let reasoning = Reasoning::with_answer("thinking", "final answer");
    let outcome = run_act(ActInput {
        context: ctx.clone(),
        reasoning,
        catalog: ctx.runtime.tool_catalog.snapshot(),
        receipt: Some(receipt),
    })
    .await;
    assert!(outcome.is_err(), "barrier 失败必须让本次执行失败");

    assert_eq!(
        ctx.session.async_result_mailbox.pending_count(),
        1,
        "未确认落盘前不得清空处理责任"
    );
    assert_eq!(
        ctx.session.async_result_mailbox.state_of(&id),
        Some(crate::session::async_result_mailbox::AsyncResultState::Blocked),
        "barrier 失败必须转为 Blocked，而不是 Covered"
    );
    let snapshot = ctx.session.async_result_mailbox.protected_snapshot();
    let diagnostic = snapshot.items[0]
        .diagnostic
        .as_ref()
        .expect("Blocked 必须带诊断");
    assert_eq!(diagnostic.stage, "act");
    assert!(
        diagnostic.summary.contains("persistence barrier"),
        "诊断需指出 barrier 失败：{}",
        diagnostic.summary
    );
}
