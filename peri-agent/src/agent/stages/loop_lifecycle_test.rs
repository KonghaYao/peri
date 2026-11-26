//! 循环完成、取消及 idle 用户消息交接。
use crate::agent::events::Stage;
use crate::agent::events::StageStatus;

use crate::agent::events_v2::ObserveEvent;
use crate::agent::react::ReactLLM;

use crate::agent::stages::context::StageContext;
use crate::agent::stages::LoopResult;
use crate::messages::BaseMessage;
use crate::messages::MessageContent;
use crate::session::queue::MessageSource;
use crate::session::store::FrozenContext;
use crate::session::QueuedMessage;
use crate::session::Session;

use std::sync::Arc;

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
        if let Some(entered) = self
            .entered
            .lock()
            .expect("self entered mutex must remain unpoisoned throughout the test")
            .take()
        {
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
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(Arc::new(FinalAnswerLLM {
            answer: "task completed",
        }))
        .build();

    // 推入用户输入
    ctx.session.queue.push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human(MessageContent::text("do the task")),
    ));

    let result = crate::agent::stages::run_react_loop(ctx.clone(), 10).await;
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
                entered
                    .send(())
                    .expect("entered send test receiver must remain available for this delivery");
                self.resume.notified().await;
            }
            Ok(crate::agent::react::Reasoning::with_answer("", "完成"))
        }
    }
    let mut context = make_stage_context();
    let inbox = Arc::new(SessionInbox::new(Arc::new(context.session.queue.clone())));
    let mailbox = UserInputMailbox::new("session".into(), inbox.clone(), Arc::new(|_| {}));
    mailbox
        .attach_external_attempt_sync(context.session.turn.cancel_token.as_ref().clone(), false)
        .expect("the attach external attempt sync fixture result is required for this assertion");
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
    context.session.queue.push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("初始任务"),
    ));
    let task = tokio::spawn(crate::agent::stages::run_react_loop(context, 3));
    tokio::time::timeout(std::time::Duration::from_secs(1), entered_rx)
        .await
        .expect("初始模型调用必须开始")
        .expect("the expect fixture result is required for this assertion");
    let mut input_ids = Vec::new();
    for text in ["A", "B"] {
        let input_id = uuid::Uuid::now_v7().to_string();
        let receipt = mailbox
            .enqueue_sync(&EnqueueUserInputRequest {
                session_id: "session".into(),
                generation: mailbox.generation().into(),
                command_id: format!("enqueue-{text}"),
                input_id: input_id.clone(),
                content: MessageContent::text(text),
                original_draft: text.into(),
                plugin_selections: Vec::new(),
            })
            .expect("the enqueue sync fixture result is required for this assertion");
        assert_eq!(receipt.results[0].state, UserInputState::Queued);
        input_ids.push(input_id);
    }
    assert!(
        inbox.queue_for_test().is_empty_sync(),
        "loading 期间不能交接普通待办"
    );
    resume.notify_one();
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .expect("已有队列必须在 idle 自动继续，无需再次提交或等待后台结果")
        .expect("the expect fixture result is required for this assertion");
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
    assert!(mailbox.snapshot_sync().items.is_empty());
    let mut delivered = Vec::new();
    while let Ok(event) = handles.render_rx.try_recv() {
        assert!(
            !matches!(
                event,
                crate::agent::events_v2::RenderEvent::TurnSuspended { .. }
            ),
            "已有可执行 prompt 时不发布虚假的挂起状态"
        );
        if let crate::agent::events_v2::RenderEvent::UserInputDelivered { input_id, .. } = event {
            delivered.push(input_id);
        }
    }
    assert_eq!(
        delivered, input_ids,
        "聊天投递事件按稳定输入 ID 顺序发射且无重复"
    );
}

/// [回归测试] Act 工具批次中放行的立即发送不取消执行；下一次 Receive 在
/// 同一 attempt 内摄取，工具结果在前、steer 输入按 MQ 顺序在后。
#[tokio::test]
async fn test_run_react_loop_steer_during_tool_batch_is_received_next_iteration() {
    assert_tool_boundary_steer(0, false, SteerMode::Queued).await;
}

#[tokio::test]
async fn test_tool_boundary_steer_behind_two_pages_reaches_next_model_request() {
    assert_tool_boundary_steer(130, false, SteerMode::Queued).await;
}

#[tokio::test]
async fn test_tool_boundary_steer_during_request_preparation_reaches_next_model_request() {
    assert_tool_boundary_steer(0, true, SteerMode::Queued).await;
}

#[tokio::test]
async fn test_boundary_queued_steer_during_tool_batch_reaches_next_model_request() {
    assert_tool_boundary_steer(130, false, SteerMode::Boundary).await;
}

#[tokio::test]
async fn test_boundary_queued_steer_during_request_preparation_reaches_next_model_request() {
    assert_tool_boundary_steer(0, true, SteerMode::Boundary).await;
}

/// Enter 的发送选择在工具批次中只放行 MQ：不取消整批工具，下一次 Receive 摄取。
#[tokio::test]
async fn test_enter_dispatch_during_tool_batch_does_not_cancel_the_batch() {
    assert_tool_boundary_steer(0, false, SteerMode::Enter).await;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SteerMode {
    /// 普通入队后显式发送。
    Queued,
    /// 仅按 toolBoundary 入队，等待工具边界自动抽取。
    Boundary,
    /// Enter：按 toolBoundary 入队并立即发送。
    Enter,
}

async fn assert_tool_boundary_steer(
    backlog: usize,
    dispatch_in_preparation: bool,
    mode: SteerMode,
) {
    use crate::agent::react::{Reasoning, ToolCall};
    use crate::middleware::{capabilities::BeforeModelState, Middleware, MiddlewareChain};
    use crate::session::tool_catalog::SessionToolCatalog;
    use crate::session::user_input_mailbox::UserInputMailbox;
    use crate::tools::BaseTool;
    use peri_acp_types::session::{
        DispatchUserInputsRequest, EnqueueUserInputRequest, SessionInbox, UserInputState,
    };
    use std::collections::BTreeMap;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    struct PauseSecondRequest {
        hooks: AtomicUsize,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl Middleware for PauseSecondRequest {
        fn name(&self) -> &str {
            "pause-second-request"
        }
        async fn before_model(
            &self,
            _: &mut dyn BeforeModelState,
        ) -> crate::error::AgentResult<()> {
            if self.hooks.fetch_add(1, Ordering::SeqCst) == 1 {
                self.entered.notify_one();
                self.release.notified().await;
            }
            Ok(())
        }
    }

    struct ToolBatchLlm {
        calls: Arc<std::sync::atomic::AtomicUsize>,
        requests: Arc<parking_lot::Mutex<Vec<Vec<BaseMessage>>>>,
    }

    #[async_trait::async_trait]
    impl ReactLLM for ToolBatchLlm {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<crate::agent::react::StreamingContext>,
        ) -> crate::error::AgentResult<Reasoning> {
            self.requests.lock().push(messages.to_vec());
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(Reasoning::with_tools(
                    "need the blocking tool",
                    vec![ToolCall::new(
                        "call-block",
                        "Blocking",
                        serde_json::json!({}),
                    )],
                ));
            }
            Ok(Reasoning::with_answer("", "done"))
        }
        fn model_name(&self) -> String {
            "mock-tool-batch".to_string()
        }
    }

    struct BlockingTool {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl BaseTool for BlockingTool {
        fn name(&self) -> &str {
            "Blocking"
        }
        fn description(&self) -> &str {
            "blocks the Act batch"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn is_direct(&self) -> bool {
            true
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            self.entered.notify_one();
            self.release.notified().await;
            Ok("tool finished".to_string())
        }
    }

    let session = Session::new(
        Arc::from("/tmp/steer-during-tool-batch"),
        FrozenContext::builder().build(),
        None,
    );
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut tools: BTreeMap<String, Arc<dyn BaseTool>> = BTreeMap::new();
    tools.insert(
        "Blocking".to_string(),
        Arc::new(BlockingTool {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        }),
    );
    let shared_tools = Arc::new(parking_lot::RwLock::new(tools.clone()));
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let requests = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut chain = MiddlewareChain::new();
    let preparing = Arc::new(tokio::sync::Notify::new());
    let resume_preparation = Arc::new(tokio::sync::Notify::new());
    if dispatch_in_preparation {
        chain.add(Box::new(PauseSecondRequest {
            hooks: AtomicUsize::new(0),
            entered: preparing.clone(),
            release: resume_preparation.clone(),
        }));
    }
    let mut context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(ToolBatchLlm {
        calls: Arc::clone(&calls),
        requests: Arc::clone(&requests),
    }))
    .with_middleware_chain(Arc::new(chain))
    .with_tools(shared_tools)
    .with_tool_catalog(Arc::new(SessionToolCatalog::new(tools, None)))
    .build();
    let inbox = Arc::new(SessionInbox::new(Arc::new(context.session.queue.clone())));
    let mailbox = UserInputMailbox::new("session".into(), inbox.clone(), Arc::new(|_| {}));
    let cancel = context.session.turn.cancel_token.as_ref().clone();
    let ticket = mailbox
        .attach_external_attempt_sync(cancel.clone(), false)
        .expect("the attach external attempt sync fixture result is required for this assertion");
    context.session.user_input_mailbox = Some(mailbox.clone());
    context.session.queue.push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("初始任务"),
    ));

    let task = tokio::spawn(crate::agent::stages::run_react_loop(context, 2));
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .expect("工具批次必须开始执行");

    // 后台进度先于用户显式发送入队，不能耗掉一次模型调用。
    for index in 0..backlog {
        session.queue().push_sync(QueuedMessage::info(
            MessageSource::SystemInjected,
            BaseMessage::human(format!("background progress {index}")),
        ));
    }

    if dispatch_in_preparation {
        release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(2), preparing.notified())
            .await
            .expect("the tool result must have passed Receive before dispatch");
    }
    let mut input_ids = Vec::new();
    for text in ["idle-only", "steer-1", "steer-2"] {
        let input_id = uuid::Uuid::now_v7().to_string();
        let request = EnqueueUserInputRequest {
            session_id: "session".into(),
            generation: mailbox.generation().into(),
            command_id: format!("enqueue-{text}"),
            input_id: input_id.clone(),
            content: MessageContent::text(text),
            original_draft: text.into(),
            plugin_selections: Vec::new(),
        };
        let boundary_submit = mode != SteerMode::Queued && text != "idle-only";
        let receipt = if boundary_submit {
            mailbox.enqueue_at_tool_boundary(&request).await
        } else {
            mailbox.enqueue(&request).await
        }
        .expect("enqueue");
        assert_eq!(receipt.results[0].state, UserInputState::Queued);
        if text != "idle-only" {
            input_ids.push(input_id);
        }
    }
    if mode != SteerMode::Boundary {
        mailbox
            .dispatch(&DispatchUserInputsRequest {
                session_id: "session".into(),
                generation: mailbox.generation().into(),
                command_id: "dispatch-during-tool-batch".into(),
                input_ids: input_ids.clone(),
            })
            .await
            .expect("the dispatch fixture result is required for this assertion");
    }
    assert!(!cancel.is_cancelled(), "工具批次执行中不得取消当前 attempt");

    if dispatch_in_preparation {
        resume_preparation.notify_one();
    } else {
        release.notify_one();
    }
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .expect("工具完成后 loop 必须继续")
        .expect("loop task 不得 panic");
    assert!(
        matches!(result, LoopResult::Completed),
        "软注入必须完成同一 attempt，不得返回 Interrupted: {result:?}"
    );
    assert!(!cancel.is_cancelled(), "整个过程中不应触发取消");
    assert!(
        mailbox.is_active_ticket(&ticket),
        "软注入不得结束当前 attempt"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2, "工具后必须再走一轮 Reason");
    let pending = mailbox.snapshot_sync().items;
    let expected_pending: &[&str] = if mode == SteerMode::Boundary {
        // 边界每次只抽取队首一条；steer-2 解除本次 attempt 的边界绑定，
        // 保持 Queued 等下一次 idle，不得挤进同一次模型请求。
        &["idle-only", "steer-2"]
    } else {
        &["idle-only"]
    };
    let pending_drafts: Vec<&str> = pending
        .iter()
        .map(|item| item.original_draft.as_str())
        .collect();
    assert_eq!(pending_drafts, expected_pending);
    assert!(pending
        .iter()
        .all(|item| item.state == UserInputState::Queued));

    let requests = requests.lock();
    let second = requests.get(1).expect("第二轮 Reason 请求必须存在");
    let expected_steers: &[&str] = if mode == SteerMode::Boundary {
        &["steer-1"]
    } else {
        &["steer-1", "steer-2"]
    };
    for text in expected_steers {
        assert_eq!(
            second
                .iter()
                .filter(|message| message.content() == *text)
                .count(),
            1
        );
    }
    if mode == SteerMode::Boundary {
        assert!(
            !second.iter().any(|message| message.content() == "steer-2"),
            "边界一次只放行一条，后续输入必须留到下一轮"
        );
    }
    assert!(!second
        .iter()
        .any(|message| message.content() == "idle-only"));
    let tool_position = second
        .iter()
        .position(|message| {
            matches!(
                message,
                BaseMessage::Tool { tool_call_id, .. } if tool_call_id == "call-block"
            )
        })
        .expect("第二轮请求必须包含工具结果");
    let steer_positions: Vec<_> = expected_steers
        .iter()
        .map(|text| {
            second
                .iter()
                .position(|message| {
                    matches!(message, BaseMessage::Human { .. }) && message.content() == *text
                })
                .unwrap_or_else(|| panic!("第二轮请求缺少 {text}"))
        })
        .collect();
    assert!(
        steer_positions
            .iter()
            .all(|position| tool_position < *position)
            && steer_positions.windows(2).all(|pair| pair[0] < pair[1]),
        "steer 必须按 MQ 顺序排在工具结果之后: {steer_positions:?}, tool={tool_position}"
    );
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

    let result = crate::agent::stages::run_react_loop(ctx, 10).await;
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
    ctx.session.queue.push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("cancel while reasoning"),
    ));
    let loop_ctx = ctx.clone();
    let task =
        tokio::spawn(async move { crate::agent::stages::run_react_loop(loop_ctx, 10).await });
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
    let result = crate::agent::stages::run_react_loop(ctx.clone(), 0).await;
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
async fn idle_suspension_waits_for_render_capacity() {
    use crate::agent::events_v2::{EventBus, EventBusConfig, RenderEvent};
    let mut context = make_stage_context();
    let (bus, mut handles) = EventBus::new(EventBusConfig {
        render_capacity: 1,
        ..Default::default()
    });
    let bus = Arc::new(bus);
    context.runtime.event_bus = bus.clone();
    context.async_ctx.idle_wait_enabled = true;
    context.async_ctx.idle_should_wait = Some(Arc::new(|| true));
    let cancel = context.session.turn.cancel_token.clone();
    bus.emit_render(RenderEvent::TurnSuspended {
        turn_id: context.turn_id(),
        agent_id: context.session.agent_id,
    });
    let running = crate::agent::stages::run_react_loop(context, 1);
    tokio::pin!(running);
    assert!(
        std::future::poll_fn(|cx| {
            std::task::Poll::Ready(std::future::Future::poll(running.as_mut(), cx).is_pending())
        })
        .await
    );
    assert!(!bus.health().dirty);
    handles
        .render_rx
        .recv()
        .await
        .expect("handles render rx producer must deliver the expected test event before closing");
    assert!(
        std::future::poll_fn(|cx| {
            std::task::Poll::Ready(std::future::Future::poll(running.as_mut(), cx).is_pending())
        })
        .await
    );
    assert!(matches!(
        handles.try_render(),
        Some(RenderEvent::TurnSuspended { .. })
    ));
    assert!(!bus.health().dirty);
    cancel.cancel();
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), running)
        .await
        .expect("the timeout fixture result is required for this assertion");
    assert!(matches!(result, LoopResult::Interrupted));
}
