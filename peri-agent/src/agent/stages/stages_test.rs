//! 从 mod.rs 分离的测试模块
use super::*;
use crate::messages::MessageContent;
use crate::session::queue::MessageSource;
use crate::session::store::FrozenContext;
use crate::session::Session;

/// 构造测试用 StageContext
fn make_stage_context() -> StageContext {
    let cwd: Arc<str> = Arc::from("/tmp/test");
    let frozen = FrozenContext::builder()
        .system_prompt("You are a test agent.")
        .build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    StageContext::new(turn, session.transcript(), session.queue().clone())
}

// ── 类型契约测试 ──

#[test]
fn test_compact_input_output_contract() {
    let ctx = make_stage_context();
    let input = CompactInput {
        context: ctx,
        has_tool_calls: false,
    };
    assert!(!input.has_tool_calls);

    let output = CompactOutput { compacted: false };
    assert!(!output.compacted);
}

#[test]
fn test_receive_input_output_contract() {
    let ctx = make_stage_context();
    let _input = ReceiveInput { context: ctx };
    let output = ReceiveOutput { consumed_count: 0 };
    assert_eq!(output.consumed_count, 0);
}

#[test]
fn test_reason_input_output_contract() {
    let ctx = make_stage_context();
    let _input = ReasonInput {
        context: ctx,
        has_tool_calls: false,
    };
    let reasoning = crate::agent::react::Reasoning::with_answer("thinking", "answer");
    let output = ReasonOutput {
        reasoning,
        messages_snapshot: std::sync::Arc::new(vec![]),
    };
    assert!(!output.reasoning.needs_tool_call());
    assert!(output.messages_snapshot.is_empty());
}

#[test]
fn test_act_input_output_contract() {
    let ctx = make_stage_context();
    let reasoning = crate::agent::react::Reasoning::with_answer("thinking", "done");
    let _input = ActInput {
        context: ctx,
        reasoning,
    };

    let output_with_tools = ActOutput {
        has_tool_calls: true,
        final_answer: None,
    };
    assert!(output_with_tools.has_tool_calls);
    assert!(output_with_tools.final_answer.is_none());

    let output_no_tools = ActOutput {
        has_tool_calls: false,
        final_answer: Some("done".to_string()),
    };
    assert!(!output_no_tools.has_tool_calls);
    assert_eq!(output_no_tools.final_answer.as_deref(), Some("done"));
}

#[test]
fn test_stage_context_construction() {
    let ctx = make_stage_context();
    assert_eq!(&*ctx.session.turn.cwd, "/tmp/test");
    assert_eq!(ctx.session.turn.current_step(), 0);
    assert!(ctx.session.queue.is_empty());
    assert!(ctx.session.transcript.read().is_empty());
}

#[test]
fn test_stage_context_builder_default() {
    // builder 不传 llm 时，自动 fallback 到 NullReactLLM
    let cwd: Arc<str> = Arc::from("/tmp");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone()).build();
    assert_eq!(ctx.runtime.llm.model_name(), "null");
}

// ── e2e 集成测试（验证完整 v2 ReAct 循环）──

/// Mock LLM：首轮返回 final_answer，无 tool_calls
struct FinalAnswerLLM {
    answer: &'static str,
}
#[async_trait::async_trait]
impl ReactLLM for FinalAnswerLLM {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        Ok(crate::agent::react::Reasoning::with_answer(
            "thinking",
            self.answer,
        ))
    }
    fn model_name(&self) -> String {
        "mock-final-answer".to_string()
    }
}

struct CountingFinalAnswerLLM {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    answer: &'static str,
}

#[async_trait::async_trait]
impl ReactLLM for CountingFinalAnswerLLM {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(crate::agent::react::Reasoning::with_answer(
            "thinking",
            self.answer,
        ))
    }
}

struct IterationBudgetProbe {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    prompt_visible: Arc<std::sync::Mutex<Vec<bool>>>,
    prompt_marker: &'static str,
    recall_marker: &'static str,
}

#[async_trait::async_trait]
impl crate::middleware::Middleware for IterationBudgetProbe {
    fn name(&self) -> &str {
        "IterationBudgetProbe"
    }

    async fn before_agent(
        &self,
        state: &mut dyn crate::middleware::MiddlewareState,
    ) -> crate::error::AgentResult<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.prompt_visible.lock().unwrap().push(
            state
                .messages()
                .iter()
                .any(|message| message.content().contains(self.prompt_marker)),
        );
        state.push_recall(self.recall_marker.to_string());
        Ok(())
    }
}

struct OneToolCallLLM(Arc<std::sync::atomic::AtomicUsize>);

#[async_trait::async_trait]
impl ReactLLM for OneToolCallLLM {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        let call = self.0.fetch_add(1, Ordering::SeqCst);
        assert_eq!(call, 0, "预算耗尽后不得发起第二次模型调用");
        Ok(crate::agent::react::Reasoning::with_tools(
            "use the deterministic tool",
            vec![crate::agent::react::ToolCall::new(
                "iteration-budget-tool-call",
                "iteration_budget_tool",
                serde_json::json!({}),
            )],
        ))
    }
}

struct IterationBudgetTool(Arc<std::sync::atomic::AtomicUsize>);

#[async_trait::async_trait]
impl crate::tools::BaseTool for IterationBudgetTool {
    fn name(&self) -> &str {
        "iteration_budget_tool"
    }

    fn description(&self) -> &str {
        "deterministic iteration budget regression tool"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }

    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok("iteration budget tool result".to_string())
    }
}

#[derive(Debug, Default)]
struct LoopEventSummary {
    stage_lifecycle: Vec<(Stage, bool)>,
    llm_start_steps: Vec<usize>,
    llm_end_steps: Vec<usize>,
}

fn drain_loop_observe_events(
    handles: &mut crate::agent::events_v2::EventHandles,
) -> LoopEventSummary {
    let mut summary = LoopEventSummary::default();
    while let Some(event) = handles.try_observe() {
        match event {
            ObserveEvent::StageStarted { stage, .. } => {
                summary.stage_lifecycle.push((stage, false));
            }
            ObserveEvent::StageEnded { stage, status, .. } => {
                assert_eq!(status, StageStatus::Done, "阶段必须以 Done 成对结束");
                summary.stage_lifecycle.push((stage, true));
            }
            ObserveEvent::LlmCallStart { step, .. } => summary.llm_start_steps.push(step),
            ObserveEvent::LlmCallEnd { step, .. } => summary.llm_end_steps.push(step),
            _ => {}
        }
    }
    summary
}

fn expected_stage_lifecycle(stages: &[Stage]) -> Vec<(Stage, bool)> {
    stages
        .iter()
        .flat_map(|stage| [(*stage, false), (*stage, true)])
        .collect()
}

fn assert_single_turn_completed(
    handles: &mut crate::agent::events_v2::EventHandles,
    expected_steps: usize,
) {
    let completed_steps: Vec<_> = std::iter::from_fn(|| handles.try_render())
        .filter_map(|event| match event {
            crate::agent::events_v2::RenderEvent::TurnCompleted { steps, .. } => Some(steps),
            _ => None,
        })
        .collect();
    assert_eq!(completed_steps, vec![expected_steps]);
}

/// [回归测试] 最后一轮语义工作产出 final answer 后，下一次 Receive 必须观察正常完成。
///
/// 历史背景：旧循环把整个 Receive→Act 外层 `for` 计入预算，limit=1 时 Act 已提交
/// final answer，却在下一次 Receive 前直接误报 MaxIterationsExceeded。
#[tokio::test]
async fn test_run_react_loop_final_answer_at_iteration_limit_completes() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let session = Session::new(
        Arc::from("/tmp/iteration-budget-final"),
        FrozenContext::builder().build(),
        None,
    );
    let turn = session.start_turn();
    let (bus, mut handles) = crate::agent::events_v2::EventBus::new(Default::default());
    let context = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(Arc::new(CountingFinalAnswerLLM {
            calls: Arc::clone(&calls),
            answer: "done at the limit",
        }))
        .with_event_bus(Arc::new(bus))
        .build();
    context.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("final prompt"),
    ));

    let result = run_react_loop(context.clone(), 1).await;

    assert!(matches!(result, LoopResult::Completed));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(context.session.turn.current_step(), 1);
    let events = drain_loop_observe_events(&mut handles);
    assert_eq!(
        events.stage_lifecycle,
        expected_stage_lifecycle(&[
            Stage::Receive,
            Stage::Compact,
            Stage::Reason,
            Stage::Act,
            Stage::Receive,
        ])
    );
    assert_eq!(events.llm_start_steps, vec![1]);
    assert_eq!(events.llm_end_steps, vec![1]);
    assert_single_turn_completed(&mut handles, 1);
}

/// [回归测试] 零预算仍必须先进入 Receive，空队列由 Receive 唯一判定正常完成。
///
/// 历史背景：预算门禁若放在 Receive 前，max_iterations=0 会把无需语义工作的空 turn
/// 错误分类为超限，并破坏 Receive 作为正常退出唯一入口的架构契约。
#[tokio::test]
async fn test_run_react_loop_empty_queue_with_zero_budget_completes_in_receive() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let session = Session::new(
        Arc::from("/tmp/iteration-budget-empty-zero"),
        FrozenContext::builder().build(),
        None,
    );
    let turn = session.start_turn();
    let (bus, mut handles) = crate::agent::events_v2::EventBus::new(Default::default());
    let context = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(Arc::new(CountingFinalAnswerLLM {
            calls: Arc::clone(&calls),
            answer: "must not run",
        }))
        .with_event_bus(Arc::new(bus))
        .build();

    let result = run_react_loop(context.clone(), 0).await;

    assert!(matches!(result, LoopResult::Completed));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(context.session.turn.current_step(), 0);
    let events = drain_loop_observe_events(&mut handles);
    assert_eq!(
        events.stage_lifecycle,
        expected_stage_lifecycle(&[Stage::Receive])
    );
    assert!(events.llm_start_steps.is_empty());
    assert!(events.llm_end_steps.is_empty());
}

/// [回归测试] 有待处理 prompt 但语义预算为零时，只允许 Receive 消费消息。
///
/// 历史背景：预算检查需要位于 Receive 与语义阶段之间，既不能跳过消息消费，也不能
/// 推进 step、运行 before_agent、调用模型或工具。
#[tokio::test]
async fn test_run_react_loop_prompt_with_zero_budget_returns_max_iterations() {
    let before_agent_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let prompt_visible = Arc::new(std::sync::Mutex::new(Vec::new()));
    let llm_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tool_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut chain = crate::middleware::MiddlewareChain::new();
    chain.add(Box::new(IterationBudgetProbe {
        calls: Arc::clone(&before_agent_calls),
        prompt_visible: Arc::clone(&prompt_visible),
        prompt_marker: "zero budget prompt",
        recall_marker: "zero budget recall",
    }));
    let tools: SharedToolMap = Arc::new(parking_lot::RwLock::new(BTreeMap::from([(
        "iteration_budget_tool".to_string(),
        Arc::new(IterationBudgetTool(Arc::clone(&tool_calls))) as Arc<dyn crate::tools::BaseTool>,
    )])));
    let session = Session::new(
        Arc::from("/tmp/iteration-budget-prompt-zero"),
        FrozenContext::builder().build(),
        None,
    );
    let turn = session.start_turn();
    let (bus, mut handles) = crate::agent::events_v2::EventBus::new(Default::default());
    let context = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(Arc::new(CountingFinalAnswerLLM {
            calls: Arc::clone(&llm_calls),
            answer: "must not run",
        }))
        .with_tools(tools)
        .with_middleware_chain(Arc::new(chain))
        .with_event_bus(Arc::new(bus))
        .build();
    context.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("zero budget prompt"),
    ));

    let result = run_react_loop(context.clone(), 0).await;

    assert!(matches!(
        result,
        LoopResult::Error(crate::error::AgentError::MaxIterationsExceeded(0))
    ));
    assert_eq!(llm_calls.load(Ordering::SeqCst), 0);
    assert_eq!(tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(before_agent_calls.load(Ordering::SeqCst), 0);
    assert!(prompt_visible.lock().unwrap().is_empty());
    assert!(context.recall_buffer.read().is_empty());
    assert_eq!(context.session.turn.current_step(), 0);
    assert_eq!(context.session.transcript.read().len(), 1);
    let events = drain_loop_observe_events(&mut handles);
    assert_eq!(
        events.stage_lifecycle,
        expected_stage_lifecycle(&[Stage::Receive])
    );
    assert!(events.llm_start_steps.is_empty());
    assert!(events.llm_end_steps.is_empty());
}

/// [回归测试] 工具结果确实需要下一次推理时，耗尽的预算必须拒绝新语义迭代。
///
/// 历史背景：final answer 的收尾 Receive 可以越过预算，但工具调用后的空 Receive
/// 仍代表需要继续 Reason，必须精确返回 MaxIterationsExceeded(limit)。
#[tokio::test]
async fn test_run_react_loop_required_reason_beyond_limit_returns_max_iterations() {
    let llm_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tool_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tools: SharedToolMap = Arc::new(parking_lot::RwLock::new(BTreeMap::from([(
        "iteration_budget_tool".to_string(),
        Arc::new(IterationBudgetTool(Arc::clone(&tool_calls))) as Arc<dyn crate::tools::BaseTool>,
    )])));
    let session = Session::new(
        Arc::from("/tmp/iteration-budget-tool"),
        FrozenContext::builder().build(),
        None,
    );
    let turn = session.start_turn();
    let (bus, mut handles) = crate::agent::events_v2::EventBus::new(Default::default());
    let context = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(Arc::new(OneToolCallLLM(Arc::clone(&llm_calls))))
        .with_tools(tools)
        .with_event_bus(Arc::new(bus))
        .build();
    context.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("use one tool"),
    ));

    let result = run_react_loop(context.clone(), 1).await;

    assert!(matches!(
        result,
        LoopResult::Error(crate::error::AgentError::MaxIterationsExceeded(1))
    ));
    assert_eq!(llm_calls.load(Ordering::SeqCst), 1);
    assert_eq!(tool_calls.load(Ordering::SeqCst), 1);
    assert_eq!(context.session.turn.current_step(), 1);
    let events = drain_loop_observe_events(&mut handles);
    assert_eq!(
        events.stage_lifecycle,
        expected_stage_lifecycle(&[
            Stage::Receive,
            Stage::Compact,
            Stage::Reason,
            Stage::Act,
            Stage::Receive,
        ])
    );
    assert_eq!(events.llm_start_steps, vec![1]);
    assert_eq!(events.llm_end_steps, vec![1]);
    assert_single_turn_completed(&mut handles, 1);
}

/// [回归测试] idle await_wake 与随后重试的 Receive 不得消耗语义迭代预算。
///
/// 历史背景：旧外层 `for` 把首次空 Receive 的 idle 挂起算作一次迭代，limit=1 时
/// prompt 唤醒后尚未运行 Reason 就误报超限；before_agent 也不得在挂起前提前执行。
#[tokio::test]
async fn test_run_react_loop_idle_wake_does_not_consume_iteration_budget() {
    let before_agent_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let prompt_visible = Arc::new(std::sync::Mutex::new(Vec::new()));
    let llm_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let should_wait = Arc::new(AtomicBool::new(true));
    let suspended = Arc::new(AtomicBool::new(false));
    let mut chain = crate::middleware::MiddlewareChain::new();
    chain.add(Box::new(IterationBudgetProbe {
        calls: Arc::clone(&before_agent_calls),
        prompt_visible: Arc::clone(&prompt_visible),
        prompt_marker: "idle wake prompt",
        recall_marker: "idle wake recall",
    }));
    let session = Session::new(
        Arc::from("/tmp/iteration-budget-idle-wake"),
        FrozenContext::builder().build(),
        None,
    );
    let inbox = Arc::new(crate::agent::session::SessionInbox::new(Arc::new(
        session.queue().clone(),
    )));
    let handle = inbox.handle();
    let turn = session.start_turn();
    let (bus, mut handles) = crate::agent::events_v2::EventBus::new(Default::default());
    let context = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(Arc::new(CountingFinalAnswerLLM {
            calls: Arc::clone(&llm_calls),
            answer: "done after wake",
        }))
        .with_middleware_chain(Arc::new(chain))
        .with_event_bus(Arc::new(bus))
        .with_idle_inbox(inbox)
        .with_idle_should_wait({
            let should_wait = Arc::clone(&should_wait);
            Arc::new(move || should_wait.load(Ordering::Acquire))
        })
        .with_idle_suspended_flag(Arc::clone(&suspended))
        .build();
    let loop_context = context.clone();
    let loop_task = tokio::spawn(async move { run_react_loop(loop_context, 1).await });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !suspended.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("循环必须进入可观测的 idle suspended 状态");
    assert_eq!(before_agent_calls.load(Ordering::SeqCst), 0);
    should_wait.store(false, Ordering::Release);
    handle.push_prompt(
        MessageSource::UserInput,
        BaseMessage::human("idle wake prompt"),
    );

    let result = tokio::time::timeout(std::time::Duration::from_secs(1), loop_task)
        .await
        .expect("唤醒后的循环必须在有界时间内结束")
        .expect("循环任务不得 panic");

    assert!(matches!(result, LoopResult::Completed));
    assert_eq!(llm_calls.load(Ordering::SeqCst), 1);
    assert_eq!(before_agent_calls.load(Ordering::SeqCst), 1);
    assert_eq!(*prompt_visible.lock().unwrap(), vec![true]);
    assert_eq!(
        context.recall_buffer.read().as_slice(),
        ["idle wake recall"]
    );
    assert_eq!(context.session.turn.current_step(), 1);
    assert!(!suspended.load(Ordering::Acquire));
    let events = drain_loop_observe_events(&mut handles);
    assert_eq!(
        events.stage_lifecycle,
        expected_stage_lifecycle(&[
            Stage::Receive,
            Stage::Receive,
            Stage::Compact,
            Stage::Reason,
            Stage::Act,
            Stage::Receive,
        ])
    );
    assert_eq!(events.llm_start_steps, vec![1]);
    assert_eq!(events.llm_end_steps, vec![1]);
    assert_single_turn_completed(&mut handles, 1);
}

#[tokio::test]
async fn test_e2e_final_answer_no_tools() {
    // e2e：推入 Prompt → run_react_loop → 直接 final_answer → Completed
    let cwd: Arc<str> = Arc::from("/tmp/e2e");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(Arc::new(FinalAnswerLLM {
            answer: "task completed",
        }))
        .build();

    // 推入用户输入
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human(MessageContent::text("do the task")),
    ));

    let result = run_react_loop(ctx.clone(), 10).await;
    assert!(
        matches!(result, LoopResult::Completed),
        "expected Completed, got {:?}",
        result
    );

    // transcript 应包含：[user_prompt, ai_final_answer]
    let transcript = ctx.session.transcript.read();
    let visible: Vec<_> = transcript.visible_messages().into_iter().collect();
    assert_eq!(
        visible.len(),
        2,
        "expected 2 messages (user + ai), got {}",
        visible.len()
    );
    assert!(matches!(visible[0], BaseMessage::Human { .. }));
    assert!(matches!(visible[1], BaseMessage::Ai { .. }));
}

#[tokio::test]
async fn test_e2e_cancel_before_loop() {
    // e2e：cancel_token 在 run_react_loop 之前触发 → Interrupted
    let cwd: Arc<str> = Arc::from("/tmp/e2e-cancel");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(Arc::new(FinalAnswerLLM {
            answer: "should not reach",
        }))
        .build();

    // 立即 cancel
    ctx.session.turn.cancel_token.cancel();

    let result = run_react_loop(ctx, 10).await;
    assert!(
        matches!(result, LoopResult::Interrupted),
        "expected Interrupted, got {:?}",
        result
    );
}

#[tokio::test]
async fn test_e2e_empty_queue_completes_immediately() {
    // e2e：无 Prompt 推入 → Receive 阶段 consumed=0 → Completed
    let cwd: Arc<str> = Arc::from("/tmp/e2e-empty");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(Arc::new(FinalAnswerLLM { answer: "answer" }))
        .build();

    // 不推入 Prompt，直接跑循环（首轮 Receive consumed=0 → 直接退出）
    let result = run_react_loop(ctx.clone(), 0).await;
    assert!(
        matches!(result, LoopResult::Completed),
        "expected Completed, got {:?}",
        result
    );

    // RCRA：空队列立即退出，不会进入 Reason/Act，transcript 为空
    let transcript = ctx.session.transcript.read();
    assert!(
        transcript.is_empty(),
        "expected empty transcript on immediate exit"
    );
}

#[tokio::test]
async fn test_p0_2_before_agent_runs_once_after_tool_round_trip() {
    use std::collections::BTreeMap;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };

    struct ToolRoundTripLLM(Arc<AtomicUsize>);

    #[async_trait::async_trait]
    impl ReactLLM for ToolRoundTripLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn crate::tools::BaseTool],
            _streaming: Option<crate::agent::react::StreamingContext>,
        ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
            match self.0.fetch_add(1, Ordering::SeqCst) {
                0 => Ok(crate::agent::react::Reasoning::with_tools(
                    "use the local tool",
                    vec![crate::agent::react::ToolCall::new(
                        "p0-2-tool-call",
                        "p0_2_local_tool",
                        serde_json::json!({}),
                    )],
                )),
                1 => {
                    assert!(
                        messages
                            .iter()
                            .any(|message| message.content().contains("p0-2 tool result marker")),
                        "second LLM call must observe the local tool result"
                    );
                    Ok(crate::agent::react::Reasoning::with_answer("", "done"))
                }
                call => panic!("unexpected LLM call {call}"),
            }
        }
    }

    struct LocalTool(Arc<AtomicUsize>);

    #[async_trait::async_trait]
    impl crate::tools::BaseTool for LocalTool {
        fn name(&self) -> &str {
            "p0_2_local_tool"
        }

        fn description(&self) -> &str {
            "deterministic local test tool"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }

        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok("p0-2 tool result marker".to_string())
        }
    }

    struct BeforeAgentProbe {
        calls: Arc<AtomicUsize>,
        prompt_visible: Arc<Mutex<Vec<bool>>>,
    }

    #[async_trait::async_trait]
    impl crate::middleware::Middleware for BeforeAgentProbe {
        fn name(&self) -> &str {
            "BeforeAgentProbe"
        }

        async fn before_agent(
            &self,
            state: &mut dyn crate::middleware::MiddlewareState,
        ) -> crate::error::AgentResult<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.prompt_visible.lock().unwrap().push(
                state
                    .messages()
                    .iter()
                    .any(|message| message.content().contains("p0-2 prompt marker")),
            );
            state.push_recall("p0-2 recall marker".to_string());
            Ok(())
        }
    }

    let before_agent_calls = Arc::new(AtomicUsize::new(0));
    let prompt_visible = Arc::new(Mutex::new(Vec::new()));
    let llm_calls = Arc::new(AtomicUsize::new(0));
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let mut chain = crate::middleware::MiddlewareChain::new();
    chain.add(Box::new(BeforeAgentProbe {
        calls: Arc::clone(&before_agent_calls),
        prompt_visible: Arc::clone(&prompt_visible),
    }));
    let tools: SharedToolMap = Arc::new(parking_lot::RwLock::new(BTreeMap::from([(
        "p0_2_local_tool".to_string(),
        Arc::new(LocalTool(Arc::clone(&tool_calls))) as Arc<dyn crate::tools::BaseTool>,
    )])));

    let session = Session::new(
        Arc::from("/tmp/p0-2-before-agent-tool-round-trip"),
        FrozenContext::builder().build(),
        None,
    );
    let turn = session.start_turn();
    let context = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(Arc::new(ToolRoundTripLLM(Arc::clone(&llm_calls))))
        .with_tools(tools)
        .with_middleware_chain(Arc::new(chain))
        .build();
    context.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("p0-2 prompt marker"),
    ));

    assert!(matches!(
        run_react_loop(context.clone(), 10).await,
        LoopResult::Completed
    ));
    assert_eq!(llm_calls.load(Ordering::SeqCst), 2);
    assert_eq!(tool_calls.load(Ordering::SeqCst), 1);
    assert_eq!(before_agent_calls.load(Ordering::SeqCst), 1);
    assert_eq!(*prompt_visible.lock().unwrap(), vec![true]);
    assert_eq!(
        context.recall_buffer.read().as_slice(),
        ["p0-2 recall marker"]
    );
}

#[tokio::test]
async fn test_p0_2_before_agent_runs_once_after_receive_and_skips_empty_or_cancelled_turns() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };

    struct CountingLLM(Arc<AtomicUsize>);

    #[async_trait::async_trait]
    impl ReactLLM for CountingLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn crate::tools::BaseTool],
            _streaming: Option<crate::agent::react::StreamingContext>,
        ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(crate::agent::react::Reasoning::with_answer("", "done"))
        }
    }

    struct BeforeAgentProbe {
        calls: Arc<AtomicUsize>,
        prompt_visible: Arc<Mutex<Vec<bool>>>,
    }

    #[async_trait::async_trait]
    impl crate::middleware::Middleware for BeforeAgentProbe {
        fn name(&self) -> &str {
            "BeforeAgentProbe"
        }

        async fn before_agent(
            &self,
            state: &mut dyn crate::middleware::MiddlewareState,
        ) -> crate::error::AgentResult<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.prompt_visible.lock().unwrap().push(
                state
                    .messages()
                    .iter()
                    .any(|message| message.content().contains("p0-2 prompt marker")),
            );
            state.push_recall("p0-2 recall marker".to_string());
            Ok(())
        }
    }

    let calls = Arc::new(AtomicUsize::new(0));
    let prompt_visible = Arc::new(Mutex::new(Vec::new()));
    let llm_calls = Arc::new(AtomicUsize::new(0));
    let mut chain = crate::middleware::MiddlewareChain::new();
    chain.add(Box::new(BeforeAgentProbe {
        calls: Arc::clone(&calls),
        prompt_visible: Arc::clone(&prompt_visible),
    }));

    let cwd: Arc<str> = Arc::from("/tmp/p0-2-before-agent");
    let session = Session::new(cwd, FrozenContext::builder().build(), None);
    let turn = session.start_turn();
    let context = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(Arc::new(CountingLLM(Arc::clone(&llm_calls))))
        .with_middleware_chain(Arc::new(chain))
        .build();
    context.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("p0-2 prompt marker"),
    ));

    assert!(matches!(
        run_react_loop(context.clone(), 10).await,
        LoopResult::Completed
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(*prompt_visible.lock().unwrap(), vec![true]);
    assert_eq!(llm_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        context.recall_buffer.read().as_slice(),
        ["p0-2 recall marker"]
    );

    let empty_session = Session::new(
        Arc::from("/tmp/p0-2-before-agent-empty"),
        FrozenContext::builder().build(),
        None,
    );
    let empty_turn = empty_session.start_turn();
    let empty_context = StageContext::builder(
        empty_turn,
        empty_session.transcript(),
        empty_session.queue().clone(),
    )
    .with_llm(Arc::new(CountingLLM(Arc::clone(&llm_calls))))
    .with_middleware_chain(context.runtime.middleware_chain.clone())
    .build();
    assert!(matches!(
        run_react_loop(empty_context.clone(), 10).await,
        LoopResult::Completed
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(empty_context.recall_buffer.read().is_empty());
    assert_eq!(llm_calls.load(Ordering::SeqCst), 1);

    let cancelled_session = Session::new(
        Arc::from("/tmp/p0-2-before-agent-cancelled"),
        FrozenContext::builder().build(),
        None,
    );
    let cancelled_turn = cancelled_session.start_turn();
    let cancelled_context = StageContext::builder(
        cancelled_turn,
        cancelled_session.transcript(),
        cancelled_session.queue().clone(),
    )
    .with_llm(Arc::new(CountingLLM(Arc::clone(&llm_calls))))
    .with_middleware_chain(context.runtime.middleware_chain.clone())
    .build();
    cancelled_context.session.turn.cancel_token.cancel();
    assert!(matches!(
        run_react_loop(cancelled_context.clone(), 10).await,
        LoopResult::Interrupted
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(cancelled_context.recall_buffer.read().is_empty());
    assert_eq!(llm_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn test_append_messages_prompt_kept_as_is() {
    // Prompt 消息应原样 append（用户输入不包裹 reminder）
    let ctx = make_stage_context();
    let msgs = vec![QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human(MessageContent::text("hello user")),
    )];
    {
        let mut transcript = ctx.session.transcript.write();
        append_messages_to_transcript(&mut transcript, msgs);
    }
    let transcript = ctx.session.transcript.read();
    assert_eq!(transcript.len(), 1);
    let content = transcript.entries()[0].message.content();
    assert_eq!(content, "hello user");
}

#[test]
fn test_append_messages_empty_prompt_skipped() {
    // keepgoing：空 Prompt（真实 payload 为 `MessageContent::text("")`，见
    // peri-tui submit_consumer handle_keepgoing_submit）驱动 loop 继续但不写入
    // transcript——用户没有输入新内容，历史中不应出现空 user 消息。
    let ctx = make_stage_context();
    let msgs = vec![QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human(MessageContent::text("")),
    )];
    {
        let mut transcript = ctx.session.transcript.write();
        append_messages_to_transcript(&mut transcript, msgs);
    }
    let transcript = ctx.session.transcript.read();
    assert_eq!(transcript.len(), 0, "空 Prompt 不应写入 transcript");
}

#[test]
fn test_append_messages_whitespace_prompt_kept() {
    // 空白文本不算空——与 peri-acp `is_keepgoing` 的 content-block 判空一致：
    // 按 content block 判空（`Blocks([Image])` 等纯附件消息不应被误判为空），
    // 而非按 text trim 判空；用户输入空格应正常写入。
    let ctx = make_stage_context();
    let msgs = vec![QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human(MessageContent::text("   ")),
    )];
    {
        let mut transcript = ctx.session.transcript.write();
        append_messages_to_transcript(&mut transcript, msgs);
    }
    let transcript = ctx.session.transcript.read();
    assert_eq!(transcript.len(), 1, "空白 Prompt 应正常写入 transcript");
}

#[test]
fn test_append_messages_info_wrapped_in_reminder() {
    // Info 消息应用 <system-reminder> 包裹
    let ctx = make_stage_context();
    let msgs = vec![QueuedMessage::info(
        MessageSource::SystemInjected,
        BaseMessage::human(MessageContent::text("system info")),
    )];
    {
        let mut transcript = ctx.session.transcript.write();
        append_messages_to_transcript(&mut transcript, msgs);
    }
    let transcript = ctx.session.transcript.read();
    assert_eq!(transcript.len(), 1);
    let content = transcript.entries()[0].message.content();
    assert!(content.contains("<system-reminder>"));
    assert!(content.contains("system info"));
}

#[test]
fn test_append_messages_defer_wrapped_in_reminder() {
    // Defer 消息（bg_results / WorkflowComplete）应用 <system-reminder> 包裹
    // —— 这是本次修复的关键断言：mod.rs:520-528 把 awakened_messages 写入
    // transcript 时，Defer 走 reminder 包裹路径（与 Info 一致）。
    let ctx = make_stage_context();
    let msgs = vec![QueuedMessage::defer(
        MessageSource::SubAgentComplete,
        BaseMessage::human(MessageContent::text("bg-result-payload")),
    )];
    {
        let mut transcript = ctx.session.transcript.write();
        append_messages_to_transcript(&mut transcript, msgs);
    }
    let transcript = ctx.session.transcript.read();
    assert_eq!(transcript.len(), 1);
    let content = transcript.entries()[0].message.content();
    assert!(
        content.contains("<system-reminder>"),
        "Defer 应被 reminder 包裹, got: {}",
        content
    );
    assert!(
        content.contains("bg-result-payload"),
        "Defer 内容应在 transcript 中, got: {}",
        content
    );
}

#[tokio::test]
async fn test_e2e_defer_consumed_in_receive() {
    // RCRA：push Defer → run_react_loop → 第一轮 Receive 消费 Defer（drain_all）
    // → Compact → Reason → Act → Receive（空→退出）→ Completed。
    //
    // 迁移自原 test_e2e_defer_written_to_transcript_when_end_awakens，
    // 验证 Defer 在 RCRA 的 Receive 阶段被正确消费和写入 transcript。
    let cwd: Arc<str> = Arc::from("/tmp/rcra-defer");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(Arc::new(FinalAnswerLLM { answer: "ok" }))
        .build();

    ctx.session.queue.push(QueuedMessage::defer(
        MessageSource::SubAgentComplete,
        BaseMessage::human(MessageContent::text("bg-result-payload")),
    ));

    let result = run_react_loop(ctx.clone(), 5).await;
    assert!(
        matches!(result, LoopResult::Completed),
        "expected Completed, got {:?}",
        result
    );

    // transcript 应包含 Defer 内容（reminder 包裹，由 Receive 阶段写入）
    let transcript = ctx.session.transcript.read();
    let combined: String = transcript
        .visible_messages()
        .iter()
        .map(|m| m.content().to_string())
        .collect::<Vec<_>>()
        .join("\n---\n");
    assert!(
        combined.contains("<system-reminder>"),
        "Defer 应被 reminder 包裹写入 transcript, got: {}",
        combined
    );
    assert!(
        combined.contains("bg-result-payload"),
        "Defer 内容应在 transcript 中, got: {}",
        combined
    );
}
