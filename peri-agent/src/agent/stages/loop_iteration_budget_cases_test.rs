// Responsibility-specific cases share the original test namespace and fixtures.

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
    context.session.queue.push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("final prompt"),
    ));

    let result = crate::agent::stages::run_react_loop(context.clone(), 1).await;

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
    context.session.queue.push_sync(QueuedMessage::info(
        MessageSource::SystemInjected,
        BaseMessage::human("micro compact state update"),
    ));

    let result = crate::agent::stages::run_react_loop(context.clone(), 1).await;

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
    context.session.queue.push_sync(QueuedMessage::defer(
        MessageSource::SubAgentComplete,
        BaseMessage::human("real deferred result"),
    ));

    let result = crate::agent::stages::run_react_loop(context.clone(), 1).await;

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

    let result = crate::agent::stages::run_react_loop(context.clone(), 0).await;

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
    context.session.queue.push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("zero budget prompt"),
    ));

    let result = crate::agent::stages::run_react_loop(context.clone(), 0).await;

    assert!(matches!(
        result,
        LoopResult::Error(crate::error::AgentError::MaxIterationsExceeded(0))
    ));
    assert_eq!(llm_calls.load(Ordering::SeqCst), 0);
    assert_eq!(tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(before_agent_calls.load(Ordering::SeqCst), 0);
    assert!(prompt_visible
        .lock()
        .expect("prompt visible lock mutex must remain unpoisoned throughout the test")
        .is_empty());
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
    context.session.queue.push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("use one tool"),
    ));

    let result = crate::agent::stages::run_react_loop(context.clone(), 1).await;

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
        .with_idle_waiting()
        .with_idle_should_wait({
            let should_wait = Arc::clone(&should_wait);
            Arc::new(move || should_wait.load(Ordering::Acquire))
        })
        .with_idle_suspended_flag(Arc::clone(&suspended))
        .build();
    let loop_context = context.clone();
    let loop_task =
        tokio::spawn(async move { crate::agent::stages::run_react_loop(loop_context, 1).await });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !suspended.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("循环必须进入可观测的 idle suspended 状态");
    assert_eq!(before_agent_calls.load(Ordering::SeqCst), 0);
    should_wait.store(false, Ordering::Release);
    handle.push_prompt_sync(
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
    assert_eq!(
        *prompt_visible
            .lock()
            .expect("prompt visible lock mutex must remain unpoisoned throughout the test"),
        vec![true]
    );
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
