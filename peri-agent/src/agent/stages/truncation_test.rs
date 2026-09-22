//! 输出截断必须继续或明确停止，不能提交为成功。

use super::*;
use crate::agent::react::{AgentOutput, Reasoning, StreamingContext, ToolCall};
use crate::error::AgentResult;
use crate::middleware::{capabilities::AfterAgentState, Middleware};
use crate::session::{queue::MessageSource, store::FrozenContext, Session};
use peri_model::{
    ContentBlock as ModelContentBlock, ModelCapabilities, ModelMessage, ModelRequest,
    ModelResponse, ModelStream, ModelStreamEvent,
};
use std::collections::VecDeque;
use std::sync::atomic::AtomicUsize;

struct ScriptedModel {
    responses: parking_lot::Mutex<VecDeque<Reasoning>>,
    requests: parking_lot::Mutex<Vec<Vec<BaseMessage>>>,
    cancel_on_call: Option<usize>,
    tool_executions: Arc<AtomicUsize>,
}

struct CountingTool(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl BaseTool for CountingTool {
    fn name(&self) -> &str {
        "Count"
    }
    fn description(&self) -> &str {
        "Record one execution"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    fn is_direct(&self) -> bool {
        true
    }
    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok("executed".into())
    }
}

#[async_trait::async_trait]
impl ReactLLM for ScriptedModel {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _tools: &[&dyn BaseTool],
        streaming: Option<StreamingContext>,
    ) -> AgentResult<Reasoning> {
        let call = {
            let mut requests = self.requests.lock();
            requests.push(messages.to_vec());
            requests.len()
        };
        if self.cancel_on_call == Some(call) {
            streaming.unwrap().cancel.cancel();
        }
        Ok(self.responses.lock().pop_front().expect("不得额外请求模型"))
    }
}

struct CompletionCounter(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Middleware for CompletionCounter {
    fn name(&self) -> &str {
        "completion_counter"
    }

    async fn after_agent(
        &self,
        _state: &mut dyn AfterAgentState,
        output: &AgentOutput,
    ) -> AgentResult<AgentOutput> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(output.clone())
    }
}

fn make_interrupted(text: &str, max_attempts: u32) -> Reasoning {
    let mut response = Reasoning::with_answer(text, text);
    response.source_message = Some(BaseMessage::ai(text));
    response.stream_interruption = Some(crate::agent::react::StreamInterruption {
        error: peri_model::ModelError::stream_interrupted(Some("anthropic"), Some("req-partial")),
        attempts: 1,
        max_attempts,
    });
    response
}

/// [回归测试] 中断后的部分消息进入下一轮历史，完成 hook 只在恢复成功后运行。
#[tokio::test]
async fn test_stream_interruption_continues_from_saved_message() {
    let partial = make_interrupted("部分正文", 2);
    let source = partial.source_message.clone().unwrap();
    let (ctx, model, completions) =
        make_context(vec![partial, Reasoning::with_answer("", "done")], None);
    let result = run_react_loop(ctx, 10).await;
    assert!(matches!(result, LoopResult::Completed), "{result:?}");
    let requests = model.requests.lock();
    assert_eq!(requests.len(), 2);
    assert!(requests[1]
        .iter()
        .any(|m| m.id() == source.id() && m.content() == source.content()));
    assert!(requests[1].iter().any(|m| m
        .content()
        .to_string()
        .contains("参数被截断的工具调用需要重新发起")));
    assert_eq!(completions.load(Ordering::SeqCst), 1);
}

/// [回归测试] 预算取事件而非另立常量，耗尽仍保留最后一条部分响应和错误事实。
#[tokio::test]
async fn test_stream_interruption_exhausts_event_budget() {
    for budget in [1, 2, 4] {
        let (ctx, model, completions) = make_context(
            vec![make_interrupted("partial", budget); budget as usize],
            None,
        );
        let result = run_react_loop(ctx.clone(), 10).await;
        match result {
            LoopResult::Error(crate::error::AgentError::StreamRecoveryExhausted {
                attempts,
                source,
            }) => {
                assert_eq!(attempts, budget as usize);
                assert_eq!(source.diagnostic().category_name(), "stream_interrupted");
                assert_eq!(source.request_id(), Some("req-partial"));
            }
            other => panic!("预期恢复耗尽：{other:?}"),
        }
        assert_eq!(model.requests.lock().len(), budget as usize);
        assert_eq!(completions.load(Ordering::SeqCst), 0);
        assert!(ctx.session.queue.is_empty());
        assert_eq!(
            ctx.session
                .transcript
                .read()
                .visible_messages()
                .iter()
                .filter(|m| matches!(m, BaseMessage::Ai { .. }))
                .count(),
            budget as usize
        );
    }
}

#[tokio::test]
async fn test_stream_interruption_respects_cancel_and_iteration_limit() {
    let (ctx, _, completions) = make_context(vec![make_interrupted("partial", 4)], Some(1));
    assert!(matches!(
        run_react_loop(ctx, 10).await,
        LoopResult::Interrupted
    ));
    assert_eq!(completions.load(Ordering::SeqCst), 0);
    let (ctx, model, completions) = make_context(vec![make_interrupted("partial", 4)], None);
    assert!(matches!(
        run_react_loop(ctx, 1).await,
        LoopResult::Error(crate::error::AgentError::MaxIterationsExceeded(1))
    ));
    assert_eq!(model.requests.lock().len(), 1);
    assert_eq!(completions.load(Ordering::SeqCst), 0);
}

/// [回归测试] 续跑提醒必须是可信、必达的 Defer，而不是用户消息或 Info。
#[test]
fn test_stream_interruption_reminder_contract() {
    use crate::session::queue::{MessageKind, QueuedPayload};
    use peri_acp_types::system_reminder::{ReminderCategory, ReminderDelivery, ReminderSeverity};
    let (ctx, _, _) = make_context(vec![], None);
    ctx.session.queue.drain_all();
    enqueue_stream_interruption_continuation(&ctx);
    let queued = ctx.session.queue.drain_all();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].kind, MessageKind::Defer);
    assert_eq!(queued[0].source, MessageSource::SystemInjected);
    let QueuedPayload::SystemReminder(trusted) = &queued[0].payload else {
        panic!("必须保留可信来源")
    };
    let reminder = trusted.as_reminder();
    assert_eq!(reminder.category, ReminderCategory::Guidance);
    assert_eq!(reminder.source.0, "model_runtime");
    assert_eq!(reminder.kind, "stream_interrupted");
    assert_eq!(reminder.severity, ReminderSeverity::Warning);
    assert_eq!(reminder.delivery, ReminderDelivery::Required);
}

/// [回归测试] 完整工具仍执行一次，但不能重置本轮中断恢复预算。
#[tokio::test]
async fn test_stream_interruption_tool_progress_does_not_reset_budget() {
    let tool = Reasoning::with_tools(
        "",
        vec![ToolCall::new("call-1", "Count", serde_json::json!({}))],
    );
    let (ctx, model, completions) = make_context(
        vec![make_interrupted("a", 2), tool, make_interrupted("b", 2)],
        None,
    );
    assert!(matches!(
        run_react_loop(ctx, 10).await,
        LoopResult::Error(crate::error::AgentError::StreamRecoveryExhausted { attempts: 2, .. })
    ));
    assert_eq!(model.requests.lock().len(), 3);
    assert_eq!(model.tool_executions.load(Ordering::SeqCst), 1);
    assert_eq!(completions.load(Ordering::SeqCst), 0);
}

/// 走真实 `AgentModelBridge`：第一次无正文中断（仅思考 + 半截工具），第二次正常完成。
struct InterruptedThenDone {
    requests: parking_lot::Mutex<Vec<ModelRequest>>,
}

#[async_trait::async_trait]
impl peri_model::Model for InterruptedThenDone {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> peri_model::ModelResult<ModelStream> {
        let call = {
            let mut requests = self.requests.lock();
            requests.push(request);
            requests.len()
        };
        let events = if call == 1 {
            // cut_tool_use / 纯思考形态：可见标志已置位，但正文为空。
            vec![
                Ok(ModelStreamEvent::ReasoningDelta {
                    text: "先想一下".into(),
                }),
                Ok(ModelStreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some("call-partial".into()),
                    name: Some("Count".into()),
                    arguments_delta: "{\"".into(),
                }),
                Ok(ModelStreamEvent::Interrupted {
                    error: peri_model::ModelError::stream_interrupted(
                        Some("anthropic"),
                        Some("req-cut"),
                    ),
                    attempts: 1,
                    max_attempts: 2,
                }),
            ]
        } else {
            let response = ModelResponse::new(
                ModelMessage::assistant_text("done"),
                peri_model::StopReason::EndTurn,
                None,
                None,
            )
            .expect("valid response");
            vec![Ok(ModelStreamEvent::Completed(response))]
        };
        Ok(ModelStream::with_parent_cancellation(
            futures::stream::iter(events),
            cancellation,
        ))
    }
}

struct BridgeHarness {
    ctx: StageContext,
    model: Arc<InterruptedThenDone>,
    completions: Arc<AtomicUsize>,
    tool_executions: Arc<AtomicUsize>,
}

fn make_bridge_harness() -> BridgeHarness {
    let session = Session::new(
        Arc::from("/tmp/stream-interruption-test"),
        FrozenContext::builder().build(),
        None,
    );
    let model = Arc::new(InterruptedThenDone {
        requests: parking_lot::Mutex::new(Vec::new()),
    });
    let bridge = crate::agent::model_bridge::AgentModelBridge::new(model.clone());
    let tool_executions = Arc::new(AtomicUsize::new(0));
    let mut tools: BTreeMap<String, Arc<dyn BaseTool>> = BTreeMap::new();
    tools.insert(
        "Count".into(),
        Arc::new(CountingTool(tool_executions.clone())),
    );
    let completions = Arc::new(AtomicUsize::new(0));
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(CompletionCounter(completions.clone())));
    let ctx = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(bridge))
    .with_tools(Arc::new(RwLock::new(tools)))
    .with_middleware_chain(Arc::new(chain))
    .build();
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("finish the task"),
    ));
    BridgeHarness {
        ctx,
        model,
        completions,
        tool_executions,
    }
}

fn assistant_texts(request: &ModelRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .filter_map(|message| match message {
            ModelMessage::Assistant { content, .. } => Some(
                content
                    .iter()
                    .filter_map(|block| match block {
                        ModelContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect()
}

/// [回归测试] 无正文中断（只收到思考或半截工具）不得把空 assistant 消息写进规范历史。
///
/// 历史背景：`Interrupted` 分支曾无条件挂 `source_message`，正文为空时会写入空
/// assistant 消息；provider 编码后得到空 text block，Anthropic 以 400
/// `text content blocks must be non-empty` 拒收，使本可续跑的断流变成下一轮硬失败。
#[tokio::test]
async fn test_stream_interruption_without_text_keeps_history_clean() {
    let harness = make_bridge_harness();
    let result = run_react_loop(harness.ctx.clone(), 10).await;
    assert!(matches!(result, LoopResult::Completed), "{result:?}");
    let requests = harness.model.requests.lock();
    assert_eq!(requests.len(), 2, "无正文中断仍必须有界续跑");
    let texts = assistant_texts(&requests[1]);
    assert!(
        texts.is_empty(),
        "下一轮请求不得包含空 assistant 消息：{texts:?}"
    );
    assert!(
        serde_json::to_string(&requests[1])
            .expect("request serializes")
            .contains("参数被截断的工具调用需要重新发起"),
        "续跑必须靠可信提醒驱动"
    );
    drop(requests);
    let transcript = harness.ctx.session.transcript.read();
    let visible = transcript.visible_messages();
    assert!(
        visible
            .iter()
            .all(|message| !matches!(message, BaseMessage::Ai { .. })
                || !message.message_content().is_empty()),
        "空 assistant 消息不得进入规范历史"
    );
    assert_eq!(
        harness.tool_executions.load(Ordering::SeqCst),
        0,
        "半截工具参数不得执行"
    );
    assert_eq!(
        harness.completions.load(Ordering::SeqCst),
        1,
        "只有恢复成功才触发完成 hook"
    );
}

fn make_truncated(answer: &str) -> Reasoning {
    let mut response = Reasoning::with_answer("unfinished thinking", answer);
    response.stop_reason = peri_model::StopReason::MaxTokens;
    response
}

fn make_context(
    responses: Vec<Reasoning>,
    cancel_on_call: Option<usize>,
) -> (StageContext, Arc<ScriptedModel>, Arc<AtomicUsize>) {
    let session = Session::new(
        Arc::from("/tmp/truncation-test"),
        FrozenContext::builder().build(),
        None,
    );
    let model = Arc::new(ScriptedModel {
        responses: parking_lot::Mutex::new(responses.into()),
        requests: parking_lot::Mutex::new(Vec::new()),
        cancel_on_call,
        tool_executions: Arc::new(AtomicUsize::new(0)),
    });
    let mut tools: BTreeMap<String, Arc<dyn BaseTool>> = BTreeMap::new();
    tools.insert(
        "Count".into(),
        Arc::new(CountingTool(model.tool_executions.clone())),
    );
    let completions = Arc::new(AtomicUsize::new(0));
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(CompletionCounter(completions.clone())));
    let ctx = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(model.clone())
    .with_tools(Arc::new(RwLock::new(tools)))
    .with_middleware_chain(Arc::new(chain))
    .build();
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("finish the task"),
    ));
    (ctx, model, completions)
}

/// [回归测试] thinking-only 和半句正文截断都必须继续，只有完整回答触发 Stop hook。
#[tokio::test]
async fn test_truncation_continues_without_premature_completion() {
    for partial in ["", "I'll start exploring"] {
        let source = BaseMessage::ai(crate::messages::MessageContent::Blocks(vec![
            crate::messages::ContentBlock::reasoning_with_signature(
                "unfinished thinking",
                "fixture-signature",
            ),
            crate::messages::ContentBlock::text(partial),
        ]));
        let mut truncated = make_truncated(partial);
        truncated.source_message = Some(source.clone());
        let (ctx, model, completions) =
            make_context(vec![truncated, Reasoning::with_answer("", "done")], None);
        let result = run_react_loop(ctx.clone(), 10).await;
        assert!(matches!(result, LoopResult::Completed), "{result:?}");
        let requests = model.requests.lock();
        assert_eq!(requests.len(), 2, "截断响应不能当作成功结束");
        let saved = requests[1]
            .iter()
            .find(|m| m.id() == source.id())
            .expect("续跑保留原始消息身份");
        assert_eq!(
            serde_json::to_value(saved).unwrap(),
            serde_json::to_value(&source).unwrap(),
            "正文、思考和签名块必须完整保留"
        );
        assert!(requests[1]
            .iter()
            .any(|m| m.content().to_string().contains("output token limit")));
        assert_eq!(completions.load(Ordering::SeqCst), 1);
        assert!(ctx
            .session
            .transcript
            .read()
            .visible_messages()
            .iter()
            .any(|m| m.content() == "done"));
    }
}

/// [回归测试] 连续截断只允许两次续跑，保留最后响应但不能调用完成 hook。
#[tokio::test]
async fn test_truncation_repeated_responses_stop_with_bounded_attempts() {
    let (ctx, model, completions) = make_context(vec![make_truncated("partial"); 3], None);
    let result = run_react_loop(ctx.clone(), 10).await;
    assert_eq!(model.requests.lock().len(), 3);
    assert!(
        matches!(
            result,
            LoopResult::Error(crate::error::AgentError::OutputTruncated { attempts: 3 })
        ),
        "{result:?}"
    );
    assert_eq!(completions.load(Ordering::SeqCst), 0);
    assert!(ctx.session.queue.is_empty(), "耗尽后不能留下额外续跑请求");
    assert_eq!(
        ctx.session
            .transcript
            .read()
            .visible_messages()
            .iter()
            .filter(|m| matches!(m, BaseMessage::Ai { .. }))
            .count(),
        3
    );
}

/// 完整工具结果是进展，必须继续且重置连续无工具截断预算。
#[tokio::test]
async fn test_truncation_complete_tool_call_continues_and_resets_budget() {
    let mut tool_response = Reasoning::with_tools(
        "",
        vec![ToolCall::new("call-1", "Count", serde_json::json!({}))],
    );
    tool_response.stop_reason = peri_model::StopReason::MaxTokens;
    let (ctx, model, completions) = make_context(
        vec![
            make_truncated("a"),
            make_truncated("b"),
            tool_response,
            make_truncated("c"),
            make_truncated("d"),
            Reasoning::with_answer("", "done"),
        ],
        None,
    );
    let result = run_react_loop(ctx.clone(), 10).await;
    assert!(matches!(result, LoopResult::Completed), "{result:?}");
    assert_eq!(model.requests.lock().len(), 6);
    assert_eq!(completions.load(Ordering::SeqCst), 1);
    assert_eq!(
        model.tool_executions.load(Ordering::SeqCst),
        1,
        "真实分派只能执行一次工具副作用"
    );
    assert_eq!(
        ctx.session
            .transcript
            .read()
            .visible_messages()
            .iter()
            .filter(|m| matches!(m, BaseMessage::Tool { .. }))
            .count(),
        1,
        "工具调用不能在续跑时被重放"
    );
}

#[tokio::test]
async fn test_truncation_continuation_respects_cancel() {
    let (ctx, model, completions) = make_context(vec![make_truncated("partial"); 2], Some(2));
    let result = run_react_loop(ctx, 10).await;
    assert!(matches!(result, LoopResult::Interrupted), "{result:?}");
    assert_eq!(model.requests.lock().len(), 2);
    assert_eq!(completions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn test_truncation_continuation_respects_iteration_limit() {
    let (ctx, model, completions) = make_context(vec![make_truncated("partial")], None);
    let result = run_react_loop(ctx, 1).await;
    assert!(
        matches!(
            result,
            LoopResult::Error(crate::error::AgentError::MaxIterationsExceeded(1))
        ),
        "{result:?}"
    );
    assert_eq!(model.requests.lock().len(), 1);
    assert_eq!(completions.load(Ordering::SeqCst), 0);
}
