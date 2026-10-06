//! 循环完成、取消及 idle 用户消息交接。
use super::super::*;
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
    StageContext::new_best_effort_fixture(turn, session.transcript(), session.queue().clone())
}

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

#[tokio::test]
async fn test_e2e_final_answer_no_tools() {
    // e2e：推入 Prompt → run_react_loop → 直接 final_answer → Completed
    let cwd: Arc<str> = Arc::from("/tmp/e2e");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    let ctx = StageContext::best_effort_fixture_builder(
        turn,
        session.transcript(),
        session.queue().clone(),
    )
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

/// [回归测试] loading 期间排队的两个 prompt 在后台任务仍活跃时逐条驱动真实 loop。
#[tokio::test]
async fn test_run_react_loop_idle_dispatches_queued_prompts_one_at_a_time() {
    use crate::session::user_input_mailbox::UserInputMailbox;
    use peri_acp_types::session::{EnqueueUserInputRequest, SessionInbox, UserInputState};
    struct PausedFirstAnswerLLM {
        entered: parking_lot::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        resume: Arc<tokio::sync::Notify>,
        seen: Arc<parking_lot::Mutex<Vec<Vec<String>>>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for PausedFirstAnswerLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn crate::tools::BaseTool],
            _streaming: Option<crate::agent::react::StreamingContext>,
        ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
            self.seen.lock().push(
                messages
                    .iter()
                    .filter(|message| matches!(message, BaseMessage::Human { .. }))
                    .map(|message| message.content().to_string())
                    .collect(),
            );
            let entered = self.entered.lock().take();
            if let Some(entered) = entered {
                entered.send(()).unwrap();
                self.resume.notified().await;
            }
            Ok(crate::agent::react::Reasoning::with_answer("", "完成"))
        }
    }
    let mut context = make_stage_context();
    let inbox = Arc::new(SessionInbox::new(Arc::new(context.session.queue.clone())));
    let mailbox = UserInputMailbox::new("session".into(), inbox.clone(), Arc::new(|_| {}));
    mailbox
        .attach_external_attempt(context.session.turn.cancel_token.as_ref().clone(), false)
        .unwrap();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let resume = Arc::new(tokio::sync::Notify::new());
    let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
    context.runtime.llm = Arc::new(PausedFirstAnswerLLM {
        entered: parking_lot::Mutex::new(Some(entered_tx)),
        resume: resume.clone(),
        seen: seen.clone(),
    });
    context.session.user_input_mailbox = Some(mailbox.clone());
    context.async_ctx.idle_wait_enabled = true;
    context.async_ctx.idle_should_wait = Some({
        let seen = seen.clone();
        Arc::new(move || seen.lock().len() < 3)
    });
    let (bus, mut handles) = crate::agent::events_v2::EventBus::new(Default::default());
    context.runtime.event_bus = Arc::new(bus);
    context.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("初始任务"),
    ));
    let task = tokio::spawn(run_react_loop(context, 3));
    tokio::time::timeout(std::time::Duration::from_secs(1), entered_rx)
        .await
        .expect("初始模型调用必须开始")
        .unwrap();
    let mut input_ids = Vec::new();
    for text in ["A", "B"] {
        let input_id = uuid::Uuid::now_v7().to_string();
        let receipt = mailbox
            .enqueue(&EnqueueUserInputRequest {
                session_id: "session".into(),
                generation: mailbox.generation().into(),
                command_id: format!("enqueue-{text}"),
                input_id: input_id.clone(),
                content: MessageContent::text(text),
                original_draft: text.into(),
            })
            .unwrap();
        assert_eq!(receipt.results[0].state, UserInputState::Queued);
        input_ids.push(input_id);
    }
    assert!(inbox.queue().is_empty(), "loading 期间不能交接普通待办");
    resume.notify_one();
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .expect("已有队列必须在 idle 自动继续，无需再次提交或等待后台结果")
        .unwrap();
    assert!(matches!(result, LoopResult::Completed));
    assert_eq!(
        *seen.lock(),
        vec![
            vec!["初始任务"],
            vec!["初始任务", "A"],
            vec!["初始任务", "A", "B"]
        ],
        "每次进入 idle 只交接 FIFO 队首"
    );
    assert!(mailbox.snapshot().items.is_empty());
    let mut delivered = Vec::new();
    while let Ok(event) = handles.render_rx.try_recv() {
        if let crate::agent::events_v2::RenderEvent::UserInputDelivered { input_id, .. } = event {
            delivered.push(input_id);
        }
    }
    assert_eq!(
        delivered, input_ids,
        "聊天投递事件按稳定输入 ID 顺序发射且无重复"
    );
    while let Ok(event) = handles.state_rx.try_recv() {
        assert!(
            !matches!(
                event,
                crate::agent::events_v2::StateEvent::TurnSuspended { .. }
            ),
            "已有可执行 prompt 时不发布虚假的挂起状态"
        );
    }
}

#[tokio::test]
async fn test_e2e_cancel_before_loop() {
    // e2e：cancel_token 在 run_react_loop 之前触发 → Interrupted
    let cwd: Arc<str> = Arc::from("/tmp/e2e-cancel");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    let ctx = StageContext::best_effort_fixture_builder(
        turn,
        session.transcript(),
        session.queue().clone(),
    )
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
    let ctx = StageContext::best_effort_fixture_builder(
        turn,
        session.transcript(),
        session.queue().clone(),
    )
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
    let ctx = StageContext::best_effort_fixture_builder(
        turn,
        session.transcript(),
        session.queue().clone(),
    )
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
