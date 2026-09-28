//! Compact 会话边界对抗测试：公开 API、临时 SQLite、确定性模型。
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use async_trait::async_trait;
use parking_lot::Mutex;
use peri_acp_types::{
    command::{CommandContext, DependencyBag, FeedbackLevel, PromptStopReason},
    event::{EventSink, ExecutorEvent},
    session_resources::{FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources},
    store::PersistedPayload,
    thread::CancelPolicy,
    workspace::{SessionBinding, SessionExecutionLease},
};
use peri_agent::{
    agent::{
        compact_v2::CompactConfig,
        react::{ReactLLM, Reasoning, StreamingContext},
        stages::{run_react_loop, LoopResult, StageContext},
        token::ContextBudget,
    },
    error::{AgentError, AgentResult},
    messages::BaseMessage,
    session::{
        exec::compact_pipeline::execute_compact, FrozenContext, MessageSource, MessageTranscript,
        QueuedMessage, Session,
    },
    tools::BaseTool,
};
use peri_model::{
    Model, ModelCapabilities, ModelMessage, ModelRequest, ModelResponse, ModelResult, ModelStream,
    StopReason, TokenUsage,
};
use tokio_util::sync::CancellationToken;

struct BoundSession {
    resources: Arc<dyn SessionResources>,
    thread_id: String,
    cwd: String,
    db_path: std::path::PathBuf,
    _lease: Arc<dyn SessionExecutionLease>,
    _directory: tempfile::TempDir,
}

impl BoundSession {
    async fn open() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let db_path = directory.path().join("compact-audit.db");
        let resources: Arc<dyn SessionResources> = Arc::new(
            peri_resources::sessions::SessionResourcesImpl::open(&db_path)
                .await
                .unwrap(),
        );
        let workspace = resources.resolve_workspace(directory.path()).await.unwrap();
        let thread_id = uuid::Uuid::now_v7().to_string();
        let cwd = workspace.cwd.to_string_lossy().into_owned();
        let lease = resources
            .create_session(&NewSession {
                thread_id: thread_id.clone(),
                created_at: "2026-09-28T00:00:00Z".into(),
                meta: NewSessionMeta {
                    title: Some("compact audit".into()),
                    cwd: cwd.clone(),
                    parent_thread_id: None,
                    hidden: false,
                    cancel_policy: CancelPolicy::default(),
                    snapshot_at_message_id: None,
                },
                binding: SessionBinding::from_workspace(&workspace),
                frozen: FrozenSnapshotBytes::new("{\"version\":1,\"test\":true}"),
            })
            .await
            .unwrap();
        Self {
            resources,
            thread_id,
            cwd,
            db_path,
            _lease: lease,
            _directory: directory,
        }
    }

    fn session(&self, payloads: Vec<PersistedPayload>) -> Arc<Session> {
        let session = Session::new(
            Arc::from(self.cwd.as_str()),
            FrozenContext::builder().build(),
            Some(self.thread_id.clone()),
        );
        *session.transcript().write() = MessageTranscript::new()
            .with_own_payloads(payloads)
            .with_persistence(self.resources.clone(), self.thread_id.clone());
        session
    }
}

struct PrimaryModel {
    accept_first: bool,
    calls: AtomicUsize,
    order: Arc<Mutex<Vec<&'static str>>>,
    requests: Mutex<Vec<Vec<BaseMessage>>>,
}

#[async_trait]
impl ReactLLM for PrimaryModel {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _: &[&dyn BaseTool],
        _: Option<StreamingContext>,
    ) -> AgentResult<Reasoning> {
        self.order.lock().push("reason");
        self.requests.lock().push(messages.to_vec());
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let chars: usize = messages
            .iter()
            .map(|message| message.content().chars().count())
            .sum();
        if chars > 380_000 && !(self.accept_first && call == 0) {
            return Err(AgentError::LlmHttpError {
                status: 400,
                message: "fixture: restored request exceeded the context limit".into(),
            });
        }
        let mut response = Reasoning::with_answer("", "done");
        response.usage = Some(TokenUsage {
            input_tokens: if chars > 350_000 { 90_000 } else { 1_000 },
            output_tokens: 1,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
        Ok(response)
    }
}

struct SummaryModel {
    failures: usize,
    calls: AtomicUsize,
    order: Arc<Mutex<Vec<&'static str>>>,
    cancel: Option<CancellationToken>,
}

#[async_trait]
impl Model for SummaryModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
    async fn stream(&self, _: ModelRequest, _: CancellationToken) -> ModelResult<ModelStream> {
        unreachable!("摘要必须走 complete")
    }
    async fn complete(&self, _: ModelRequest, _: CancellationToken) -> ModelResult<ModelResponse> {
        self.order.lock().push("summary");
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
            std::future::pending::<()>().await;
        }
        ModelResponse::new(
            ModelMessage::assistant_text(if call < self.failures {
                "<analysis>no summary</analysis>"
            } else {
                "<summary>RECOVERED: continue the work.</summary>"
            }),
            StopReason::EndTurn,
            None,
            None,
        )
    }
}

fn make_models(accept_first: bool) -> (Arc<PrimaryModel>, Arc<SummaryModel>) {
    let order = Arc::new(Mutex::new(Vec::new()));
    (
        Arc::new(PrimaryModel {
            accept_first,
            calls: AtomicUsize::new(0),
            order: order.clone(),
            requests: Default::default(),
        }),
        Arc::new(SummaryModel {
            failures: 0,
            calls: AtomicUsize::new(0),
            order,
            cancel: None,
        }),
    )
}

fn make_context(
    session: &Session,
    model: Arc<PrimaryModel>,
    summary: Arc<SummaryModel>,
) -> StageContext {
    StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(model)
    .with_compact_llm(summary)
    .with_context_budget(ContextBudget::new(100_000))
    .with_compact_config(CompactConfig::default())
    .build()
}

/// [回归测试] 同会话下一 prompt 不得遗失上一轮 90% 的压力而直接发送超限请求。
#[tokio::test]
async fn test_compact_session_next_prompt_restores_pressure_before_reason() {
    let bound = BoundSession::open().await;
    let session = bound.session(Vec::new());
    let (model, summary) = make_models(true);
    let first = make_context(&session, model.clone(), summary.clone());
    session.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("x".repeat(360_000)),
    ));
    assert!(matches!(
        run_react_loop(first.clone(), 4).await,
        LoopResult::Completed
    ));
    assert_eq!(
        first
            .compact
            .token_tracker
            .read()
            .estimated_context_tokens(),
        Some(90_000)
    );
    let second = make_context(&session, model.clone(), summary.clone());
    session.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("y".repeat(60_000)),
    ));
    let result = run_react_loop(second, 4).await;
    assert!(
        matches!(result, LoopResult::Completed),
        "下一 prompt 必须先压缩；实际 {result:?}，顺序 {:?}，摘要调用 {}",
        *model.order.lock(),
        summary.calls.load(Ordering::SeqCst)
    );
    assert_eq!(*model.order.lock(), ["reason", "summary", "reason"]);
}

/// [回归测试] SQLite 新句柄重建的大历史必须在首次 Reason 前建立压力基线。
#[tokio::test]
async fn test_compact_session_cold_snapshot_compacts_before_first_reason() {
    let bound = BoundSession::open().await;
    let payloads = vec![PersistedPayload::Message(BaseMessage::human(
        "x".repeat(440_000),
    ))];
    bound
        .resources
        .append_history(&bound.thread_id, &payloads)
        .await
        .unwrap();
    let reader =
        peri_resources::sessions::SessionResourcesImpl::open_existing_read_only(&bound.db_path)
            .await
            .unwrap();
    let snapshot = reader
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert_eq!(snapshot.payloads.len(), 1);
    let session = bound.session(snapshot.payloads);
    let (model, summary) = make_models(false);
    let context = make_context(&session, model.clone(), summary.clone());
    session.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("continue"),
    ));
    let result = run_react_loop(context, 4).await;
    assert!(
        matches!(result, LoopResult::Completed),
        "冷读历史必须先压缩；实际 {result:?}，顺序 {:?}，摘要调用 {}",
        *model.order.lock(),
        summary.calls.load(Ordering::SeqCst)
    );
    assert_eq!(*model.order.lock(), ["summary", "reason"]);
}

#[derive(Default)]
struct RecordingSink(Mutex<Vec<ExecutorEvent>>);

#[async_trait]
impl EventSink for RecordingSink {
    async fn push_event(&self, _: &str, event: &ExecutorEvent, _: u32) {
        self.0.lock().push(event.clone());
    }
    async fn push_done(&self, _: &str, _: &str, _: Option<&str>) {}
}

async fn manual_case(
    failures: usize,
    cancel_during_summary: bool,
) -> (
    BoundSession,
    peri_acp_types::command::CommandResult,
    Arc<SummaryModel>,
    Arc<RecordingSink>,
    Vec<BaseMessage>,
) {
    let token = CancellationToken::new();
    let model = Arc::new(SummaryModel {
        failures,
        calls: AtomicUsize::new(0),
        order: Default::default(),
        cancel: cancel_during_summary.then(|| token.clone()),
    });
    let (bound, result, sink, history) = manual_with_model(model.clone(), token).await;
    (bound, result, model, sink, history)
}

async fn manual_with_model(
    model: Arc<dyn Model>,
    token: CancellationToken,
) -> (
    BoundSession,
    peri_acp_types::command::CommandResult,
    Arc<RecordingSink>,
    Vec<BaseMessage>,
) {
    let bound = BoundSession::open().await;
    let history = vec![
        BaseMessage::human("retain original task"),
        BaseMessage::ai("retain original answer"),
    ];
    let sink = Arc::new(RecordingSink::default());
    let mut command = CommandContext::new(
        bound.thread_id.clone(),
        history.clone(),
        bound.cwd.clone(),
        sink.clone(),
        token,
        DependencyBag::new(),
    );
    command.auxiliary_model = Some(model.clone());
    command.session_resources = Some(bound.resources.clone());
    command.thread_id = Some(bound.thread_id.clone());
    let result = execute_compact(command).await;
    (bound, result, sink, history)
}

#[tokio::test]
async fn test_compact_session_manual_empty_summary_retries_without_next_prompt() {
    let (bound, result, model, sink, history) = manual_case(2, false).await;
    assert_eq!(model.calls.load(Ordering::SeqCst), 3);
    assert!(matches!(result.stop_reason, PromptStopReason::EndTurn));
    assert!(matches!(
        result.feedback.unwrap().level,
        FeedbackLevel::Info
    ));
    assert!(result.messages[0].content().contains("RECOVERED"));
    let snapshot = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    for original in history {
        assert!(snapshot.flags[&original.id()].excluded);
    }
    let events = sink.0.lock();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ExecutorEvent::CompactStarted { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ExecutorEvent::CompactCompleted { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn test_compact_session_manual_failure_preserves_history_and_reports_feedback() {
    let (bound, result, model, sink, history) = manual_case(usize::MAX, false).await;
    assert_eq!(model.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        serde_json::to_value(&result.messages).unwrap(),
        serde_json::to_value(&history).unwrap()
    );
    let feedback = result.feedback.unwrap();
    assert!(matches!(feedback.level, FeedbackLevel::Error));
    assert_eq!(
        feedback.message,
        "Full Compact failed after 3 attempts. Retry or change the compact model."
    );
    let snapshot = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert!(snapshot.flags.values().all(|flag| !flag.excluded));
    assert_eq!(snapshot.payloads.len(), history.len());
    assert_eq!(
        sink.0.lock().len(),
        1,
        "当前失败仅发送 Started，反馈由命令编排投影"
    );
}

#[tokio::test]
async fn test_compact_session_manual_cancel_preserves_history() {
    let (bound, result, model, sink, history) = manual_case(0, true).await;
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    assert!(matches!(result.stop_reason, PromptStopReason::Cancelled));
    assert_eq!(
        serde_json::to_value(&result.messages).unwrap(),
        serde_json::to_value(&history).unwrap()
    );
    let feedback = result.feedback.unwrap();
    assert!(matches!(feedback.level, FeedbackLevel::Warning));
    assert_eq!(feedback.message, "compact cancelled");
    let snapshot = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert!(snapshot.flags.values().all(|flag| !flag.excluded));
    assert_eq!(snapshot.payloads.len(), history.len());
    assert_eq!(
        sink.0.lock().len(),
        1,
        "当前取消仅发送 Started，上游用 TurnDone 结束 loading"
    );
}

struct RejectedSummary {
    protocol: bool,
    cancel: Option<CancellationToken>,
}

#[async_trait]
impl Model for RejectedSummary {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
    async fn stream(&self, _: ModelRequest, _: CancellationToken) -> ModelResult<ModelStream> {
        unreachable!("摘要必须走 complete")
    }
    async fn complete(&self, _: ModelRequest, _: CancellationToken) -> ModelResult<ModelResponse> {
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
        Err(if self.protocol {
            peri_model::ModelError::protocol_with_summary(
                peri_model::ProtocolErrorKind::InvalidJsonObject,
                "fixture-private-body: sk-not-a-real-key https://private.example/path",
            )
        } else {
            peri_model::ModelError::http_status(401, "fixture", Some("req-compact-401"))
        })
    }
}

/// [回归测试] 手动 compact 必须保留安全的 HTTP 诊断，并保全原历史。
#[tokio::test]
async fn test_compact_session_manual_http_failure_reports_safe_cause() {
    let (bound, result, _, history) = manual_with_model(
        Arc::new(RejectedSummary {
            protocol: false,
            cancel: None,
        }),
        CancellationToken::new(),
    )
    .await;
    let feedback = result.feedback.unwrap();
    assert!(matches!(feedback.level, FeedbackLevel::Error));
    assert_eq!(
        feedback.message,
        "An LLM API error occurred (HTTP 401, request id: req-compact-401). Please try again."
    );
    assert_eq!(
        serde_json::to_value(result.messages).unwrap(),
        serde_json::to_value(history).unwrap()
    );
    let snapshot = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert!(snapshot.flags.values().all(|flag| !flag.excluded));
}

/// [回归测试] 协议诊断只显示 allowlist 分类，不透出 provider 原文。
#[tokio::test]
async fn test_compact_session_manual_protocol_failure_redacts_provider_body() {
    let (_, result, _, _) = manual_with_model(
        Arc::new(RejectedSummary {
            protocol: true,
            cancel: None,
        }),
        CancellationToken::new(),
    )
    .await;
    let feedback = result.feedback.unwrap();
    assert!(matches!(feedback.level, FeedbackLevel::Error));
    assert_eq!(
        feedback.message,
        "An LLM API error occurred (protocol failure: invalid JSON object). Please try again."
    );
}

/// [回归测试] provider 同 poll 返回错误并触发取消时，终态仍必须为取消。
#[tokio::test]
async fn test_compact_session_manual_cancel_wins_over_ready_provider_error() {
    let token = CancellationToken::new();
    let (_, result, _, history) = manual_with_model(
        Arc::new(RejectedSummary {
            protocol: false,
            cancel: Some(token.clone()),
        }),
        token,
    )
    .await;
    assert!(matches!(result.stop_reason, PromptStopReason::Cancelled));
    assert_eq!(result.feedback.unwrap().message, "compact cancelled");
    assert_eq!(
        serde_json::to_value(result.messages).unwrap(),
        serde_json::to_value(history).unwrap()
    );
}

/// [负对照] 轻上下文跨 prompt 不应新增 Full 或摘要请求。
#[tokio::test]
async fn test_compact_session_small_prompts_do_not_call_summary() {
    let bound = BoundSession::open().await;
    let session = bound.session(Vec::new());
    let (model, summary) = make_models(false);
    for text in ["first small task", "second small task"] {
        let context = make_context(&session, model.clone(), summary.clone());
        session.queue().push(QueuedMessage::prompt(
            MessageSource::UserInput,
            BaseMessage::human(text),
        ));
        let result = run_react_loop(context, 4).await;
        assert!(matches!(result, LoopResult::Completed), "{result:?}");
    }
    assert_eq!(*model.order.lock(), ["reason", "reason"]);
    assert_eq!(summary.calls.load(Ordering::SeqCst), 0);
}

/// [负对照] 冷读已提交 Full 的 canonical 原文仍存在，但不能复活进请求或压力估算。
#[tokio::test]
async fn test_compact_session_cold_compacted_history_keeps_exclusions() {
    let bound = BoundSession::open().await;
    let original = BaseMessage::human(format!("ORIGINAL_HIDDEN_SENTINEL{}", "x".repeat(440_000)));
    let sink = Arc::new(RecordingSink::default());
    let (_, summary) = make_models(false);
    let mut command = CommandContext::new(
        bound.thread_id.clone(),
        vec![original.clone()],
        bound.cwd.clone(),
        sink,
        CancellationToken::new(),
        DependencyBag::new(),
    );
    command.auxiliary_model = Some(summary);
    command.session_resources = Some(bound.resources.clone());
    command.thread_id = Some(bound.thread_id.clone());
    let compacted = execute_compact(command).await;
    assert!(matches!(
        compacted.feedback.unwrap().level,
        FeedbackLevel::Info
    ));
    let reader =
        peri_resources::sessions::SessionResourcesImpl::open_existing_read_only(&bound.db_path)
            .await
            .unwrap();
    let snapshot = reader
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert!(snapshot.flags[&original.id()].excluded);
    assert!(snapshot
        .payloads
        .iter()
        .any(|payload| payload.id() == original.id()));
    let session = bound.session(snapshot.payloads);
    session.transcript().write().set_flags_batch(snapshot.flags);
    let (model, summary) = make_models(false);
    let context = make_context(&session, model.clone(), summary.clone());
    session.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("continue"),
    ));
    let result = run_react_loop(context, 4).await;
    assert!(matches!(result, LoopResult::Completed), "{result:?}");
    assert_eq!(summary.calls.load(Ordering::SeqCst), 0);
    assert!(model.requests.lock()[0]
        .iter()
        .all(|message| !message.content().contains("ORIGINAL_HIDDEN_SENTINEL")));
    assert!(model.requests.lock()[0]
        .iter()
        .any(|message| message.content().contains("RECOVERED")));
}

struct AppendBeforeModel(Arc<AtomicUsize>);

#[async_trait]
impl peri_agent::middleware::Middleware for AppendBeforeModel {
    fn name(&self) -> &str {
        "append-before-model"
    }
    async fn before_model(
        &self,
        state: &mut dyn peri_agent::middleware::capabilities::BeforeModelState,
    ) -> AgentResult<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        state.add_message(BaseMessage::human("z".repeat(440_000)));
        Ok(())
    }
}

/// [回归测试] before_model 在常规 Compact 后新增内容仍须在真实请求前压缩，hook只跑一次。
#[tokio::test]
async fn test_compact_session_before_model_growth_is_checked_once_before_request() {
    let bound = BoundSession::open().await;
    let session = bound.session(Vec::new());
    let (model, summary) = make_models(false);
    let calls = Arc::new(AtomicUsize::new(0));
    let mut chain = peri_agent::middleware::MiddlewareChain::new();
    chain.add(Box::new(AppendBeforeModel(calls.clone())));
    let mut context = make_context(&session, model.clone(), summary.clone());
    context.runtime.middleware_chain = Arc::new(chain);
    session.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("small task"),
    ));
    let result = run_react_loop(context, 4).await;
    assert!(matches!(result, LoopResult::Completed), "{result:?}");
    assert_eq!(*model.order.lock(), ["summary", "reason"]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

struct MicroGrowthModel(AtomicUsize);

#[async_trait]
impl ReactLLM for MicroGrowthModel {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _: &[&dyn BaseTool],
        _: Option<StreamingContext>,
    ) -> AgentResult<Reasoning> {
        let call = self.0.fetch_add(1, Ordering::SeqCst);
        let summarized = messages
            .iter()
            .any(|message| message.content().contains("RECOVERED"));
        // 固定 provider 边界：旧 Read 的长重复正文实际成本很低，字符级 Micro
        // 节省不是权威节省；90k 真实基线主要来自其他输入/编码成本。
        if !summarized
            && messages
                .iter()
                .any(|message| message.content().contains("NEW_WORK_SENTINEL"))
        {
            return Err(AgentError::LlmHttpError {
                status: 400,
                message: "fixture: 90000 unchanged provider tokens + 12000 new work exceeds limit"
                    .into(),
            });
        }
        let mut result = if call == 0 {
            Reasoning::with_tools(
                "",
                vec![peri_agent::agent::react::ToolCall::new(
                    "work",
                    "Work",
                    serde_json::json!({}),
                )],
            )
        } else {
            Reasoning::with_answer("", "done")
        };
        result.usage = Some(TokenUsage {
            input_tokens: if summarized { 1_000 } else { 90_000 },
            output_tokens: 1,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
        Ok(result)
    }
}

struct ShortWork;

#[async_trait]
impl BaseTool for ShortWork {
    fn name(&self) -> &str {
        "Work"
    }
    fn description(&self) -> &str {
        "return short result"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type":"object"})
    }
    fn is_direct(&self) -> bool {
        true
    }
    async fn invoke(
        &self,
        _: serde_json::Value,
        _: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok("done".into())
    }
}

struct AppendAfterMicro(AtomicUsize);

#[async_trait]
impl peri_agent::middleware::Middleware for AppendAfterMicro {
    fn name(&self) -> &str {
        "append-after-micro"
    }
    async fn before_model(
        &self,
        state: &mut dyn peri_agent::middleware::capabilities::BeforeModelState,
    ) -> AgentResult<()> {
        if self.0.fetch_add(1, Ordering::SeqCst) == 1 {
            state.add_message(BaseMessage::human(format!(
                "NEW_WORK_SENTINEL{}",
                "new ".repeat(12_000)
            )));
        }
        Ok(())
    }
}

/// [对抗回归] Micro 的近似缩减不能抵销后续新增工作，旧权威 usage 尚未验证压缩收益。
#[tokio::test]
async fn test_compact_session_micro_shrink_does_not_hide_new_before_model_growth() {
    let bound = BoundSession::open().await;
    let history = vec![
        BaseMessage::human("old read"),
        BaseMessage::ai_with_tool_calls(
            "",
            vec![peri_agent::messages::ToolCallRequest::new(
                "old-read",
                "Read",
                serde_json::json!({"file_path":"/fixture"}),
            )],
        ),
        BaseMessage::tool_result("old-read", "x".repeat(160_000)),
        BaseMessage::human("new task"),
    ];
    let payloads: Vec<_> = history.into_iter().map(PersistedPayload::Message).collect();
    bound
        .resources
        .append_history(&bound.thread_id, &payloads)
        .await
        .unwrap();
    let session = bound.session(payloads);
    let model = Arc::new(MicroGrowthModel(AtomicUsize::new(0)));
    let (_, summary) = make_models(false);
    let mut chain = peri_agent::middleware::MiddlewareChain::new();
    chain.add(Box::new(AppendAfterMicro(AtomicUsize::new(0))));
    let (bus, mut events) = peri_agent::agent::events_v2::EventBus::new(Default::default());
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(model.clone())
    .with_compact_llm(summary.clone())
    .with_context_budget(ContextBudget::new(100_000))
    .with_compact_config(CompactConfig {
        micro_compact_stale_steps: 0,
        ..Default::default()
    })
    .with_middleware_chain(Arc::new(chain))
    .with_event_bus(Arc::new(bus))
    .with_tools(Arc::new(parking_lot::RwLock::new(
        std::collections::BTreeMap::from([(
            "Work".into(),
            Arc::new(ShortWork) as Arc<dyn BaseTool>,
        )]),
    )))
    .build();
    session.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("continue"),
    ));
    let result = run_react_loop(context.clone(), 5).await;
    assert!(
        std::iter::from_fn(|| events.try_observe()).any(|event| matches!(
            event,
            peri_agent::agent::events_v2::ObserveEvent::MessagesCompacted {
                outcome: peri_agent::agent::compact_v2::CompactOutcome::MicroApplied,
                ..
            }
        )),
        "必须真实应用 Micro 才能暴露抵销"
    );
    assert!(
        matches!(result, LoopResult::Completed),
        "Micro 后新增 12k 不得被净缩减吞掉，结果 {result:?}，tracker {:?}，summary {}",
        context
            .compact
            .token_tracker
            .read()
            .estimated_context_tokens(),
        summary.calls.load(Ordering::SeqCst)
    );
    assert_eq!(summary.calls.load(Ordering::SeqCst), 1);
}

struct UsageSequence(AtomicUsize);

#[async_trait]
impl ReactLLM for UsageSequence {
    async fn generate_reasoning(
        &self,
        _: &[BaseMessage],
        _: &[&dyn BaseTool],
        _: Option<StreamingContext>,
    ) -> AgentResult<Reasoning> {
        let call = self.0.fetch_add(1, Ordering::SeqCst);
        let mut result = Reasoning::with_answer("", "done");
        result.usage = Some(TokenUsage {
            input_tokens: match call {
                0 => 1_000,
                1 => 0,
                _ => 700,
            },
            output_tokens: 1,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
        Ok(result)
    }
}

/// [负对照] 零 usage 不能结清新增输入，多次相同视图检查不重复累加；有效 usage 才结清。
#[tokio::test]
async fn test_compact_session_zero_usage_keeps_growth_and_valid_usage_settles_it() {
    use peri_agent::agent::stages::{compact, reason, CompactInput, ReasonInput};
    let bound = BoundSession::open().await;
    let session = bound.session(Vec::new());
    session
        .transcript()
        .write()
        .append(BaseMessage::human("base"));
    let (_, summary) = make_models(false);
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(UsageSequence(AtomicUsize::new(0))))
    .with_compact_llm(summary.clone())
    .with_context_budget(ContextBudget::new(100_000))
    .with_compact_config(CompactConfig::default())
    .build();
    reason::run_reason(ReasonInput {
        context: context.clone(),
        has_tool_calls: false,
    })
    .await
    .unwrap();
    session
        .transcript()
        .write()
        .append(BaseMessage::human("x".repeat(4_000)));
    for _ in 0..3 {
        compact::run_compact(CompactInput {
            context: context.clone(),
            has_tool_calls: false,
        })
        .await
        .unwrap();
        assert_eq!(
            context
                .compact
                .token_tracker
                .read()
                .estimated_context_tokens(),
            Some(2_000)
        );
    }
    reason::run_reason(ReasonInput {
        context: context.clone(),
        has_tool_calls: false,
    })
    .await
    .unwrap();
    for _ in 0..3 {
        compact::run_compact(CompactInput {
            context: context.clone(),
            has_tool_calls: false,
        })
        .await
        .unwrap();
        assert_eq!(
            context
                .compact
                .token_tracker
                .read()
                .estimated_context_tokens(),
            Some(2_000)
        );
    }
    reason::run_reason(ReasonInput {
        context: context.clone(),
        has_tool_calls: false,
    })
    .await
    .unwrap();
    assert_eq!(
        context
            .compact
            .token_tracker
            .read()
            .estimated_context_tokens(),
        Some(700)
    );
    assert_eq!(summary.calls.load(Ordering::SeqCst), 0);
}

/// [负对照] 同一 Micro 投影视图反复检查不增加压力，也不重复提交压缩。
#[tokio::test]
async fn test_compact_session_repeated_micro_view_does_not_add_pressure() {
    use peri_agent::agent::stages::{compact, reason, CompactInput, ReasonInput};
    let bound = BoundSession::open().await;
    let session = bound.session(Vec::new());
    for message in [
        BaseMessage::human("old"),
        BaseMessage::ai_with_tool_calls(
            "",
            vec![peri_agent::messages::ToolCallRequest::new(
                "old",
                "Read",
                serde_json::json!({"file_path":"/fixture"}),
            )],
        ),
        BaseMessage::tool_result("old", "x".repeat(160_000)),
        BaseMessage::human("current"),
    ] {
        session.transcript().write().append(message);
    }
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(MicroGrowthModel(AtomicUsize::new(0))))
    .with_context_budget(ContextBudget::new(100_000))
    .with_compact_config(CompactConfig {
        micro_compact_stale_steps: 0,
        ..Default::default()
    })
    .build();
    reason::run_reason(ReasonInput {
        context: context.clone(),
        has_tool_calls: false,
    })
    .await
    .unwrap();
    assert!(
        compact::run_compact(CompactInput {
            context: context.clone(),
            has_tool_calls: false,
        })
        .await
        .unwrap()
        .compacted
    );
    for _ in 0..3 {
        assert!(
            !compact::run_compact(CompactInput {
                context: context.clone(),
                has_tool_calls: false,
            })
            .await
            .unwrap()
            .compacted
        );
    }
    assert_eq!(
        context
            .compact
            .token_tracker
            .read()
            .estimated_context_tokens(),
        Some(90_000),
        "投影缩减后重复评估不能扣权威 usage，也不能重计增长"
    );
}
