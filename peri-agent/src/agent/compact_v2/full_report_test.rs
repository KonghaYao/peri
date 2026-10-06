//! Full 报告替换：真实请求边界、长历史、失败与继承所有权。
use super::*;
use crate::agent::stages::{compact, reason, CompactInput, ReasonInput, StageContext};
use crate::agent::token::ContextBudget;
use crate::error::AgentError;
use crate::messages::{MessageId, ToolCallRequest};
use crate::session::store::FrozenContext;
use crate::session::test_resources::TestSession;
use crate::session::{MessageKind, MessageQueue, MessageSource, QueuedMessage, Session};
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminder, TrustedSystemReminderFactory,
    SYSTEM_REMINDER_VERSION,
};
use peri_model::{
    Model, ModelCapabilities, ModelResponse, ModelResult, ModelStream, ModelStreamEvent, StopReason,
};
use std::sync::{Arc, Mutex};

fn report(body: impl Into<String>) -> TrustedSystemReminder {
    TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSource("subagent".into()),
            kind: "completed".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: body.into(),
            summary: None,
            metadata: serde_json::json!({}),
        })
        .unwrap()
}

struct ReportModel {
    requests: Mutex<Vec<ModelRequest>>,
    summary: &'static str,
    stop: StopReason,
    incoming: Option<MessageQueue>,
}

impl ReportModel {
    fn new() -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            summary: "<summary>REPORT_DECISION: preserve the final recommendation.</summary>",
            stop: StopReason::EndTurn,
            incoming: None,
        }
    }
}

#[async_trait::async_trait]
impl Model for ReportModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_streaming: true,
            ..Default::default()
        }
    }

    async fn complete(
        &self,
        request: ModelRequest,
        _: CancellationToken,
    ) -> ModelResult<ModelResponse> {
        self.requests.lock().unwrap().push(request);
        if let Some(queue) = &self.incoming {
            queue.push(QueuedMessage::system_reminder(
                MessageKind::Defer,
                MessageSource::SubAgentComplete,
                report("REPORT_ARRIVED_DURING_FULL"),
            ));
        }
        ModelResponse::new(
            ModelMessage::assistant_text(self.summary),
            self.stop.clone(),
            None,
            None,
        )
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        self.requests.lock().unwrap().push(request);
        let response = ModelResponse::new(
            ModelMessage::assistant_text("continue"),
            StopReason::EndTurn,
            None,
            None,
        )?;
        Ok(ModelStream::with_parent_cancellation(
            futures::stream::iter(vec![Ok(ModelStreamEvent::Completed(response))]),
            cancellation,
        ))
    }
}

/// [回归测试] 3000 条历史经自动 Full 后，真实 Reason 请求不再携带 23 份旧报告。
#[tokio::test]
async fn test_full_report_long_session_releases_reports_before_next_reason_request() {
    let bound = TestSession::open().await;
    let mut payloads = vec![PersistedPayload::Message(BaseMessage::system(
        "fixed instruction",
    ))];
    for _ in 0..1488 {
        payloads.push(PersistedPayload::Message(BaseMessage::human(
            "inspect module",
        )));
        payloads.push(PersistedPayload::Message(BaseMessage::ai(
            "inspection complete",
        )));
    }
    let mut report_ids = Vec::new();
    for index in 0..23 {
        let id = MessageId::new();
        report_ids.push(id);
        payloads.push(PersistedPayload::SystemReminder {
            id,
            reminder: report(format!("{}REPORT_TAIL_{index}", "报告细节 ".repeat(2200))),
        });
    }
    assert_eq!(payloads.len(), 3000);
    bound
        .resources
        .append_history(&bound.thread_id, &payloads)
        .await
        .unwrap();
    let session = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some(bound.thread_id.clone()),
    );
    *session.transcript().write() = MessageTranscript::new()
        .with_own_payloads(payloads)
        .with_persistence(bound.resources(), bound.thread_id.clone());
    let model = Arc::new(ReportModel::new());
    let context = StageContext::best_effort_fixture_builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(AgentModelBridge::from_arc(model.clone())))
    .with_compact_llm(model.clone())
    .with_compact_config(CompactConfig::default())
    .with_context_budget(ContextBudget::new(200_000))
    .build();
    // 固定压力仅用于驱动自动触发；不把该值当作本夹具的实测 token 数。
    context
        .compact
        .token_tracker
        .write()
        .accumulate(&peri_model::TokenUsage {
            input_tokens: 196_000,
            output_tokens: 0,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
    assert!(
        compact::run_compact(CompactInput {
            context: context.clone(),
            has_tool_calls: false
        })
        .await
        .unwrap()
        .compacted
    );
    reason::run_reason(ReasonInput {
        context: context.clone(),
        has_tool_calls: false,
    })
    .await
    .unwrap();
    {
        let requests = model.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let summary_request = serde_json::to_string(&requests[0]).unwrap();
        for index in 0..23 {
            assert!(
                summary_request.contains(&format!("REPORT_TAIL_{index}")),
                "摘要必须读到每份报告尾部"
            );
        }
        assert_eq!(
            summary_request.matches("source=\\\"subagent\\\"").count(),
            23
        );
        let next_request = serde_json::to_string(&requests[1]).unwrap();
        assert!(next_request.contains("REPORT_DECISION"));
        assert!(next_request.contains("fixed instruction"));
        assert!(!next_request.contains("REPORT_TAIL_"));
        assert!(!next_request.contains("报告细节"));
        assert!(next_request.len() < 2000, "后续请求不应随历史报告正文增长");
    }
    let stored = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert_eq!(
        stored.payloads.len(),
        3001,
        "旧报告保留在 canonical 存储供回查"
    );
    assert!(report_ids.iter().all(|id| stored.flags[id].excluded));
}

/// [回归测试] 只有报告时仍须摘要；第二次 Full 不复活旧报告，新报告正常进入下一次摘要。
#[tokio::test]
async fn test_full_report_only_history_and_successive_snapshots() {
    let bound = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    let first = transcript.append_system_reminder(report("FIRST_REPORT"));
    let model = ReportModel::new();
    let config = CompactConfig::default();
    let result = full_compact_inner(&mut transcript, Some(&model), &config, "/tmp")
        .await
        .unwrap();
    assert_eq!(result.affected_count, 1);
    assert_eq!(
        (result.before_visible_len, result.after_visible_len),
        (1, 1)
    );
    let later = transcript.append_system_reminder(report("LATER_REPORT"));
    assert!(!transcript.flags(later).excluded);
    assert!(transcript
        .visible_model_messages()
        .unwrap()
        .iter()
        .any(|m| m.id() == later));
    full_compact_inner(&mut transcript, Some(&model), &config, "/tmp")
        .await
        .unwrap();
    let requests = model.requests.lock().unwrap();
    assert!(serde_json::to_string(&requests[0])
        .unwrap()
        .contains("FIRST_REPORT"));
    let second = serde_json::to_string(&requests[1]).unwrap();
    assert!(!second.contains("FIRST_REPORT"));
    assert!(second.contains("LATER_REPORT"));
    assert!(second.contains("REPORT_DECISION"));
    assert!(transcript.flags(first).excluded && transcript.flags(later).excluded);
    assert_eq!(transcript.visible_model_messages().unwrap().len(), 1);
}

/// [回归测试] 摘要须保留完整工具参数、结果和消息角色，不再按字段或前三行预览。
#[tokio::test]
async fn test_full_report_request_preserves_structured_tool_history() {
    let bound = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    transcript.append(BaseMessage::human("request"));
    transcript.append(BaseMessage::ai_with_tool_calls(
        "inspect",
        vec![ToolCallRequest::new(
            "inspect-call",
            "Bash",
            serde_json::json!({"command": format!("{}ARGUMENT_TAIL", "x".repeat(4000))}),
        )],
    ));
    transcript.append(BaseMessage::tool_result(
        "inspect-call",
        "first\nsecond\nthird\nfourth\nRESULT_TAIL",
    ));
    let model = ReportModel::new();
    full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .unwrap();
    let requests = model.requests.lock().unwrap();
    let request = &requests[0];
    assert!(matches!(
        request.messages[2],
        ModelMessage::Assistant { .. }
    ));
    assert!(matches!(
        request.messages[3],
        ModelMessage::ToolResult { .. }
    ));
    let body = serde_json::to_string(request).unwrap();
    assert!(body.contains("ARGUMENT_TAIL") && body.contains("RESULT_TAIL"));
    assert!(request.tools.is_empty(), "摘要请求不得开放工具执行");
}

/// [回归测试] 摘要空白或截断时不排除报告、不提交占位摘要。
#[tokio::test]
async fn test_full_report_invalid_summary_preserves_original_history() {
    for (summary, stop) in [
        ("<analysis>thinking only</analysis>", StopReason::EndTurn),
        ("<summary>partial", StopReason::MaxTokens),
    ] {
        let bound = TestSession::open().await;
        let mut transcript =
            MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
        let id = transcript.append_system_reminder(report("REPORT_MUST_SURVIVE_FAILURE"));
        transcript.flush_persistence().await.unwrap();
        let model = ReportModel {
            summary,
            stop,
            ..ReportModel::new()
        };
        let error = full_compact_inner(
            &mut transcript,
            Some(&model),
            &CompactConfig::default(),
            "/tmp",
        )
        .await
        .unwrap_err();
        match model.stop {
            StopReason::EndTurn => assert!(matches!(error, AgentError::CompactEmptyResponse)),
            _ => assert!(matches!(
                error,
                AgentError::CompactIncompleteResponse {
                    stop_reason: StopReason::MaxTokens
                }
            )),
        }
        assert!(!transcript.flags(id).excluded);
        assert_eq!(transcript.visible_model_messages().unwrap().len(), 1);
        let stored = bound
            .resources
            .load_session_snapshot(&bound.thread_id)
            .await
            .unwrap();
        assert_eq!(stored.payloads.len(), 1);
        assert!(stored.flags.is_empty());
    }
}

/// [回归测试] 只有继承报告的子会话没有可替换历史，不能为祖先内容调用摘要模型。
#[tokio::test]
async fn test_full_report_inherited_only_skips_summary_model() {
    let bound = TestSession::open().await;
    let ancestor = MessageId::new();
    let mut transcript = MessageTranscript::new()
        .with_ancestor_payloads(vec![PersistedPayload::SystemReminder {
            id: ancestor,
            reminder: report("ANCESTOR_REPORT"),
        }])
        .with_persistence(bound.resources(), bound.thread_id.clone());
    let model = ReportModel::new();
    let result = full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .unwrap();
    assert!(model.requests.lock().unwrap().is_empty());
    assert_eq!(result.affected_count, 0);
    assert_eq!(
        result.summary.as_deref(),
        Some("No conversation history to compact.")
    );
    assert!(!transcript.flags(ancestor).excluded);
    let stored = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert!(stored.flags.is_empty());
    assert_eq!(stored.payloads.len(), 1);
    assert!(stored.payloads[0]
        .as_message()
        .unwrap()
        .content()
        .contains("No conversation history to compact."));
}

/// [回归测试] 子会话只排除自己的报告；祖先报告用于摘要但不得改写父会话标记。
#[tokio::test]
async fn test_full_report_inherited_context_preserves_ancestor_ownership() {
    let bound = TestSession::open().await;
    let ancestor = MessageId::new();
    let mut transcript = MessageTranscript::new()
        .with_ancestor_payloads(vec![PersistedPayload::SystemReminder {
            id: ancestor,
            reminder: report("ANCESTOR_REPORT"),
        }])
        .with_persistence(bound.resources(), bound.thread_id.clone());
    let own = transcript.append_system_reminder(report("OWN_REPORT"));
    let model = ReportModel::new();
    full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .unwrap();
    let body = serde_json::to_string(&model.requests.lock().unwrap()[0]).unwrap();
    assert!(body.contains("ANCESTOR_REPORT") && body.contains("OWN_REPORT"));
    assert!(!transcript.flags(ancestor).excluded);
    assert!(transcript.flags(own).excluded);
    let stored = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert!(!stored.flags.contains_key(&ancestor));
    assert!(stored.flags[&own].excluded);
}

/// [回归测试] 摘要模型调用期间到达 inbox 的新报告不在旧快照排除集合内。
#[tokio::test]
async fn test_full_report_arriving_during_summary_stays_available() {
    let bound = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    let old = transcript.append_system_reminder(report("OLD_REPORT"));
    let queue = MessageQueue::new();
    let model = ReportModel {
        incoming: Some(queue.clone()),
        ..ReportModel::new()
    };
    full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .unwrap();
    let request = serde_json::to_string(&model.requests.lock().unwrap()[0]).unwrap();
    assert!(request.contains("OLD_REPORT"));
    assert!(!request.contains("REPORT_ARRIVED_DURING_FULL"));
    crate::agent::stages::append_messages_to_transcript(&mut transcript, queue.drain_all());
    transcript.flush_persistence().await.unwrap();
    assert!(transcript.flags(old).excluded);
    let visible = transcript.visible_model_messages().unwrap();
    assert_eq!(visible.len(), 2);
    assert!(visible[0].content().contains("REPORT_DECISION"));
    assert!(visible[1].content().contains("REPORT_ARRIVED_DURING_FULL"));
    let stored = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert!(!stored
        .flags
        .get(&visible[1].id())
        .is_some_and(|flags| flags.excluded));
}

/// [回归测试] Full 必须沿用已提交的 Micro 视图，不能重新展开被压缩的工具输出。
#[tokio::test]
async fn test_full_report_uses_committed_micro_view() {
    let bound = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    for index in 0..8 {
        let call_id = format!("micro-{index}");
        transcript.append(BaseMessage::human("inspect"));
        transcript.append(BaseMessage::ai_with_tool_calls(
            "read",
            vec![ToolCallRequest::new(
                &call_id,
                "Bash",
                serde_json::json!({"command": "inspect"}),
            )],
        ));
        transcript.append(BaseMessage::tool_result(&call_id, "x".repeat(10_000)));
    }
    transcript.append_system_reminder(report("UNTRUNCATED_REPORT"));
    let config = CompactConfig::default();
    assert!(crate::agent::compact_v2::micro_compact(&mut transcript, &config) > 0);
    let view = super::super::projection::render_persisted_llm_view(
        &transcript,
        &super::super::projection::ProviderCapabilities::default(),
    )
    .unwrap();
    let expected = AgentModelBridge::convert_messages(&view).unwrap();
    let model = ReportModel::new();
    full_compact_inner(&mut transcript, Some(&model), &config, "/tmp")
        .await
        .unwrap();
    let requests = model.requests.lock().unwrap();
    let messages = &requests[0].messages;
    assert_eq!(
        serde_json::to_value(&messages[1..messages.len() - 1]).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    assert!(serde_json::to_string(&messages)
        .unwrap()
        .contains("UNTRUNCATED_REPORT"));
}
