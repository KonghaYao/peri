//! 从 act.rs 分离的测试模块
use super::*;
use crate::agent::events_v2::{EventBus, EventHandles, RenderEvent};
use crate::agent::react::{AgentOutput, Reasoning, ToolCall};
use crate::agent::stages::{MiddlewareChain, StageContext};
use crate::error::{AgentError, AgentResult};
use crate::messages::{BaseMessage, ContentBlock, MessageContent};
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

/// TurnCompleted 快照是观察出口：Responses 原生历史载荷（reasoning 密文与来源身份）
/// 不得随事件离开进程，用户可见的派生内容必须保留。
#[tokio::test]
async fn test_turn_completed_snapshot_strips_provider_native_history() {
    let (ctx, mut handles) = make_context_with_handles();
    {
        let mut transcript = ctx.session.transcript.write();
        transcript.append(BaseMessage::ai(MessageContent::Blocks(vec![
            ContentBlock::text("答案正文"),
            ContentBlock::responses_native_history(serde_json::json!({
                "version": 1,
                "source": {"nonce": "ab".repeat(16), "digest": "cd".repeat(32)},
                "items": [{
                    "type": "reasoning",
                    "id": "rs_1",
                    "encrypted_content": "CIPHERTEXT-REASONING",
                    "status": "completed",
                }],
            })),
        ])));
    }

    let reasoning = Reasoning::with_answer("thinking", "final answer");
    run_act(ActInput {
        context: ctx.clone(),
        reasoning,
        catalog: ctx.runtime.tool_catalog.snapshot(),
    })
    .await
    .unwrap();

    let mut turn_completed = None;
    while let Some(ev) = handles.try_render() {
        if let RenderEvent::TurnCompleted {
            finalized_messages, ..
        } = ev
        {
            turn_completed = Some(finalized_messages);
        }
    }
    let wire = serde_json::to_string(&*turn_completed.expect("TurnCompleted 必须 emit"))
        .expect("serialize snapshot");
    assert!(
        !wire.contains("CIPHERTEXT-REASONING"),
        "事件快照不得携带密文"
    );
    assert!(!wire.contains(&"cd".repeat(32)), "事件快照不得携带来源摘要");
    assert!(wire.contains("答案正文"), "可见文本必须保留");
    // 脱敏是「载荷替换」而非「消息消失」：占位块仍带确定 tag，消费方无需理解载荷。
    assert!(
        wire.contains("responses_native_history") && wire.contains("[REDACTED]"),
        "原生历史消息必须仍在快照中（载荷为占位）"
    );

    // transcript 自身保持原消息：外层脱敏不得改写事实源
    let stored = serde_json::to_string(&{
        let guard = ctx.session.transcript.read();
        guard
            .visible_messages()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>()
    })
    .expect("serialize transcript");
    assert!(
        stored.contains("CIPHERTEXT-REASONING"),
        "transcript 必须保留完整原生记录"
    );
}
