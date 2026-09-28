//! 手动 Full 的完整 payload 恢复、报告排除与冷加载。
use super::*;
use peri_acp_types::session_resources::{BindingState, ChildSnapshot, FrozenState};
use peri_acp_types::store::InheritedContext;
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};
use peri_agent::session::MessageTranscript;
use peri_model::{
    Model, ModelCapabilities, ModelMessage, ModelRequest, ModelResponse, ModelResult, ModelStream,
    StopReason,
};
use tokio_util::sync::CancellationToken;

fn report(body: &str) -> PersistedPayload {
    PersistedPayload::SystemReminder {
        id: MessageId::new(),
        reminder: TrustedSystemReminderFactory::for_producer()
            .construct(SystemReminder {
                version: SYSTEM_REMINDER_VERSION,
                category: ReminderCategory::Task,
                source: ReminderSource("subagent".into()),
                kind: "completed".into(),
                severity: ReminderSeverity::Info,
                delivery: ReminderDelivery::Configurable,
                audiences: ReminderAudiences(vec![ReminderAudience::Model]),
                body: body.to_owned(),
                summary: None,
                metadata: serde_json::json!({}),
            })
            .unwrap(),
    }
}

#[derive(Default)]
struct CapturingSummary {
    requests: Mutex<Vec<ModelRequest>>,
}

#[async_trait]
impl Model for CapturingSummary {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
    async fn stream(&self, _: ModelRequest, _: CancellationToken) -> ModelResult<ModelStream> {
        unreachable!("摘要只调用 complete")
    }
    async fn complete(
        &self,
        request: ModelRequest,
        _: CancellationToken,
    ) -> ModelResult<ModelResponse> {
        self.requests.lock().unwrap().push(request);
        ModelResponse::new(
            ModelMessage::assistant_text("<summary>REPORT_DECISION</summary>"),
            StopReason::EndTurn,
            None,
            None,
        )
    }
}

/// [回归测试] 手动命令必须摘要完整报告；重复 Full 和关闭旧句柄后加载均不复活全文。
#[tokio::test]
async fn test_compact_report_manual_full_and_cold_reload() {
    let session = BoundSession::open("compact-report.db").await;
    let history = vec![
        BaseMessage::human("review modules"),
        BaseMessage::ai("started"),
    ];
    session.append(&history).await;
    let payload = report(&format!("{}REPORT_TAIL", "报告细节 ".repeat(2000)));
    session
        .resources
        .append_history(&session.thread_id, std::slice::from_ref(&payload))
        .await
        .unwrap();
    let model = Arc::new(CapturingSummary::default());
    let first = execute_compact(
        &CompactCommand,
        make_ctx_with_model_and_thread(
            Arc::new(MockEventSink::new()),
            history,
            session.cwd.clone(),
            model.clone(),
            Some(session.resources()),
            Some(session.thread_id.clone()),
        ),
    )
    .await;
    assert_eq!(first.feedback.as_ref().unwrap().level, FeedbackLevel::Info);
    assert!(first.messages[0].content().contains("REPORT_DECISION"));
    assert!(session.stored_flags().await[&payload.id()].excluded);
    let second = execute_compact(
        &CompactCommand,
        make_ctx_with_model_and_thread(
            Arc::new(MockEventSink::new()),
            session.stored_messages().await,
            session.cwd.clone(),
            model.clone(),
            Some(session.resources()),
            Some(session.thread_id.clone()),
        ),
    )
    .await;
    assert_eq!(second.feedback.as_ref().unwrap().level, FeedbackLevel::Info);
    {
        let requests = model.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(serde_json::to_string(&requests[0])
            .unwrap()
            .contains("REPORT_TAIL"));
        assert!(!serde_json::to_string(&requests[1])
            .unwrap()
            .contains("REPORT_TAIL"));
        assert!(serde_json::to_string(&requests[1])
            .unwrap()
            .contains("REPORT_DECISION"));
    }
    let db_path = session.db_path.clone();
    let thread_id = session.thread_id.clone();
    let _db = session._db;
    drop(session.resources);
    drop(session._lease);
    let reopened =
        peri_resources::sessions::SessionResourcesImpl::open_existing_read_only(&db_path)
            .await
            .unwrap();
    let snapshot = reopened.load_session_snapshot(&thread_id).await.unwrap();
    assert!(
        snapshot.payloads.iter().any(|p| p.id() == payload.id()),
        "原始报告仍存储供回查"
    );
    assert!(snapshot.flags[&payload.id()].excluded);
    let mut restored = MessageTranscript::new().with_own_payloads(snapshot.payloads);
    restored.set_flags_batch(snapshot.flags);
    let visible = restored.visible_model_messages().unwrap();
    assert_eq!(visible.len(), 1);
    assert!(visible[0].content().contains("REPORT_DECISION"));
    assert!(!visible[0].content().contains("REPORT_TAIL"));
}

/// [回归测试] history 普通消息投影为空时，不能跳过仅由报告组成的持久化上下文。
#[tokio::test]
async fn test_compact_report_only_persisted_reminders() {
    let session = BoundSession::open("compact-report-only.db").await;
    let payload = report("ONLY_REPORT");
    session
        .resources
        .append_history(&session.thread_id, std::slice::from_ref(&payload))
        .await
        .unwrap();
    let model = Arc::new(CapturingSummary::default());
    let result = execute_compact(
        &CompactCommand,
        make_ctx_with_model_and_thread(
            Arc::new(MockEventSink::new()),
            vec![],
            session.cwd.clone(),
            model.clone(),
            Some(session.resources()),
            Some(session.thread_id.clone()),
        ),
    )
    .await;
    assert_eq!(result.feedback.as_ref().unwrap().level, FeedbackLevel::Info);
    assert!(result.messages[0].content().contains("REPORT_DECISION"));
    assert!(serde_json::to_string(&model.requests.lock().unwrap()[0])
        .unwrap()
        .contains("ONLY_REPORT"));
    assert!(session.stored_flags().await[&payload.id()].excluded);
}

/// [回归测试] 手动压缩 child 时恢复继承快照，校验完整 history 但仅排除 own 报告。
#[tokio::test]
async fn test_compact_report_manual_child_preserves_parent_flags() {
    let session = BoundSession::open("compact-report-child.db").await;
    let parent_message = BaseMessage::human("parent context");
    let parent_report = report("PARENT_REPORT");
    let inherited_payloads = vec![
        PersistedPayload::Message(parent_message.clone()),
        parent_report.clone(),
    ];
    session
        .resources
        .append_history(&session.thread_id, &inherited_payloads)
        .await
        .unwrap();
    let snapshot = session.snapshot().await;
    let BindingState::Bound(binding) = snapshot.binding else {
        panic!("fixture must be bound")
    };
    let FrozenState::Present(frozen) = snapshot.frozen else {
        panic!("fixture must be frozen")
    };
    let child_id = "compact-report-child".to_owned();
    let child = ChildSnapshot {
        target: NewSession {
            thread_id: child_id.clone(),
            created_at: "2026-09-28T00:00:00Z".to_owned(),
            meta: NewSessionMeta {
                title: None,
                cwd: session.cwd.clone(),
                parent_thread_id: Some(session.thread_id.clone()),
                hidden: true,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding,
            frozen,
        },
        parent_id: session.thread_id.clone(),
        root_id: session.thread_id.clone(),
        inherited: InheritedContext {
            payloads: inherited_payloads,
            flags: HashMap::new(),
        },
    };
    session
        .resources
        .save_child(&child, &session._lease)
        .await
        .unwrap();
    let own_report = report("CHILD_REPORT");
    session
        .resources
        .append_history(&child_id, std::slice::from_ref(&own_report))
        .await
        .unwrap();
    let model = Arc::new(CapturingSummary::default());
    let result = execute_compact(
        &CompactCommand,
        make_ctx_with_model_and_thread(
            Arc::new(MockEventSink::new()),
            vec![parent_message],
            session.cwd.clone(),
            model.clone(),
            Some(session.resources()),
            Some(child_id.clone()),
        ),
    )
    .await;
    assert_eq!(result.feedback.as_ref().unwrap().level, FeedbackLevel::Info);
    let request = serde_json::to_string(&model.requests.lock().unwrap()[0]).unwrap();
    assert!(request.contains("PARENT_REPORT") && request.contains("CHILD_REPORT"));
    assert!(
        session.stored_flags().await.is_empty(),
        "父会话不能被子会话 Full 改写"
    );
    let snapshot = session
        .resources
        .load_session_snapshot(&child_id)
        .await
        .unwrap();
    assert!(snapshot.flags[&own_report.id()].excluded);
    assert!(!snapshot.flags.contains_key(&parent_report.id()));
    assert!(snapshot.inherited.flags.is_empty());
}
