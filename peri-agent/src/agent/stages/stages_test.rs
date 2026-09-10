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
    let output = ReceiveOutput {
        consumed_count: 0,
        wake_up_count: 0,
    };
    assert_eq!(output.consumed_count, 0);
    assert_eq!(output.wake_up_count, 0);
}

#[test]
fn test_reason_input_output_contract() {
    let ctx = make_stage_context();
    let _input = ReasonInput {
        context: ctx.clone(),
        has_tool_calls: false,
    };
    let reasoning = crate::agent::react::Reasoning::with_answer("thinking", "answer");
    let output = ReasonOutput {
        reasoning,
        catalog: ctx.runtime.tool_catalog.snapshot(),
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
        context: ctx.clone(),
        reasoning,
        catalog: ctx.runtime.tool_catalog.snapshot(),
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

struct InterruptibleReasonLLM {
    entered: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

#[async_trait::async_trait]
impl ReactLLM for InterruptibleReasonLLM {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        if let Some(entered) = self.entered.lock().unwrap().take() {
            let _ = entered.send(());
        }
        std::future::pending().await
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

#[tokio::test]
async fn test_run_react_loop_info_only_does_not_wake_model() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let session = Session::new(
        Arc::from("/tmp/micro-compact-info-only"),
        FrozenContext::builder().build(),
        None,
    );
    let turn = session.start_turn();
    let context = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(Arc::new(CountingFinalAnswerLLM {
            calls: Arc::clone(&calls),
            answer: "must not run",
        }))
        .build();
    context.session.queue.push(QueuedMessage::info(
        MessageSource::SystemInjected,
        BaseMessage::human("micro compact state update"),
    ));

    let result = run_react_loop(context.clone(), 1).await;

    assert!(matches!(result, LoopResult::Completed));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(context.session.turn.current_step(), 0);
    assert!(context.session.transcript.read().entries()[0]
        .message()
        .content()
        .contains("micro compact state update"));
}

#[tokio::test]
async fn test_run_react_loop_defer_still_continues_to_model() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let session = Session::new(
        Arc::from("/tmp/defer-continuation"),
        FrozenContext::builder().build(),
        None,
    );
    let turn = session.start_turn();
    let context = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(Arc::new(CountingFinalAnswerLLM {
            calls: Arc::clone(&calls),
            answer: "continued",
        }))
        .build();
    context.session.queue.push(QueuedMessage::defer(
        MessageSource::SubAgentComplete,
        BaseMessage::human("real deferred result"),
    ));

    let result = run_react_loop(context.clone(), 1).await;

    assert!(matches!(result, LoopResult::Completed));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(context.session.turn.current_step(), 1);
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

/// [回归测试] Reason 内取消必须保留成对的 stage lifecycle，
/// 同时将 loop 终态规范化为 Interrupted，不得降级成 Error(Interrupted)。
#[tokio::test]
async fn test_run_react_loop_cancel_during_reason_is_interrupted() {
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let session = Session::new(
        Arc::from("/tmp/e2e-cancel-during-reason"),
        FrozenContext::builder().build(),
        None,
    );
    let turn = session.start_turn();
    let (bus, mut handles) = crate::agent::events_v2::EventBus::new(Default::default());
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(Arc::new(InterruptibleReasonLLM {
            entered: std::sync::Mutex::new(Some(entered_tx)),
        }))
        .with_event_bus(Arc::new(bus))
        .build();
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("cancel while reasoning"),
    ));
    let loop_ctx = ctx.clone();
    let task = tokio::spawn(async move { run_react_loop(loop_ctx, 10).await });
    entered_rx.await.expect("Reason LLM 必须进入调用");

    ctx.session.turn.cancel_token.cancel();
    let result = task.await.expect("loop task 不得 panic");

    assert!(
        matches!(result, LoopResult::Interrupted),
        "Reason 内取消必须返回 Interrupted，got: {result:?}"
    );
    let lifecycle: Vec<_> = std::iter::from_fn(|| handles.try_observe())
        .filter_map(|event| match event {
            ObserveEvent::StageStarted { stage, .. } => Some((stage, None)),
            ObserveEvent::StageEnded { stage, status, .. } => Some((stage, Some(status))),
            _ => None,
        })
        .collect();
    assert_eq!(
        lifecycle,
        vec![
            (Stage::Receive, None),
            (Stage::Receive, Some(StageStatus::Done)),
            (Stage::Compact, None),
            (Stage::Compact, Some(StageStatus::Done)),
            (Stage::Reason, None),
            (Stage::Reason, Some(StageStatus::Error)),
        ],
        "Reason 取消仍必须发射成对 StageEnded(Error)，且不得进入 Act"
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

struct UsageChurnReactLLM {
    usages: Vec<u32>,
    calls: Arc<std::sync::atomic::AtomicUsize>,
    requests: Arc<std::sync::Mutex<Vec<usize>>>,
}

#[async_trait::async_trait]
impl ReactLLM for UsageChurnReactLLM {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(messages.len());
        let usage = self.usages[call];
        let mut reasoning = if call + 1 == self.usages.len() {
            crate::agent::react::Reasoning::with_answer("finish", "done")
        } else {
            crate::agent::react::Reasoning::with_tools(
                format!("generation {call}"),
                vec![crate::agent::react::ToolCall::new(
                    format!("usage-churn-{call}"),
                    "usage_churn_tool",
                    serde_json::json!({ "generation": call }),
                )],
            )
        };
        reasoning.usage = Some(peri_model::TokenUsage {
            input_tokens: usage,
            output_tokens: 100,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
        reasoning.request_id = Some(format!("usage-generation-{call}"));
        reasoning.model = "scripted-usage-churn".to_string();
        Ok(reasoning)
    }
}

struct UsageChurnTool;

#[async_trait::async_trait]
impl crate::tools::BaseTool for UsageChurnTool {
    fn name(&self) -> &str {
        "usage_churn_tool"
    }

    fn description(&self) -> &str {
        "keeps the characterization loop running"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "generation": { "type": "integer" } }
        })
    }

    async fn invoke(
        &self,
        input: serde_json::Value,
        _ctx: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(format!(
            "generation {} tool output {}",
            input["generation"],
            "x".repeat(256)
        ))
    }
}

struct CountingCompactModel {
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl peri_model::Model for CountingCompactModel {
    fn capabilities(&self) -> peri_model::ModelCapabilities {
        peri_model::ModelCapabilities {
            supports_tools: false,
            supports_reasoning: false,
            supports_vision: false,
            supports_streaming: true,
        }
    }

    async fn stream(
        &self,
        _request: peri_model::ModelRequest,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> peri_model::ModelResult<peri_model::ModelStream> {
        unreachable!("compact characterization uses complete")
    }

    async fn complete(
        &self,
        _request: peri_model::ModelRequest,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> peri_model::ModelResult<peri_model::ModelResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        peri_model::ModelResponse::new(
            peri_model::ModelMessage::assistant_text(format!(
                "<summary>compact generation {call}</summary>"
            )),
            peri_model::StopReason::EndTurn,
            None,
            None,
        )
    }
}

struct SuccessfulFullReactLLM {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    requests: Arc<std::sync::Mutex<Vec<Vec<String>>>>,
    file_path: String,
}

#[async_trait::async_trait]
impl ReactLLM for SuccessfulFullReactLLM {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(
            messages
                .iter()
                .map(|message| message.content().to_string())
                .collect(),
        );
        let mut reasoning = if call == 0 {
            crate::agent::react::Reasoning::with_tools(
                "original reasoning marker",
                vec![crate::agent::react::ToolCall::new(
                    "successful-full-read",
                    "Read",
                    serde_json::json!({ "file_path": self.file_path }),
                )],
            )
        } else {
            crate::agent::react::Reasoning::with_answer("post-full reasoning", "done")
        };
        reasoning.usage = Some(peri_model::TokenUsage {
            input_tokens: if call == 0 { 96_000 } else { 1_000 },
            output_tokens: 100,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
        reasoning.request_id = Some(format!("scripted-successful-full-{call}"));
        reasoning.model = "scripted-successful-full".to_string();
        Ok(reasoning)
    }
}

struct SuccessfulFullReadTool;

#[async_trait::async_trait]
impl crate::tools::BaseTool for SuccessfulFullReadTool {
    fn name(&self) -> &str {
        "Read"
    }

    fn description(&self) -> &str {
        "reads the characterization fixture"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "file_path": { "type": "string" } },
            "required": ["file_path"]
        })
    }

    async fn invoke(
        &self,
        input: serde_json::Value,
        _ctx: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(std::fs::read_to_string(
            input["file_path"].as_str().unwrap(),
        )?)
    }
}

/// Characterization：scripted provider usage 驱动一次真实持久化 Full lifecycle，随后 tracker
/// 接受 Full 后 Reason 返回的新低 usage 样本。
#[tokio::test]
async fn test_run_react_loop_successful_full_replaces_history_reinjects_read_file_and_resets_usage()
{
    use crate::thread::{SqliteThreadStore, ThreadMeta, ThreadStore};

    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("full-reinject-marker.txt");
    let file_marker = "successful full reinjected file marker";
    std::fs::write(&file_path, file_marker).unwrap();

    let store = Arc::new(
        SqliteThreadStore::new(dir.path().join("successful-full.db"))
            .await
            .unwrap(),
    );
    let thread_id = store
        .create_thread(ThreadMeta::new(dir.path().to_string_lossy()))
        .await
        .unwrap();
    let store_dyn: Arc<dyn ThreadStore> = store.clone();
    let session = Session::new(
        Arc::from(dir.path().to_string_lossy().as_ref()),
        FrozenContext::builder().build(),
        Some(thread_id.clone()),
    );
    {
        let transcript = session.transcript();
        let mut transcript = transcript.write();
        *transcript =
            std::mem::take(&mut *transcript).with_persistence(store_dyn.clone(), thread_id.clone());
    }

    let reason_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let reason_requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let compact_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (bus, mut handles) = crate::agent::events_v2::EventBus::new(Default::default());
    let tools: SharedToolMap = Arc::new(parking_lot::RwLock::new(BTreeMap::from([(
        "Read".to_string(),
        Arc::new(SuccessfulFullReadTool) as Arc<dyn crate::tools::BaseTool>,
    )])));
    let config = CompactConfig {
        micro_compact_stale_steps: 0,
        target_headroom_tokens: 50_000,
        micro_field_threshold_chars: 32,
        micro_field_keep_head_chars: 8,
        micro_field_keep_tail_chars: 8,
        ..Default::default()
    };
    let mut budget = crate::agent::token::ContextBudget::new(100_000);
    budget.output_reserve = 40_000;
    let turn = session.start_turn();
    let context = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(Arc::new(SuccessfulFullReactLLM {
            calls: Arc::clone(&reason_calls),
            requests: Arc::clone(&reason_requests),
            file_path: file_path.to_string_lossy().into_owned(),
        }))
        .with_tools(tools)
        .with_event_bus(Arc::new(bus))
        .with_context_budget(budget)
        .with_compact_config(config)
        .with_compact_llm(Arc::new(CountingCompactModel {
            calls: Arc::clone(&compact_calls),
        }))
        .build();
    let prompt_marker = "successful full original prompt marker";
    context.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human(prompt_marker),
    ));

    assert!(matches!(
        run_react_loop(context.clone(), 2).await,
        LoopResult::Completed
    ));
    assert_eq!(reason_calls.load(Ordering::SeqCst), 2);
    assert_eq!(compact_calls.load(Ordering::SeqCst), 1);

    {
        let requests = reason_requests.lock().unwrap();
        assert!(requests[0]
            .iter()
            .any(|content| content.contains(prompt_marker)));
        let post_full = &requests[1];
        assert!(post_full
            .iter()
            .any(|content| content.contains("compact generation 0")));
        assert!(post_full
            .iter()
            .any(|content| content.contains(file_marker)));
        assert!(!post_full
            .iter()
            .any(|content| content.contains(prompt_marker)));
        assert!(!post_full
            .iter()
            .any(|content| content.contains("original reasoning marker")));
    }

    let outcomes: Vec<_> = std::iter::from_fn(|| handles.try_observe())
        .filter_map(|event| match event {
            ObserveEvent::MessagesCompacted { outcome, .. } => Some(outcome),
            _ => None,
        })
        .collect();
    assert_eq!(
        outcomes,
        vec![crate::agent::compact_v2::CompactOutcome::FullApplied]
    );
    assert_eq!(
        context
            .compact
            .token_tracker
            .read()
            .estimated_context_tokens(),
        Some(1_000),
        "Full reset 后第二次 Reason 的低 usage 应成为权威样本"
    );

    let persist_tx = context
        .session
        .transcript
        .read()
        .persist_tx_handle()
        .expect("测试 transcript 应绑定持久化 writer");
    crate::session::transcript::MessageTranscript::flush_via_tx(&persist_tx)
        .await
        .unwrap();
    let persisted = store.load_messages(&thread_id).await.unwrap();
    let flags = store.load_message_flags(&thread_id).await.unwrap();
    let summary_count = persisted
        .iter()
        .filter(|message| message.content().contains("compact generation 0"))
        .count();
    let reinject_count = persisted
        .iter()
        .filter(|message| {
            message.content().contains(file_marker)
                && !flags.get(&message.id()).is_some_and(|flag| flag.excluded)
        })
        .count();
    assert_eq!(summary_count, 1);
    assert_eq!(reinject_count, 1);
    assert!(persisted
        .iter()
        .filter(|message| {
            message.content().contains(prompt_marker)
                || message.content().contains("original reasoning marker")
                || message.content() == file_marker
        })
        .all(|message| flags.get(&message.id()).is_some_and(|flag| flag.excluded)));
}

/// Characterization：每次 Reason 都返回新的 provider usage generation 时，已消费样本 guard
/// 会重新 arm；策略由新的高位值决定，而不是由上一次 Compact 的结果决定。
#[tokio::test]
async fn test_run_react_loop_new_high_usage_generations_continue_full_micro_churn() {
    let reason_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let reason_requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let compact_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let session = Session::new(
        Arc::from("/tmp/usage-churn-characterization"),
        FrozenContext::builder().build(),
        None,
    );
    let turn = session.start_turn();
    let (bus, mut handles) = crate::agent::events_v2::EventBus::new(Default::default());
    let tools: SharedToolMap = Arc::new(parking_lot::RwLock::new(BTreeMap::from([(
        "usage_churn_tool".to_string(),
        Arc::new(UsageChurnTool) as Arc<dyn crate::tools::BaseTool>,
    )])));
    let config = CompactConfig {
        micro_compact_stale_steps: 0,
        target_headroom_tokens: 50_000,
        micro_field_threshold_chars: 32,
        micro_field_keep_head_chars: 8,
        micro_field_keep_tail_chars: 8,
        ..Default::default()
    };
    let mut budget = crate::agent::token::ContextBudget::new(100_000);
    budget.output_reserve = 40_000;
    let context = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(Arc::new(UsageChurnReactLLM {
            usages: vec![96_000, 80_000, 97_000, 81_000],
            calls: Arc::clone(&reason_calls),
            requests: Arc::clone(&reason_requests),
        }))
        .with_tools(tools)
        .with_event_bus(Arc::new(bus))
        .with_context_budget(budget)
        .with_compact_config(config)
        .with_compact_llm(Arc::new(CountingCompactModel {
            calls: Arc::clone(&compact_calls),
        }))
        .build();
    context.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("characterize usage churn"),
    ));

    let result = run_react_loop(context, 4).await;

    assert!(matches!(result, LoopResult::Completed));
    assert_eq!(reason_calls.load(Ordering::SeqCst), 4);
    assert_eq!(reason_requests.lock().unwrap().len(), 4);
    let compacted: Vec<_> = std::iter::from_fn(|| handles.try_observe())
        .filter_map(|event| match event {
            ObserveEvent::MessagesCompacted {
                strategy,
                estimated_tokens_before,
                full_escalation_reason,
                outcome,
                ..
            } => Some((
                strategy,
                estimated_tokens_before,
                full_escalation_reason,
                outcome,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        compacted,
        vec![
            (
                crate::agent::events::CompactStrategy::Micro,
                96_000,
                Some(crate::agent::compact_v2::planner::FullEscalationReason::InsufficientReclaim),
                crate::agent::compact_v2::CompactOutcome::MicroAppliedThenFullFailed,
            ),
            (
                crate::agent::events::CompactStrategy::Micro,
                80_000,
                None,
                crate::agent::compact_v2::CompactOutcome::MicroApplied,
            ),
            (
                crate::agent::events::CompactStrategy::Micro,
                97_000,
                Some(crate::agent::compact_v2::planner::FullEscalationReason::InsufficientReclaim),
                crate::agent::compact_v2::CompactOutcome::MicroAppliedThenFullFailed,
            ),
        ],
        "每个新的非零高位 usage generation 都会再次触发；Full 区间会升级尝试，Micro 区间只执行 Micro"
    );
    assert_eq!(
        compact_calls.load(Ordering::SeqCst),
        2,
        "两次 Full 各调用一次 compact LLM；中间 Micro 不调用"
    );
}

struct AuditAlternatingOutputTool;

#[async_trait::async_trait]
impl crate::tools::BaseTool for AuditAlternatingOutputTool {
    fn name(&self) -> &str {
        "usage_churn_tool"
    }

    fn description(&self) -> &str {
        "为审计循环交替提供长短结果"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {"generation": {"type": "integer"}}})
    }

    async fn invoke(
        &self,
        input: serde_json::Value,
        _ctx: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(if input["generation"].as_u64().unwrap().is_multiple_of(2) {
            "x".repeat(8_000)
        } else {
            "ok".to_string()
        })
    }
}

/// 审计 characterization：真实 SQLite Full 成功后仍能 Full→Micro→Full→Micro。
/// usage 数列为受控输入，只证明新高样本下的执行链，不代表现场 token 测量。
#[tokio::test]
async fn test_audit_run_react_loop_successful_full_micro_churn_has_zero_micro_delta() {
    use crate::agent::compact_v2::{planner::plan_micro, projection, CompactOutcome};
    use crate::thread::{SqliteThreadStore, ThreadMeta, ThreadStore};
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn ThreadStore> = Arc::new(
        SqliteThreadStore::new(dir.path().join("audit-full-churn.db"))
            .await
            .unwrap(),
    );
    let thread_id = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();
    let session = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some(thread_id.clone()),
    );
    {
        let transcript = session.transcript();
        let mut transcript = transcript.write();
        *transcript =
            std::mem::take(&mut *transcript).with_persistence(store.clone(), thread_id.clone());
    }
    let reason_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let compact_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (bus, mut handles) = crate::agent::events_v2::EventBus::new(Default::default());
    let tools: SharedToolMap = Arc::new(parking_lot::RwLock::new(BTreeMap::from([(
        "usage_churn_tool".to_string(),
        Arc::new(AuditAlternatingOutputTool) as Arc<dyn crate::tools::BaseTool>,
    )])));
    let config = CompactConfig {
        micro_compact_stale_steps: 0,
        ..Default::default()
    };
    let mut budget = crate::agent::token::ContextBudget::new(100_000);
    budget.output_reserve = 40_000;
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(UsageChurnReactLLM {
        usages: vec![96_000, 80_000, 96_000, 80_000, 1_000],
        calls: reason_calls.clone(),
        requests: Arc::new(std::sync::Mutex::new(Vec::new())),
    }))
    .with_tools(tools)
    .with_event_bus(Arc::new(bus))
    .with_context_budget(budget)
    .with_compact_config(config.clone())
    .with_compact_llm(Arc::new(CountingCompactModel {
        calls: compact_calls.clone(),
    }))
    .build();
    context.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("audit churn"),
    ));
    assert!(matches!(
        run_react_loop(context.clone(), 5).await,
        LoopResult::Completed
    ));
    assert_eq!(reason_calls.load(Ordering::SeqCst), 5);
    assert_eq!(compact_calls.load(Ordering::SeqCst), 2);
    let compacted: Vec<_> = std::iter::from_fn(|| handles.try_observe())
        .filter_map(|event| match event {
            ObserveEvent::MessagesCompacted {
                outcome,
                estimated_tokens_saved,
                affected_count,
                ..
            } => Some((outcome, estimated_tokens_saved, affected_count)),
            _ => None,
        })
        .collect();
    assert_eq!(
        compacted.iter().map(|entry| entry.0).collect::<Vec<_>>(),
        vec![
            CompactOutcome::FullApplied,
            CompactOutcome::MicroApplied,
            CompactOutcome::FullApplied,
            CompactOutcome::MicroApplied,
        ]
    );
    for index in [1, 3] {
        assert!(compacted[index].1 > 0, "Micro 对外报告正收益");
        assert_eq!(
            compacted[index].2, 1,
            "每次只重新标记刚被 Full 排除的长结果"
        );
    }
    {
        let transcript = context.session.transcript.read();
        let plan = plan_micro(&transcript, &config, false);
        assert_eq!(plan.actions.len(), 2);
        assert!(plan
            .actions
            .iter()
            .all(|action| transcript.flags(action.message_id).excluded));
        let canonical = transcript.visible_model_messages().unwrap();
        let projected =
            projection::render_llm_view(&transcript, &plan, &Default::default()).unwrap();
        assert_eq!(
            serde_json::to_value(canonical).unwrap(),
            serde_json::to_value(projected).unwrap(),
            "两个 Micro directive 均指向不可见历史，实际模型消息收益为零"
        );
    }
    let tx = context
        .session
        .transcript
        .read()
        .persist_tx_handle()
        .unwrap();
    crate::session::transcript::MessageTranscript::flush_via_tx(&tx)
        .await
        .unwrap();
    let flags = store.load_message_flags(&thread_id).await.unwrap();
    assert_eq!(
        flags
            .values()
            .filter(|flag| flag.excluded && flag.projection.is_some())
            .count(),
        2
    );
}

/// [回归测试] 工具结果已进入 transcript 后，下一轮 Compact 必须看见其新增压力。
#[tokio::test]
#[ignore = "已确认 compact 缺陷：生产 tool dispatch 未接线 token growth；修复后移除此标记"]
async fn test_audit_dispatch_must_account_for_tool_output_pressure() {
    let context = make_stage_context();
    context.runtime.tools.write().insert(
        "usage_churn_tool".into(),
        Arc::new(AuditAlternatingOutputTool),
    );
    let catalog = context
        .runtime
        .tool_catalog
        .pin_working_tools(&context.runtime.tools.read())
        .unwrap();
    context
        .compact
        .token_tracker
        .write()
        .accumulate(&peri_model::TokenUsage {
            input_tokens: 74_000,
            output_tokens: 100,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
    let reasoning = crate::agent::react::Reasoning::with_tools(
        "inspect",
        vec![crate::agent::react::ToolCall::new(
            "audit-output",
            "usage_churn_tool",
            serde_json::json!({"generation": 0}),
        )],
    );
    super::tool_dispatch::dispatch_tools(
        &context,
        &reasoning,
        &catalog,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(
        context
            .session
            .transcript
            .read()
            .visible_messages()
            .iter()
            .any(|message| {
                matches!(message, BaseMessage::Tool { .. }) && message.content().len() == 8_000
            }),
        "必须实际执行工具并写入其完整结果"
    );
    assert_eq!(
        context
            .compact
            .token_tracker
            .read()
            .estimated_context_tokens(),
        Some(76_000),
        "74k provider input 加上已提交的 8k 字符工具结果，按 tracker 的 chars/4 应为 76k"
    );
}

/// [回归测试] Reason 已投影的内容不能在随后 Micro 中再次报告增量回收收益。
#[tokio::test]
#[ignore = "已确认 compact 缺陷：Reason 预投影导致 Micro 重复记收益；修复后移除此标记"]
async fn test_audit_micro_savings_must_change_previous_reason_view() {
    let session = Session::new(Arc::from("/tmp"), FrozenContext::builder().build(), None);
    {
        let transcript = session.transcript();
        let mut transcript = transcript.write();
        for turn in 0..4 {
            let call_id = format!("audit-reason-{turn}");
            transcript.append(BaseMessage::human("inspect"));
            transcript.append(BaseMessage::ai_with_tool_calls(
                "inspect",
                vec![crate::messages::ToolCallRequest::new(
                    &call_id,
                    "Bash",
                    serde_json::json!({"command": "fixture"}),
                )],
            ));
            transcript.append(BaseMessage::tool_result(&call_id, "x".repeat(8_000)));
        }
    }
    let (bus, mut handles) = crate::agent::events_v2::EventBus::new(Default::default());
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(UsageChurnReactLLM {
        usages: vec![80_000, 80_000],
        calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        requests: Arc::new(std::sync::Mutex::new(Vec::new())),
    }))
    .with_event_bus(Arc::new(bus))
    .with_context_budget(crate::agent::token::ContextBudget::new(100_000))
    .with_compact_config(CompactConfig::default())
    .build();
    let first = super::reason::run_reason(ReasonInput {
        context: context.clone(),
        has_tool_calls: false,
    })
    .await
    .unwrap();
    super::compact::run_compact(CompactInput {
        context: context.clone(),
        has_tool_calls: false,
    })
    .await
    .unwrap();
    let saved = std::iter::from_fn(|| handles.try_observe())
        .find_map(|event| match event {
            ObserveEvent::MessagesCompacted {
                estimated_tokens_saved,
                ..
            } => Some(estimated_tokens_saved),
            _ => None,
        })
        .unwrap_or(0);
    let second = super::reason::run_reason(ReasonInput {
        context,
        has_tool_calls: false,
    })
    .await
    .unwrap();
    assert!(
        saved == 0
            || serde_json::to_value(&*first.messages_snapshot).unwrap()
                != serde_json::to_value(&*second.messages_snapshot).unwrap(),
        "Micro 报告节省 {saved} tokens，但两次真实 Reason 消息快照完全相同"
    );
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
    let content = transcript.entries()[0].message().content();
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
fn test_append_messages_info_kept_as_plain_message() {
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
    assert_eq!(transcript.entries()[0].message().content(), "system info");
}

#[test]
fn test_append_messages_defer_kept_as_plain_message() {
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
    assert_eq!(
        transcript.entries()[0].message().content(),
        "bg-result-payload"
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

    // transcript 应包含普通 Defer 内容，保持 legacy event/message 语义。
    let transcript = ctx.session.transcript.read();
    let combined: String = transcript
        .visible_messages()
        .iter()
        .map(|m| m.content().to_string())
        .collect::<Vec<_>>()
        .join("\n---\n");
    assert!(
        combined.contains("bg-result-payload"),
        "Defer 内容应在 transcript 中, got: {}",
        combined
    );
}
