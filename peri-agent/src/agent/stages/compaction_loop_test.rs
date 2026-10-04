//! RCRA 中的 Compact 压力代际、持久化 Full 与模型可见视图。
use super::super::*;
use crate::session::queue::MessageSource;
use crate::session::store::FrozenContext;
use crate::session::test_resources::mock::MockSessionResources;
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
async fn test_run_react_loop_successful_full_replaces_history_without_file_re_read_and_resets_usage(
) {
    use crate::thread::ThreadMeta;

    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("full-read-marker.txt");
    let file_marker = "successful full historical file marker";
    std::fs::write(&file_path, file_marker).unwrap();

    let store = MockSessionResources::new();
    let thread_id = store
        .create_thread(ThreadMeta::new(dir.path().to_string_lossy()))
        .await
        .unwrap();
    let store_dyn: Arc<dyn peri_acp_types::session_resources::SessionResources> = store.clone();
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
        assert!(!post_full
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
    let visible_file_count = persisted
        .iter()
        .filter(|message| {
            message.content().contains(file_marker)
                && !flags.get(&message.id()).is_some_and(|flag| flag.excluded)
        })
        .count();
    assert_eq!(summary_count, 1);
    assert_eq!(visible_file_count, 0, "Full 不应重新注入本机文件内容");
    assert!(persisted
        .iter()
        .any(|message| message.content() == file_marker));
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
/// 会重新 arm；绑定真实持久化后验证 Full → Micro → Full，不依赖吞掉 Full 提交失败。
#[tokio::test]
async fn test_run_react_loop_new_high_usage_generations_continue_full_micro_churn() {
    let reason_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let reason_requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let compact_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let bound = crate::session::test_resources::TestSession::open().await;
    let session = Session::new(
        Arc::from("/tmp/usage-churn-characterization"),
        FrozenContext::builder().build(),
        Some(bound.thread_id.clone()),
    );
    {
        let transcript = session.transcript();
        let mut guard = transcript.write();
        *guard = std::mem::take(&mut *guard)
            .with_persistence(bound.resources(), bound.thread_id.clone());
    }
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

    let result = run_react_loop(context.clone(), 4).await;

    assert!(matches!(result, LoopResult::Completed), "{result:?}");
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
                crate::agent::events::CompactStrategy::Full,
                96_082,
                Some(crate::agent::compact_v2::planner::FullEscalationReason::InsufficientReclaim),
                crate::agent::compact_v2::CompactOutcome::FullApplied,
            ),
            (
                crate::agent::events::CompactStrategy::Micro,
                80_081,
                None,
                crate::agent::compact_v2::CompactOutcome::MicroApplied,
            ),
            (
                crate::agent::events::CompactStrategy::Full,
                97_082,
                Some(crate::agent::compact_v2::planner::FullEscalationReason::InsufficientReclaim),
                crate::agent::compact_v2::CompactOutcome::FullApplied,
            ),
        ],
        "每个新的非零高位 usage generation 都会再次触发；Full 区间会升级尝试，Micro 区间只执行 Micro"
    );
    assert_eq!(
        compact_calls.load(Ordering::SeqCst),
        2,
        "两次 Full 各调用一次 compact LLM；中间 Micro 不调用"
    );
    let transcript = std::mem::take(&mut *context.session.transcript.write());
    transcript.flush_persistence().await.unwrap();
    let stored = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert_eq!(
        stored
            .payloads
            .iter()
            .filter(|payload| payload
                .as_message()
                .is_some_and(|message| message.content().contains("compact generation")))
            .count(),
        2,
        "两次 Full 摘要必须真实提交"
    );
    assert!(
        stored
            .payloads
            .iter()
            .filter_map(|payload| payload.as_message())
            .filter(|message| message.content().contains("characterize usage churn"))
            .all(|message| stored.flags[&message.id()].excluded),
        "旧任务原文保留并由成功摘要替代"
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

/// [回归测试] SQLite Full 后新高 usage 可再次 Full，但不能对 excluded 历史发 Micro。
/// usage 数列为受控输入，只证明新高样本下的执行链，不代表现场 token 测量。
#[tokio::test]
async fn test_run_react_loop_successful_full_does_not_recompact_excluded_history() {
    use crate::agent::compact_v2::{planner::plan_micro, projection, CompactOutcome};
    use crate::thread::ThreadMeta;
    let store = MockSessionResources::new();
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
        vec![CompactOutcome::FullApplied, CompactOutcome::FullApplied,]
    );
    {
        let transcript = context.session.transcript.read();
        let plan = plan_micro(&transcript, &config, false);
        assert!(
            plan.actions.is_empty(),
            "Full 后只有短结果可见，不应再次规划旧工具输出"
        );
        let canonical = transcript.visible_model_messages().unwrap();
        let projected =
            projection::render_llm_view(&transcript, &plan, &Default::default()).unwrap();
        assert_eq!(
            serde_json::to_value(canonical).unwrap(),
            serde_json::to_value(projected).unwrap(),
            "无 Micro action 时维持 canonical 模型视图"
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
        0
    );
}

/// [回归测试] 工具结果已进入 transcript 后，下一轮 Compact 必须看见其新增压力。
#[tokio::test]
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
    let first_tool_outputs = first
        .messages_snapshot
        .iter()
        .filter(|message| matches!(message, BaseMessage::Tool { .. }))
        .map(|message| message.content().chars().count())
        .collect::<Vec<_>>();
    assert_eq!(
        first_tool_outputs,
        vec![8_000; 4],
        "没有已提交 directive 时 Reason 必须发送 canonical 工具结果"
    );
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
