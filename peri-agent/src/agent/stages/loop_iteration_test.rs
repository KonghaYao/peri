//! 循环语义迭代预算、Receive 唤醒与 idle 重试。
use crate::agent::events::Stage;
use crate::agent::events::StageStatus;

use crate::agent::events_v2::ObserveEvent;
use crate::agent::react::ReactLLM;

use crate::agent::stages::context::StageContext;
use crate::agent::stages::LoopResult;
use crate::agent::stages::SharedToolMap;
use crate::messages::BaseMessage;
use crate::middleware::capabilities as hook_state;

use crate::session::queue::MessageSource;
use crate::session::store::FrozenContext;

use crate::session::QueuedMessage;
use crate::session::Session;

use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;

struct CountingFinalAnswerLLM {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    answer: &'static str,
}

struct CompletionDuringReasonLlm {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    queue: crate::session::MessageQueue,
    passive_prefix: usize,
}

struct StopDuringReasonLlm {
    completion: CompletionDuringReasonLlm,
    stop_first: bool,
}

struct FailingFollowupLlm(Arc<std::sync::atomic::AtomicUsize>);

#[async_trait::async_trait]
impl ReactLLM for FailingFollowupLlm {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        self.0.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            messages
                .iter()
                .filter(|message| message
                    .content()
                    .contains("background result awaiting parent reasoning"))
                .count(),
            1
        );
        Err(anyhow::anyhow!("followup model failed").into())
    }
}

#[async_trait::async_trait]
impl ReactLLM for StopDuringReasonLlm {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        tools: &[&dyn crate::tools::BaseTool],
        streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        let stop = QueuedMessage::info(MessageSource::HookStopIntent, BaseMessage::human("stop"));
        if self.stop_first {
            self.completion.queue.push_sync(stop.clone());
        }
        let answer = self
            .completion
            .generate_reasoning(messages, tools, streaming)
            .await?;
        if !self.stop_first {
            self.completion.queue.push_sync(stop);
        }
        Ok(answer)
    }
}

#[tokio::test]
async fn hook_stop_in_budget_boundary_batch_wins_without_followup_activation() {
    for stop_first in [true, false] {
        let session = Session::new(
            Arc::from("/tmp/stop-budget-boundary"),
            FrozenContext::builder().build(),
            None,
        );
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        session.queue().push_sync(QueuedMessage::prompt(
            MessageSource::UserInput,
            BaseMessage::human("initial work"),
        ));
        let context = StageContext::builder(
            session.start_turn(),
            session.transcript(),
            session.queue().clone(),
        )
        .with_llm(Arc::new(StopDuringReasonLlm {
            completion: CompletionDuringReasonLlm {
                calls: calls.clone(),
                queue: session.queue().clone(),
                passive_prefix: 0,
            },
            stop_first,
        }))
        .build();
        assert!(matches!(
            crate::agent::stages::run_react_loop(context, 1).await,
            LoopResult::HookStopped { unprocessed_tool_calls } if unprocessed_tool_calls.is_empty()
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(session.queue().is_empty_sync());
        let delivery_id = crate::agent::async_tasks::delivery::terminal_delivery_id(
            "bg-budget-boundary",
            "terminal",
        );
        assert!(session.transcript().read().get(delivery_id).is_some());
    }
}

#[async_trait::async_trait]
impl ReactLLM for CompletionDuringReasonLlm {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            for _ in 0..self.passive_prefix {
                self.queue.push_sync(QueuedMessage::info(
                    MessageSource::SystemInjected,
                    BaseMessage::human("passive batch filler"),
                ));
            }
            let complete = crate::session::bg_complete::task_bg_complete_callback(
                crate::session::bg_complete::queue_terminal_delivery(self.queue.clone()),
            );
            complete(
                &crate::agent::events::BackgroundTaskResult {
                    task_id: "bg-budget-boundary".to_string(),
                    agent_name: "budget-boundary-child".to_string(),
                    prompt_summary: "complete during parent reasoning".to_string(),
                    success: true,
                    output: "background result awaiting parent reasoning".to_string(),
                    tool_calls_count: 0,
                    duration_ms: 1,
                    child_thread_id: None,
                    timed_out: false,
                    subagent_failure: None,
                    shell_output: None,
                },
                peri_acp_types::tasks::BgTaskKind::Agent,
            )
            .expect("production completion callback must accept the result");
            assert!(self.queue.has_ensure_processing_sync());
        } else {
            assert_eq!(
                messages
                    .iter()
                    .filter(|message| message
                        .content()
                        .contains("background result awaiting parent reasoning"))
                    .count(),
                1,
                "the followup model request must contain the canonical result exactly once"
            );
        }
        Ok(crate::agent::react::Reasoning::with_answer("", "done"))
    }
}

async fn completion_during_reason_fixture(
    max_iterations: usize,
) -> (
    Arc<Session>,
    Arc<std::sync::atomic::AtomicUsize>,
    LoopResult,
) {
    completion_during_reason_with_prefix(max_iterations, 0).await
}

async fn completion_during_reason_with_prefix(
    max_iterations: usize,
    passive_prefix: usize,
) -> (
    Arc<Session>,
    Arc<std::sync::atomic::AtomicUsize>,
    LoopResult,
) {
    let session = Session::new(
        Arc::from("/tmp/background-budget-boundary"),
        FrozenContext::builder().build(),
        None,
    );
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    session.queue().push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("start parent reasoning"),
    ));
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(CompletionDuringReasonLlm {
        calls: Arc::clone(&calls),
        queue: session.queue().clone(),
        passive_prefix,
    }))
    .build();
    let result = crate::agent::stages::run_react_loop(context, max_iterations).await;
    (session, calls, result)
}

#[tokio::test]
async fn background_completion_at_budget_boundary_retains_followup_eligibility() {
    let (session, calls, result) = completion_during_reason_fixture(1).await;
    assert!(matches!(
        result,
        LoopResult::Error(crate::error::AgentError::MaxIterationsExceeded(1))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let delivery_id =
        crate::agent::async_tasks::delivery::terminal_delivery_id("bg-budget-boundary", "terminal");
    assert!(session.transcript().read().get(delivery_id).is_none());
    assert!(session.queue().has_ensure_processing_after_sync(None));
    assert!(session
        .queue()
        .has_pending_defer_sync(&MessageSource::SubAgentComplete));
    let pending = session.queue().snapshot_sync();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].delivery_id, Some(delivery_id));
    // MQ 以 (enqueued_at, id) 排序，admission_sequence 不再是队列事实。
    assert!(pending[0].enqueued_at.is_some());
    session.queue().push_batch_sync(pending);
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(CompletionDuringReasonLlm {
        calls: Arc::clone(&calls),
        queue: session.queue().clone(),
        passive_prefix: 0,
    }))
    .build();
    assert!(matches!(
        crate::agent::stages::run_react_loop(context, 10).await,
        LoopResult::Completed
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(session.transcript().read().get(delivery_id).is_some());
    assert!(session.queue().is_empty_sync());
}

#[tokio::test]
async fn background_completion_during_reason_runs_followup_with_remaining_budget() {
    let (_, calls, result) = completion_during_reason_fixture(2).await;
    assert!(matches!(result, LoopResult::Completed));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn deferred_result_that_reaches_failed_reason_is_not_requeued() {
    let (session, calls, _) = completion_during_reason_fixture(1).await;
    let watermark = session.queue().admission_watermark_sync();
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(FailingFollowupLlm(calls.clone())))
    .build();
    assert!(matches!(
        crate::agent::stages::run_react_loop(context, 1).await,
        LoopResult::Error(crate::error::AgentError::Other(_))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(session.queue().is_empty_sync());
    // 队列已排空，水位随行消失；「没有新准入」由 after 判定保证。
    assert!(session.queue().admission_watermark_sync().is_none());
    assert!(!session
        .queue()
        .has_ensure_processing_after_sync(watermark.as_ref()));
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(FailingFollowupLlm(calls.clone())))
    .build();
    assert!(matches!(
        crate::agent::stages::run_react_loop(context, 1).await,
        LoopResult::Completed
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn background_completion_after_receive_batch_boundary_retains_original_admission() {
    let (session, calls, result) = completion_during_reason_with_prefix(1, 64).await;
    assert!(matches!(
        result,
        LoopResult::Error(crate::error::AgentError::MaxIterationsExceeded(1))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let pending = session.queue().drain_all_sync();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].delivery_id,
        Some(crate::agent::async_tasks::delivery::terminal_delivery_id(
            "bg-budget-boundary",
            "terminal"
        ))
    );
}

#[tokio::test]
async fn deferred_result_in_zero_budget_followup_does_not_reactivate_itself() {
    let (session, calls, _) = completion_during_reason_fixture(1).await;
    let watermark = session.queue().admission_watermark_sync();
    assert!(session.queue().has_ensure_processing_after_sync(None));
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(CompletionDuringReasonLlm {
        calls: Arc::clone(&calls),
        queue: session.queue().clone(),
        passive_prefix: 0,
    }))
    .build();
    assert!(matches!(
        crate::agent::stages::run_react_loop(context, 0).await,
        LoopResult::Error(crate::error::AgentError::MaxIterationsExceeded(0))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // 队列已排空，水位随行消失；「没有新准入」由 after 判定保证。
    assert!(session.queue().admission_watermark_sync().is_none());
    assert!(!session
        .queue()
        .has_ensure_processing_after_sync(watermark.as_ref()));
    assert!(session.queue().is_empty_sync());
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(CompletionDuringReasonLlm {
        calls: Arc::clone(&calls),
        queue: session.queue().clone(),
        passive_prefix: 0,
    }))
    .build();
    assert!(matches!(
        crate::agent::stages::run_react_loop(context, 1).await,
        LoopResult::Completed
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn exhausted_budget_idle_wake_retains_new_required_message() {
    let session = Session::new(
        Arc::from("/tmp/idle-budget-boundary"),
        FrozenContext::builder().build(),
        None,
    );
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let suspended = Arc::new(AtomicBool::new(false));
    session.queue().push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("initial work"),
    ));
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(CountingFinalAnswerLLM {
        calls: Arc::clone(&calls),
        answer: "done",
    }))
    .with_idle_waiting()
    .with_idle_should_wait(Arc::new(|| true))
    .with_idle_suspended_flag(Arc::clone(&suspended))
    .build();
    let owner = tokio::spawn(crate::agent::stages::run_react_loop(context, 1));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !suspended.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the first answer must enter idle before result arrival");
    let message = BaseMessage::human("late idle result");
    let message_id = message.id();
    session.queue().push_sync(QueuedMessage::defer(
        MessageSource::SubAgentComplete,
        message,
    ));
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(2), owner)
            .await
            .expect("the from secs fixture result is required for this assertion")
            .expect("exhausted budget idle wake retains new required message: the completed fixture operation must provide its result payload"),
        LoopResult::Error(crate::error::AgentError::MaxIterationsExceeded(1))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(session.transcript().read().get(message_id).is_none());
    let pending = session.queue().drain_all_sync();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0]
            .message()
            .expect("the fixture entry must contain a message payload")
            .id(),
        message_id
    );
}

#[tokio::test]
async fn loaded_history_never_replays_old_tools_and_new_input_starts_a_fresh_run() {
    let session = Session::new(Arc::from("/tmp"), FrozenContext::builder().build(), None);
    session
        .transcript()
        .write()
        .append(BaseMessage::ai_with_tool_calls(
            "old answer",
            vec![crate::messages::ToolCallRequest::new(
                "old-model-call",
                "OldDelegate",
                serde_json::json!({}),
            )],
        ));
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(CountingFinalAnswerLLM {
        calls: calls.clone(),
        answer: "fresh answer",
    }))
    .build();
    assert!(matches!(
        crate::agent::stages::run_react_loop(context, 10).await,
        LoopResult::Completed
    ));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    session.queue().push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("new input"),
    ));
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(CountingFinalAnswerLLM {
        calls: calls.clone(),
        answer: "fresh answer",
    }))
    .build();
    assert!(matches!(
        crate::agent::stages::run_react_loop(context, 10).await,
        LoopResult::Completed
    ));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        session
            .transcript()
            .read()
            .visible_messages()
            .last()
            .expect("session transcript fixture must contain the expected element")
            .content(),
        "fresh answer"
    );
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
        state: &mut dyn hook_state::BeforeAgentState,
    ) -> crate::error::AgentResult<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.prompt_visible
            .lock()
            .expect("self prompt visible mutex must remain unpoisoned throughout the test")
            .push(
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
include!("loop_iteration_budget_cases_test.rs");

fn async_result_reminder(task_id: &str) -> peri_acp_types::system_reminder::TrustedSystemReminder {
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource as CanonicalSource, SystemReminder, TrustedSystemReminderFactory,
        SYSTEM_REMINDER_VERSION,
    };
    TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: CanonicalSource("shell".into()),
            kind: "terminal".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: format!("后台任务已结束\ntask_id: {task_id}\nstate: succeeded"),
            summary: None,
            metadata: serde_json::json!({"task_id": task_id}),
        })
        .expect("the construct fixture result is required for this assertion")
}

fn ready_result(context: &StageContext, task_id: &str) -> crate::messages::MessageId {
    let id = crate::messages::MessageId::new();
    let reminder = async_result_reminder(task_id);
    let mailbox = &context.session.async_result_mailbox;
    mailbox
        .accept(
            id,
            &reminder,
            peri_acp_types::session::MessageSource::ShellComplete,
        )
        .expect("the accept fixture result is required for this assertion");
    mailbox
        .mark_canonical_ready(
            id,
            &reminder,
            peri_acp_types::session::MessageSource::ShellComplete,
            None,
        )
        .expect("the mark canonical ready fixture result is required for this assertion");
    id
}

/// 队列为空但 mailbox 仍有 Ready 结果时，循环必须进入本次请求处理，
/// 不能以“没有可唤醒消息”为由退出并留下自动续跑候选。
#[tokio::test]
async fn async_result_ready_without_queue_work_runs_followup() {
    let session = Session::new(
        Arc::from("/tmp/async-ready-no-queue"),
        FrozenContext::builder().build(),
        None,
    );
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(CountingFinalAnswerLLM {
        calls: calls.clone(),
        answer: "done",
    }))
    .build();

    let _id = ready_result(&context, "shell-no-queue");
    assert!(context.session.async_result_mailbox.has_ready());
    assert!(session.queue().is_empty_sync());

    let result = crate::agent::stages::run_react_loop(context.clone(), 5).await;
    assert!(matches!(result, LoopResult::Completed));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "Ready 结果必须触发一次真实主模型请求"
    );
    assert_eq!(
        context.session.async_result_mailbox.pending_count(),
        0,
        "处理完成后结果应 Covered"
    );
    assert!(!context.session.async_result_mailbox.has_ready());
}

/// [T12] Hook stop 必须在退出前挂起未处理结果：不能保持 Ready 触发自动续跑，
/// 也不得伪造 Covered。
#[tokio::test]
async fn async_result_hook_stop_suspends_ready_result() {
    let session = Session::new(
        Arc::from("/tmp/async-result-hook-stop"),
        FrozenContext::builder().build(),
        None,
    );
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(CountingFinalAnswerLLM {
        calls: calls.clone(),
        answer: "done",
    }))
    .build();

    let id = ready_result(&context, "shell-stop");
    session.queue().push_sync(QueuedMessage::info(
        MessageSource::HookStopIntent,
        BaseMessage::human("stop"),
    ));

    let result = crate::agent::stages::run_react_loop(context.clone(), 5).await;
    assert!(
        matches!(result, LoopResult::HookStopped { .. }),
        "Hook stop 必须经唯一退出口返回 HookStopped，实际 {result:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0, "Stop 之后不得发起模型请求");
    assert_eq!(
        context.session.async_result_mailbox.state_of(&id),
        Some(crate::session::async_result_mailbox::AsyncResultState::Suspended),
        "未处理结果必须挂起，而不是保持 Ready 或伪装 Covered"
    );
    assert!(!context.session.async_result_mailbox.has_ready());
    assert_eq!(context.session.async_result_mailbox.pending_count(), 1);
}
