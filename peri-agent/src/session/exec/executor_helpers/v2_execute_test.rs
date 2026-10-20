//! forwarder 交付序号绑定的单元测试（无 ACP 装配面）。

use std::sync::Arc;

use parking_lot::Mutex;
use peri_acp_types::{
    event::{EventPublisher, ExecutorEvent},
    identity::{EventDeliveryClass, SessionSeq},
    messages::{MessageContent, MessageId},
    runtime::UnstampedEvent,
    session::{EnqueueUserInputRequest, MessageQueue, SessionInbox},
};
use tokio_util::sync::CancellationToken;

use crate::session::user_input_mailbox::UserInputMailbox;

use super::publish_forwarded_event;

struct RecordingPublisher {
    next: Mutex<Option<SessionSeq>>,
    published: Mutex<Vec<String>>,
}

impl RecordingPublisher {
    fn new() -> Self {
        Self {
            next: Mutex::new(Some(SessionSeq::initial())),
            published: Mutex::new(Vec::new()),
        }
    }

    fn unavailable() -> Self {
        Self {
            next: Mutex::new(None),
            published: Mutex::new(Vec::new()),
        }
    }

    fn take_seq(&self) -> Option<SessionSeq> {
        let mut next = self.next.lock();
        let seq = (*next)?;
        *next = Some(seq.next());
        Some(seq)
    }
}

impl EventPublisher for RecordingPublisher {
    fn publish_event(
        &self,
        session_id: &str,
        _source: &UnstampedEvent,
        _event: ExecutorEvent,
    ) -> Option<SessionSeq> {
        self.published.lock().push(session_id.to_string());
        self.take_seq()
    }
}

fn source() -> UnstampedEvent {
    UnstampedEvent::new("turn", "agent", None, EventDeliveryClass::Critical)
}

fn delivery_event(input_id: &str) -> ExecutorEvent {
    ExecutorEvent::UserInputDelivered {
        generation: "generation".into(),
        input_id: input_id.to_string(),
        content: MessageContent::text("问题"),
    }
}

/// 构造一条已写入交付日志的输入，返回 mailbox 与 input_id。
fn mailbox_with_delivery() -> (Arc<UserInputMailbox>, String) {
    let inbox = Arc::new(SessionInbox::new(Arc::new(MessageQueue::new())));
    let mailbox = UserInputMailbox::new("session".into(), inbox.clone(), Arc::new(|_| {}));
    mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let request = EnqueueUserInputRequest {
        session_id: "session".into(),
        generation: mailbox.generation().into(),
        command_id: "command".into(),
        input_id: uuid::Uuid::now_v7().to_string(),
        content: MessageContent::text("问题"),
        original_draft: "问题".into(),
    };
    mailbox.enqueue(&request).unwrap();
    mailbox.enter_idle();
    let messages = inbox.queue().drain_all();
    let id = messages[0].message().unwrap().id();
    mailbox.mark_claimed(&[id]);
    mailbox.mark_delivered(&[id]);
    (mailbox, request.input_id)
}

fn bound_sequence(mailbox: &UserInputMailbox, input_id: &str) -> Option<SessionSeq> {
    mailbox
        .pending_delivery_notifications()
        .into_iter()
        .find(|entry| entry.input_id == input_id)
        .and_then(|entry| entry.session_seq)
}

#[test]
fn forwarder_binds_the_published_sequence_to_the_delivery_journal() {
    let (mailbox, input_id) = mailbox_with_delivery();
    let publisher = RecordingPublisher::new();

    let published = publish_forwarded_event(
        &publisher,
        "session",
        &source(),
        delivery_event(&input_id),
        Some(mailbox.as_ref()),
    );

    let allocated = SessionSeq::initial();
    assert_eq!(published, Some(allocated));
    assert_eq!(*publisher.published.lock(), vec!["session".to_string()]);
    assert_eq!(
        bound_sequence(&mailbox, &input_id),
        Some(allocated),
        "交付日志必须记录实际发布序号，供泵在水位前补发"
    );
}

#[test]
fn forwarder_does_not_bind_when_publish_returns_no_sequence() {
    let (mailbox, input_id) = mailbox_with_delivery();
    let publisher = RecordingPublisher::unavailable();

    let published = publish_forwarded_event(
        &publisher,
        "session",
        &source(),
        delivery_event(&input_id),
        Some(mailbox.as_ref()),
    );

    assert_eq!(published, None);
    assert_eq!(bound_sequence(&mailbox, &input_id), None);
}

#[test]
fn forwarder_does_not_touch_mailbox_for_non_delivery_events() {
    let (mailbox, _input_id) = mailbox_with_delivery();
    let publisher = RecordingPublisher::new();

    publish_forwarded_event(
        &publisher,
        "session",
        &source(),
        ExecutorEvent::TextChunk {
            message_id: MessageId::new(),
            chunk: "回答".into(),
            source_agent_id: None,
        },
        Some(mailbox.as_ref()),
    );

    assert_eq!(*publisher.published.lock(), vec!["session".to_string()]);
    assert!(
        mailbox
            .pending_delivery_notifications()
            .iter()
            .all(|entry| entry.session_seq.is_none()),
        "非交付事件不得改动交付日志"
    );
}

// ── 主 executor hook-stop 端到端 ─────────────────────────────────────────────

struct StopProbeTool;

#[async_trait::async_trait]
impl crate::tools::BaseTool for StopProbeTool {
    fn name(&self) -> &str {
        "StopProbeTool"
    }
    fn description(&self) -> &str {
        "fixture tool for main-executor hook stop"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok("tool done".into())
    }
}

struct StopAfterToolBatch;

#[async_trait::async_trait]
impl crate::middleware::Middleware for StopAfterToolBatch {
    fn name(&self) -> &str {
        "stop_after_tool_batch"
    }

    async fn after_tools_batch(
        &self,
        state: &mut dyn crate::middleware::capabilities::AfterToolsBatchState,
        results: &[(
            crate::agent::react::ToolCall,
            crate::agent::react::ToolResult,
        )],
    ) -> crate::error::AgentResult<()> {
        assert_eq!(results.len(), 1);
        assert!(!results[0].1.is_error);
        state.enqueue_stop_intent(crate::session::producer_reminders::trusted_reminder(
            peri_acp_types::system_reminder::ReminderCategory::Lifecycle,
            "hook",
            "post_tool_batch_stop",
            peri_acp_types::system_reminder::ReminderSeverity::Info,
            peri_acp_types::system_reminder::ReminderDelivery::Configurable,
            "main hook stop".into(),
            None,
            serde_json::json!({"continue": false}),
        ));
        Ok(())
    }
}

struct OneToolLlm {
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::agent::react::ReactLLM for OneToolLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[crate::messages::BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        assert_eq!(
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
            0,
            "hook stop must not issue a second model request"
        );
        Ok(crate::agent::react::Reasoning::with_tools(
            "call the probe",
            vec![crate::agent::react::ToolCall::new(
                "call-1",
                "StopProbeTool",
                serde_json::json!({}),
            )],
        ))
    }
}

struct NoopPublisher;

impl EventPublisher for NoopPublisher {
    fn publish_event(
        &self,
        _session_id: &str,
        _source: &UnstampedEvent,
        _event: ExecutorEvent,
    ) -> Option<SessionSeq> {
        None
    }
}

fn draining_forwarder() -> super::ForwarderLauncherFn {
    Arc::new(
        |mut handles: peri_acp_types::event_v2::EventHandles,
         _agent_id: String,
         _on_event: Box<dyn Fn(UnstampedEvent, ExecutorEvent) + Send + Sync>| {
            tokio::spawn(async move { while handles.render_rx.recv().await.is_some() {} })
        },
    )
}

/// 主 Agent 在 PostToolBatch 请求停止：executor 必须正常 Done，不得 panic 或二次推理。
#[tokio::test]
async fn main_executor_hook_stop_finishes_done_without_panic() {
    use peri_acp_types::command::PromptStopReason;

    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls_for_build = calls.clone();
    let stage_build: super::StageBuildFn = Arc::new(move |sbr: super::StageBuildRequest| {
        use crate::agent::stages::StageContext;
        use crate::middleware::chain::MiddlewareChain;
        use peri_acp_types::event_v2::EventBus;

        let session = crate::session::Session::new(
            Arc::from("/tmp"),
            sbr.frozen_session.v2_frozen().clone(),
            None,
        );
        let turn = session.start_turn();
        let mut chain = MiddlewareChain::new();
        chain.add(Box::new(StopAfterToolBatch));
        let mut tools = std::collections::BTreeMap::new();
        tools.insert(
            "StopProbeTool".to_string(),
            Arc::new(StopProbeTool) as Arc<dyn crate::tools::BaseTool>,
        );
        let tools: crate::agent::stages::SharedToolMap = Arc::new(parking_lot::RwLock::new(tools));
        let (bus, handles) = EventBus::new(Default::default());
        let context = StageContext::builder(turn, session.transcript(), session.queue().clone())
            .with_llm(Arc::new(OneToolLlm {
                calls: calls_for_build.clone(),
            }))
            .with_tools(tools)
            .with_middleware_chain(Arc::new(chain))
            .with_event_bus(Arc::new(bus))
            .build();
        let (_todo_tx, todo_rx) = tokio::sync::mpsc::channel(8);
        let (_bg_tx, bg_event_rx) = tokio::sync::mpsc::unbounded_channel();
        Ok((
            crate::session::exec::stage_builder::V2AgentOutput {
                context,
                session,
                event_handles: handles,
                todo_rx,
                bg_event_rx,
            },
            None,
        ))
    });

    let outcome = super::build_and_execute_agent_v2(super::V2ExecuteRequest {
        session_id: "main-hook-stop".into(),
        user_input_mailbox: None,
        cwd: "/tmp".into(),
        cancel: CancellationToken::new(),
        session_resources: None,
        thread_id: None,
        agent_input: crate::agent::react::AgentInput::text("run probe"),
        history_payloads: vec![],
        cached_llm: None,
        task_manager: None,
        continuation: false,
        frozen_session: crate::session::exec::executor::FrozenSessionData::from_frozen_parts(
            crate::session::FrozenContext::builder().build(),
            None,
        ),
        event_handler: Arc::new(peri_acp_types::event::FnEventHandler(|_: ExecutorEvent| {})),
        agent_overrides: None,
        preload_skills: vec![],
        child_handler_factory: None,
        auxiliary_model: None,
        thread_persistence: Default::default(),
        goal_controller: None,
        on_bg_complete: None,
        publisher: Arc::new(NoopPublisher),
        stage_build,
        store_llm: Arc::new(|_| {}),
        cancel_cascade: Arc::new(|_| {}),
        forwarder_launcher: draining_forwarder(),
        continuation_notify: None,
    })
    .await;

    assert!(outcome.ok, "主会话 HookStop 必须正常结束");
    assert_eq!(outcome.stop_reason, PromptStopReason::EndTurn);
    assert!(outcome.failure.is_none(), "HookStop 不得产生 failure");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}
