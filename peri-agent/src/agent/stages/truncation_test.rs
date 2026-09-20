//! 输出截断必须继续或明确停止，不能提交为成功。

use super::*;
use crate::agent::react::{AgentOutput, Reasoning, StreamingContext, ToolCall};
use crate::error::AgentResult;
use crate::middleware::{capabilities::AfterAgentState, Middleware};
use crate::session::{queue::MessageSource, store::FrozenContext, Session};
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
