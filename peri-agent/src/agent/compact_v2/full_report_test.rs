//! Full 报告替换：真实请求边界、长历史、失败与继承所有权。
use crate::agent::compact_v2::config::CompactConfig;

use crate::agent::model_bridge::AgentModelBridge;
use crate::agent::stages::{compact, reason, CompactInput, ReasonInput, StageContext};
use crate::agent::token::ContextBudget;
use crate::error::AgentError;
use crate::messages::BaseMessage;
use crate::messages::{MessageId, ToolCallRequest};
use crate::session::store::FrozenContext;
use crate::session::test_resources::TestSession;
use crate::session::transcript::MessageTranscript;
use crate::session::{MessageKind, MessageQueue, MessageSource, QueuedMessage, Session};
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminder, TrustedSystemReminderFactory,
    SYSTEM_REMINDER_VERSION,
};
use peri_model::ModelMessage;
use peri_model::ModelRequest;
use peri_model::{
    Model, ModelCapabilities, ModelResponse, ModelResult, ModelStream, ModelStreamEvent, StopReason,
};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

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
        .expect("the construct fixture result is required for this assertion")
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
        self.requests
            .lock()
            .expect("self requests mutex must remain unpoisoned throughout the test")
            .push(request);
        if let Some(queue) = &self.incoming {
            queue.push_sync(QueuedMessage::system_reminder(
                MessageKind::Defer,
                MessageSource::SubAgentComplete,
                report("REPORT_ARRIVED_DURING_FULL"),
            ));
        }
        ModelResponse::try_new(
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
        self.requests
            .lock()
            .expect("self requests mutex must remain unpoisoned throughout the test")
            .push(request);
        let response = ModelResponse::try_new(
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
        .expect("the append history fixture result is required for this assertion");
    let session = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some(bound.thread_id.clone()),
    );
    *session.transcript().write() = MessageTranscript::new()
        .with_own_payloads(payloads)
        .with_persistence(bound.resources(), bound.thread_id.clone());
    let model = Arc::new(ReportModel::new());
    let context = StageContext::builder(
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
        .expect("the clone fixture result is required for this assertion")
        .compacted
    );
    reason::run_reason(ReasonInput {
        context: context.clone(),
        has_tool_calls: false,
    })
    .await
    .expect("the run reason fixture result is required for this assertion");
    {
        let requests = model
            .requests
            .lock()
            .expect("model requests mutex must remain unpoisoned throughout the test");
        assert_eq!(requests.len(), 2);
        let summary_request = serde_json::to_string(&requests[0])
            .expect("to_string must accept this test fixture representation");
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
        let next_request = serde_json::to_string(&requests[1])
            .expect("to_string must accept this test fixture representation");
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
        .expect("the load session snapshot fixture result is required for this assertion");
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
    let result = crate::agent::compact_v2::full::full_compact_inner(
        &mut transcript,
        Some(&model),
        &config,
        "/tmp",
    )
    .await
    .expect("the full compact inner fixture result is required for this assertion");
    assert_eq!(result.affected_count, 1);
    assert_eq!(
        (result.before_visible_len, result.after_visible_len),
        (1, 1)
    );
    let later = transcript.append_system_reminder(report("LATER_REPORT"));
    assert!(!transcript.flags(later).excluded);
    assert!(transcript
        .visible_model_messages()
        .expect("the visible model messages fixture result is required for this assertion")
        .iter()
        .any(|m| m.id() == later));
    let _ = crate::agent::compact_v2::full::full_compact_inner(
        &mut transcript,
        Some(&model),
        &config,
        "/tmp",
    )
    .await
    .expect("the full compact inner fixture result is required for this assertion");
    let requests = model
        .requests
        .lock()
        .expect("model requests mutex must remain unpoisoned throughout the test");
    assert!(serde_json::to_string(&requests[0])
        .expect("to_string must accept this test fixture representation")
        .contains("FIRST_REPORT"));
    let second = serde_json::to_string(&requests[1])
        .expect("to_string must accept this test fixture representation");
    assert!(!second.contains("FIRST_REPORT"));
    assert!(second.contains("LATER_REPORT"));
    assert!(second.contains("REPORT_DECISION"));
    assert!(
        transcript.get(first).is_none() && transcript.get(later).is_none(),
        "Full 提交后已排除报告必须从热态释放（持久化事实仍带 excluded）"
    );
    assert_eq!(
        transcript
            .visible_model_messages()
            .expect("the visible model messages fixture result is required for this assertion")
            .len(),
        1
    );
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
    let _ = crate::agent::compact_v2::full::full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .expect("the full compact inner fixture result is required for this assertion");
    let requests = model
        .requests
        .lock()
        .expect("model requests mutex must remain unpoisoned throughout the test");
    let request = &requests[0];
    assert!(matches!(
        request.messages[2],
        ModelMessage::Assistant { .. }
    ));
    assert!(matches!(
        request.messages[3],
        ModelMessage::ToolResult { .. }
    ));
    let body = serde_json::to_string(request)
        .expect("to_string must accept this test fixture representation");
    assert!(body.contains("ARGUMENT_TAIL") && body.contains("RESULT_TAIL"));
    assert!(request.tools.is_empty(), "摘要请求不得开放工具执行");
}

/// [回归测试] 摘要只有隐藏思考时不排除报告、不提交占位摘要。
#[tokio::test]
async fn test_full_report_invalid_summary_preserves_original_history() {
    for (summary, stop) in [
        ("<analysis>thinking only</analysis>", StopReason::EndTurn),
        ("<analysis>thinking only</analysis>", StopReason::MaxTokens),
    ] {
        let bound = TestSession::open().await;
        let mut transcript =
            MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
        let id = transcript.append_system_reminder(report("REPORT_MUST_SURVIVE_FAILURE"));
        transcript
            .flush_persistence()
            .await
            .expect("the flush persistence fixture result is required for this assertion");
        let model = ReportModel {
            summary,
            stop,
            ..ReportModel::new()
        };
        let error = crate::agent::compact_v2::full::full_compact_inner(
            &mut transcript,
            Some(&model),
            &CompactConfig::default(),
            "/tmp",
        )
        .await
        .unwrap_err();
        // 首轮无正文 → 重新生成一次 → 仍无正文 → 恢复耗尽；两种 stop 都不算成功。
        assert!(
            matches!(
                error,
                AgentError::CompactRecoveryExhausted {
                    attempts: 2,
                    last_reason: peri_acp_types::error::SummaryRecoveryReason::EmptyResponse,
                }
            ),
            "{error:?}"
        );
        assert_eq!(
            model
                .requests
                .lock()
                .expect("model requests mutex must remain unpoisoned throughout the test")
                .len(),
            2
        );
        assert!(!transcript.flags(id).excluded);
        assert_eq!(
            transcript
                .visible_model_messages()
                .expect("the visible model messages fixture result is required for this assertion")
                .len(),
            1
        );
        let stored = bound
            .resources
            .load_session_snapshot(&bound.thread_id)
            .await
            .expect("the load session snapshot fixture result is required for this assertion");
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
    let result = crate::agent::compact_v2::full::full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .expect("the full compact inner fixture result is required for this assertion");
    assert!(model
        .requests
        .lock()
        .expect("model requests mutex must remain unpoisoned throughout the test")
        .is_empty());
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
        .expect("the load session snapshot fixture result is required for this assertion");
    assert!(stored.flags.is_empty());
    assert_eq!(stored.payloads.len(), 1);
    assert!(stored.payloads[0]
        .as_message()
        .expect("the as message fixture result is required for this assertion")
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
    let _ = crate::agent::compact_v2::full::full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .expect("the full compact inner fixture result is required for this assertion");
    let body = serde_json::to_string(
        &model
            .requests
            .lock()
            .expect("model requests mutex must remain unpoisoned throughout the test")[0],
    )
    .expect("to_string must accept this test fixture representation");
    assert!(body.contains("ANCESTOR_REPORT") && body.contains("OWN_REPORT"));
    assert!(!transcript.flags(ancestor).excluded);
    assert!(
        transcript.get(own).is_none(),
        "own 报告必须从热态释放，ancestor 保留"
    );
    let stored = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .expect("the load session snapshot fixture result is required for this assertion");
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
    let _ = crate::agent::compact_v2::full::full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .expect("the full compact inner fixture result is required for this assertion");
    let request = serde_json::to_string(
        &model
            .requests
            .lock()
            .expect("model requests mutex must remain unpoisoned throughout the test")[0],
    )
    .expect("to_string must accept this test fixture representation");
    assert!(request.contains("OLD_REPORT"));
    assert!(!request.contains("REPORT_ARRIVED_DURING_FULL"));
    let arrived = queue.drain_all_sync().pop().expect("报告必须留在 MQ");
    assert!(queue.is_empty_sync());
    // 到达只落在 MQ，交付仍走 canonical 追加；旧快照的排除集合不得覆盖它。
    let peri_acp_types::session::QueuedPayload::SystemReminder(reminder) = arrived.payload else {
        panic!("expected a system reminder payload");
    };
    transcript.append_system_reminder(reminder);
    transcript
        .flush_persistence()
        .await
        .expect("the flush persistence fixture result is required for this assertion");
    assert!(
        transcript.get(old).is_none(),
        "Full 已排除的旧报告必须从热态释放"
    );
    let visible = transcript
        .visible_model_messages()
        .expect("the visible model messages fixture result is required for this assertion");
    assert_eq!(visible.len(), 2);
    assert!(visible[0].content().contains("REPORT_DECISION"));
    assert!(visible[1].content().contains("REPORT_ARRIVED_DURING_FULL"));
    let stored = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .expect("the load session snapshot fixture result is required for this assertion");
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
    use crate::agent::compact_v2::planner;
    use crate::agent::compact_v2::planner::ArchivePlanningScope;
    use crate::agent::compact_v2::planner::ContextPressure;
    use crate::agent::compact_v2::planner::MicroPlanningFacts;
    let mut facts = MicroPlanningFacts {
        archive_scope: Some(ArchivePlanningScope {
            thread_id: bound.thread_id.clone(),
            committed_tail: transcript
                .entries()
                .last()
                .expect("transcript entries fixture must contain the expected element")
                .id(),
        }),
        ..Default::default()
    };
    for entry in transcript.entries() {
        facts.exposed_message_ids.insert(entry.id());
        if let Some(message) = entry.as_message() {
            for call in message.tool_calls() {
                facts.closed_call_ids.insert(call.id.clone());
                facts.tool_retention.insert(
                    call.name.to_lowercase(),
                    crate::tools::ContextRetention::PreviewOnly,
                );
            }
        }
    }
    let plan = planner::plan_micro_with_facts(
        &transcript,
        &config,
        &ContextPressure {
            estimated_tokens: 150_000,
            context_window: 200_000,
            output_reserve: 8000,
            predicted_tool_growth: 0,
            safety_buffer: 0,
            cache_hit_rate: 0.0,
        },
        &facts,
        &crate::agent::compact_v2::projection::ProviderCapabilities::default(),
    )
    .expect("the plan micro with facts fixture result is required for this assertion");
    assert!(
        crate::agent::compact_v2::micro::apply_micro_plan(&mut transcript, &plan)
            .await
            .expect("the apply micro plan fixture result is required for this assertion")
            > 0
    );
    let view = crate::agent::compact_v2::projection::render_persisted_llm_view(
        &transcript,
        &crate::agent::compact_v2::projection::ProviderCapabilities::default(),
    )
    .expect("the render persisted llm view fixture result is required for this assertion");
    let expected = AgentModelBridge::convert_messages(&view)
        .expect("the convert messages fixture result is required for this assertion");
    let model = ReportModel::new();
    let _ = crate::agent::compact_v2::full::full_compact_inner(
        &mut transcript,
        Some(&model),
        &config,
        "/tmp",
    )
    .await
    .expect("the full compact inner fixture result is required for this assertion");
    let requests = model
        .requests
        .lock()
        .expect("model requests mutex must remain unpoisoned throughout the test");
    let messages = &requests[0].messages;
    assert_eq!(
        serde_json::to_value(&messages[1..messages.len() - 1])
            .expect("the len fixture result is required for this assertion"),
        serde_json::to_value(expected)
            .expect("to_value must accept this test fixture representation")
    );
    assert!(serde_json::to_string(&messages)
        .expect("to_string must accept this test fixture representation")
        .contains("UNTRUNCATED_REPORT"));
}
