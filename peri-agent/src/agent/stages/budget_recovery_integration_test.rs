use super::*;
use crate::agent::react::{AgentOutput, Reasoning, StreamingContext};
use crate::error::{AgentError, AgentResult};
use crate::middleware::{
    capabilities::{AfterAgentState, BeforeAgentState},
    Middleware,
};
use crate::session::store::FrozenContext;
use crate::session::{MessageKind, MessageQueue, MessageSource, Session};
use crate::thread::{SqliteThreadStore, ThreadId, ThreadMeta, ThreadStore};
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminder, TrustedSystemReminderFactory,
    SYSTEM_REMINDER_VERSION,
};
use std::sync::atomic::AtomicUsize;

fn make_reminder(marker: &str) -> TrustedSystemReminder {
    TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSource("budget_recovery_test".into()),
            kind: "steering".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: format!("{marker}\n{}", "retained instruction\n".repeat(2_000)),
            summary: None,
            metadata: serde_json::json!({}),
        })
        .unwrap()
}

struct ContinuingReminderMiddleware {
    // The fixture owns the same session inbox as an external reminder producer.
    // before_agent itself intentionally has no queue mutation capability.
    queue: MessageQueue,
}

#[async_trait::async_trait]
impl Middleware for ContinuingReminderMiddleware {
    fn name(&self) -> &str {
        "budget_recovery_reminder"
    }

    async fn before_agent(&self, _state: &mut dyn BeforeAgentState) -> AgentResult<()> {
        self.queue.push(QueuedMessage::system_reminder(
            MessageKind::Defer,
            MessageSource::GoalSteering,
            make_reminder("before_agent reminder"),
        ));
        Ok(())
    }

    async fn after_agent(
        &self,
        state: &mut dyn AfterAgentState,
        output: &AgentOutput,
    ) -> AgentResult<AgentOutput> {
        state.enqueue_v2_message(QueuedMessage::system_reminder(
            MessageKind::Defer,
            MessageSource::GoalSteering,
            make_reminder("after_agent reminder"),
        ));
        Ok(output.clone())
    }
}

struct HighUsageAnswerModel {
    calls: Arc<AtomicUsize>,
    requests: Arc<parking_lot::Mutex<Vec<Vec<BaseMessage>>>>,
    cancel_on_third: bool,
}

#[async_trait::async_trait]
impl ReactLLM for HighUsageAnswerModel {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _tools: &[&dyn BaseTool],
        streaming: Option<StreamingContext>,
    ) -> AgentResult<Reasoning> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        self.requests.lock().push(messages.to_vec());
        if self.cancel_on_third && call == 3 {
            streaming.unwrap().cancel.cancel();
        }
        let mut reasoning = Reasoning::with_answer("working", "continue");
        reasoning.usage = Some(peri_model::TokenUsage {
            input_tokens: 96_000,
            output_tokens: 10,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: Some(90_000),
        });
        reasoning.model = "scripted-budget-recovery".into();
        reasoning.request_id = Some(format!("budget-request-{call}"));
        Ok(reasoning)
    }
}

struct CountingSummaryModel(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl peri_model::Model for CountingSummaryModel {
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
        unreachable!("Full只调用complete")
    }

    async fn complete(
        &self,
        _request: peri_model::ModelRequest,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> peri_model::ModelResult<peri_model::ModelResponse> {
        let call = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        peri_model::ModelResponse::new(
            peri_model::ModelMessage::assistant_text(format!(
                "<summary>budget-summary-{call}</summary>"
            )),
            peri_model::StopReason::EndTurn,
            None,
            None,
        )
    }
}

struct BudgetScenario {
    _dir: tempfile::TempDir,
    context: StageContext,
    store: Arc<SqliteThreadStore>,
    thread_id: ThreadId,
    reason_calls: Arc<AtomicUsize>,
    compact_calls: Arc<AtomicUsize>,
    requests: Arc<parking_lot::Mutex<Vec<Vec<BaseMessage>>>>,
    handles: crate::agent::events_v2::EventHandles,
}

async fn make_scenario(cancel_on_third: bool) -> BudgetScenario {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        SqliteThreadStore::new(dir.path().join("budget.db"))
            .await
            .unwrap(),
    );
    let thread_id = store
        .create_thread(ThreadMeta::new(dir.path().to_string_lossy()))
        .await
        .unwrap();
    let session = Session::new(
        Arc::from(dir.path().to_string_lossy().as_ref()),
        FrozenContext::builder().build(),
        Some(thread_id.clone()),
    );
    {
        let transcript = session.transcript();
        let mut guard = transcript.write();
        *guard = std::mem::take(&mut *guard).with_persistence(store.clone(), thread_id.clone());
        guard.append_system_reminder(make_reminder("initial retained reminder A"));
        guard.append_system_reminder(make_reminder("initial retained reminder B"));
    }
    let reason_calls = Arc::new(AtomicUsize::new(0));
    let compact_calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(ContinuingReminderMiddleware {
        queue: session.queue().clone(),
    }));
    let (bus, handles) = crate::agent::events_v2::EventBus::new(Default::default());
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(HighUsageAnswerModel {
        calls: reason_calls.clone(),
        requests: requests.clone(),
        cancel_on_third,
    }))
    .with_compact_llm(Arc::new(CountingSummaryModel(compact_calls.clone())))
    .with_context_budget(ContextBudget::new(100_000))
    .with_compact_config(CompactConfig::default())
    .with_middleware_chain(Arc::new(chain))
    .with_event_bus(Arc::new(bus))
    .build();
    context.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("original budget task"),
    ));
    BudgetScenario {
        _dir: dir,
        context,
        store,
        thread_id,
        reason_calls,
        compact_calls,
        requests,
        handles,
    }
}

/// [回归测试] 合法Reminder持续驱动RCRA，两个真实SQLite Full仍有新高usage时明确终止。
#[tokio::test]
async fn test_budget_recovery_loop_stops_after_two_committed_fulls() {
    let mut scenario = make_scenario(false).await;
    let result = run_react_loop(scenario.context.clone(), 6).await;
    assert!(matches!(
        result,
        LoopResult::Error(AgentError::CompactBudgetUnrecovered {
            input_tokens: 96_000,
            context_window: 100_000,
            full_attempts: 2,
        })
    ));
    assert_eq!(scenario.reason_calls.load(Ordering::SeqCst), 3);
    assert_eq!(scenario.compact_calls.load(Ordering::SeqCst), 2);
    let events: Vec<_> = std::iter::from_fn(|| scenario.handles.try_observe()).collect();
    assert_eq!(events.iter().filter(|event| matches!(event, ObserveEvent::MessagesCompacted { outcome, .. } if outcome.is_full_applied())).count(), 2);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                ObserveEvent::LlmCallEnd {
                    input_tokens: 96_000,
                    ..
                }
            ))
            .count(),
        3,
        "错误退出前最后一条真实usage仍须完成观测"
    );
    {
        let requests = scenario.requests.lock();
        for (index, request) in requests.iter().enumerate() {
            assert!(request
                .iter()
                .any(|message| message.content().contains("initial retained reminder A")));
            assert!(request
                .iter()
                .any(|message| message.content().contains("initial retained reminder B")));
            if index > 0 {
                assert!(request.iter().any(|message| message
                    .content()
                    .contains(&format!("budget-summary-{index}"))));
                assert!(!request
                    .iter()
                    .any(|message| message.content().contains("original budget task")));
            }
        }
    }
    let tx = scenario
        .context
        .session
        .transcript
        .read()
        .persist_tx_handle()
        .unwrap();
    MessageTranscript::flush_via_tx(&tx).await.unwrap();
    let payloads = scenario
        .store
        .load_payloads(&scenario.thread_id)
        .await
        .unwrap();
    let flags = scenario
        .store
        .load_message_flags(&scenario.thread_id)
        .await
        .unwrap();
    let reminder_ids: Vec<_> = payloads
        .iter()
        .filter_map(|payload| match payload {
            PersistedPayload::SystemReminder { id, .. } => Some(*id),
            _ => None,
        })
        .collect();
    assert_eq!(
        reminder_ids.len(),
        5,
        "初始两条、before_agent一条及两次after_agent提醒都持久保留"
    );
    assert!(reminder_ids.iter().all(|id| !flags.contains_key(id)));
    let summaries: Vec<_> = payloads
        .iter()
        .filter_map(|payload| match payload {
            PersistedPayload::Message(message) if message.content().contains("budget-summary-") => {
                Some(message)
            }
            _ => None,
        })
        .collect();
    assert_eq!(summaries.len(), 2);
    assert!(flags.get(&summaries[0].id()).unwrap().excluded);
    assert!(!flags
        .get(&summaries[1].id())
        .is_some_and(|flag| flag.excluded));
}

#[tokio::test]
async fn test_budget_recovery_loop_cancel_wins_over_second_high_observation() {
    let scenario = make_scenario(true).await;
    let result = run_react_loop(scenario.context.clone(), 6).await;
    assert!(matches!(result, LoopResult::Interrupted));
    assert_eq!(scenario.reason_calls.load(Ordering::SeqCst), 3);
    assert_eq!(scenario.compact_calls.load(Ordering::SeqCst), 2);
    assert!(scenario.context.session.turn.cancel_token.is_cancelled());
}
