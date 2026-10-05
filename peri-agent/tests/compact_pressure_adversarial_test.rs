//! 对抗场景：已收到 usage 后，真实模型视图增长必须在下一次 Reason 前参与预算。

use peri_acp_types::session_resources::{
    FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources,
};
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};
use peri_acp_types::thread::CancelPolicy;
use peri_acp_types::workspace::{SessionBinding, SESSION_BINDING_VERSION};
use peri_agent::agent::compact_v2::CompactConfig;
use peri_agent::agent::react::{ReactLLM, Reasoning, StreamingContext, ToolCall};
use peri_agent::agent::stages::{run_react_loop, LoopResult, StageContext};
use peri_agent::agent::token::ContextBudget;
use peri_agent::error::{AgentError, AgentResult};
use peri_agent::messages::BaseMessage;
use peri_agent::session::{
    FrozenContext, MessageKind, MessageQueue, MessageSource, QueuedMessage, Session,
};
use peri_agent::tools::{BaseTool, ToolContext};
use peri_model::{
    ModelCapabilities, ModelMessage, ModelRequest, ModelResponse, ModelStream, TokenUsage,
};
use peri_resources::sessions::SessionResourcesImpl;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy)]
enum Growth {
    Assistant,
    User,
    Reminder,
    ToolControl,
    BeforeModel,
}

struct LateInput(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl peri_agent::middleware::Middleware for LateInput {
    fn name(&self) -> &str {
        "late_input"
    }
    async fn before_model(
        &self,
        state: &mut dyn peri_agent::middleware::capabilities::BeforeModelState,
    ) -> AgentResult<()> {
        if self.0.fetch_add(1, Ordering::SeqCst) == 1 {
            state.add_message(BaseMessage::human("new ".repeat(12_000)));
        }
        Ok(())
    }
}

struct ScriptedReasoner {
    growth: Growth,
    request_tokens: parking_lot::Mutex<Vec<usize>>,
}

#[async_trait::async_trait]
impl ReactLLM for ScriptedReasoner {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _: &[&dyn BaseTool],
        _: Option<StreamingContext>,
    ) -> AgentResult<Reasoning> {
        // 固定模型将四个可见字符计为一个 token；直接测量真实 Reason 投影，
        // 不依赖 provider output usage 是否包含不可回灌的 thinking。
        let tokens = messages
            .iter()
            .map(|message| message.content().chars().count())
            .sum::<usize>()
            / 4;
        let mut requests = self.request_tokens.lock();
        requests.push(tokens);
        if tokens > 100_000 {
            return Err(AgentError::LlmError(format!(
                "scripted context overflow: {tokens} > 100000"
            )));
        }
        let first = requests.len() == 1;
        let mut result = if first {
            Reasoning::with_tools(
                if matches!(self.growth, Growth::Assistant) {
                    "new ".repeat(8_000)
                } else {
                    String::new()
                },
                vec![ToolCall::new("work", "Work", serde_json::json!({}))],
            )
        } else {
            Reasoning::with_answer("", "done")
        };
        result.usage = Some(TokenUsage {
            input_tokens: tokens as u32,
            output_tokens: if first && matches!(self.growth, Growth::Assistant) {
                8_000
            } else {
                1
            },
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
        Ok(result)
    }
}

struct GrowthTool {
    growth: Growth,
    queue: MessageQueue,
}

#[async_trait::async_trait]
impl BaseTool for GrowthTool {
    fn name(&self) -> &str {
        "Work"
    }
    fn description(&self) -> &str {
        "Complete one work item"
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
        _: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        match self.growth {
            Growth::User => self.queue.push(QueuedMessage::prompt(
                MessageSource::UserInput,
                BaseMessage::human("new ".repeat(12_000)),
            )),
            Growth::Reminder => {
                let reminder = TrustedSystemReminderFactory::for_producer()
                    .construct(SystemReminder {
                        version: SYSTEM_REMINDER_VERSION,
                        category: ReminderCategory::Guidance,
                        source: ReminderSource("background_work".into()),
                        kind: "completed".into(),
                        severity: ReminderSeverity::Info,
                        delivery: ReminderDelivery::Required,
                        audiences: ReminderAudiences(vec![ReminderAudience::Model]),
                        body: "new ".repeat(12_000),
                        summary: None,
                        metadata: serde_json::json!({}),
                    })
                    .unwrap();
                self.queue.push(QueuedMessage::system_reminder(
                    MessageKind::Defer,
                    MessageSource::SystemInjected,
                    reminder,
                ));
            }
            _ => {}
        }
        Ok(match self.growth {
            Growth::Assistant => "out ".repeat(4_000),
            Growth::ToolControl => "out ".repeat(12_000),
            _ => "done".to_owned(),
        })
    }
}

struct SummaryModel(AtomicUsize);

#[async_trait::async_trait]
impl peri_model::Model for SummaryModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
    async fn stream(
        &self,
        _: ModelRequest,
        _: CancellationToken,
    ) -> peri_model::ModelResult<ModelStream> {
        unreachable!("只使用 complete")
    }
    async fn complete(
        &self,
        _: ModelRequest,
        _: CancellationToken,
    ) -> peri_model::ModelResult<ModelResponse> {
        self.0.fetch_add(1, Ordering::SeqCst);
        ModelResponse::new(
            ModelMessage::assistant_text(
                "<summary>Original task and current progress preserved; continue.</summary>",
            ),
            peri_model::StopReason::EndTurn,
            None,
            None,
        )
    }
}

struct BoundSession {
    session: Arc<Session>,
    resources: Arc<dyn SessionResources>,
    thread_id: String,
    _repo: tempfile::TempDir,
    _db: tempfile::TempDir,
}

async fn make_bound_session() -> BoundSession {
    let repo = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
        ],
    ] {
        let status = std::process::Command::new("git")
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", repo.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    }
    let db = tempfile::tempdir().unwrap();
    let resources: Arc<dyn SessionResources> = Arc::new(
        SessionResourcesImpl::open(db.path().join("threads.db"))
            .await
            .unwrap(),
    );
    let workspace = resources.resolve_workspace(repo.path()).await.unwrap();
    let thread_id = uuid::Uuid::now_v7().to_string();
    resources
        .create_session(&NewSession {
            thread_id: thread_id.clone(),
            created_at: "2026-09-28T00:00:00Z".into(),
            meta: NewSessionMeta {
                title: Some("pressure adversarial".into()),
                cwd: repo.path().to_string_lossy().into_owned(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: CancelPolicy::default(),
                snapshot_at_message_id: None,
            },
            binding: SessionBinding {
                schema_version: SESSION_BINDING_VERSION,
                revision: 1,
                project_id: workspace.project_id,
                workspace_id: workspace.execution_registration_id,
                cwd_relative_to_workspace: workspace.relative_cwd,
            },
            frozen: FrozenSnapshotBytes::new("{\"version\":1,\"test\":true}"),
        })
        .await
        .unwrap();
    let session = Session::new(
        Arc::from(repo.path().to_string_lossy().as_ref()),
        FrozenContext::builder().build(),
        Some(thread_id.clone()),
    );
    {
        let transcript = session.transcript();
        let mut transcript = transcript.write();
        *transcript =
            std::mem::take(&mut *transcript).with_persistence(resources.clone(), thread_id.clone());
    }
    BoundSession {
        session,
        resources,
        thread_id,
        _repo: repo,
        _db: db,
    }
}

async fn assert_growth_compacted_before_next_reason(growth: Growth) {
    let bound = make_bound_session().await;
    let model = Arc::new(ScriptedReasoner {
        growth,
        request_tokens: Default::default(),
    });
    let summary = Arc::new(SummaryModel(AtomicUsize::new(0)));
    let tool = Arc::new(GrowthTool {
        growth,
        queue: bound.session.queue().clone(),
    });
    let mut chain = peri_agent::middleware::MiddlewareChain::new();
    let late_calls = Arc::new(AtomicUsize::new(0));
    if matches!(growth, Growth::BeforeModel) {
        chain.add(Box::new(LateInput(late_calls.clone())));
    }
    let ctx = StageContext::builder(
        bound.session.start_turn(),
        bound.session.transcript(),
        bound.session.queue().clone(),
    )
    .with_llm(model.clone())
    .with_compact_llm(summary.clone())
    .with_context_budget(ContextBudget::new(100_000))
    .with_compact_config(CompactConfig::default())
    .with_middleware_chain(Arc::new(chain))
    .with_tools(Arc::new(parking_lot::RwLock::new(
        std::collections::BTreeMap::from([("Work".into(), tool as Arc<dyn BaseTool>)]),
    )))
    .build();
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("log ".repeat(90_000)),
    ));
    let result = run_react_loop(ctx.clone(), 5).await;
    assert!(matches!(result, LoopResult::Completed), "下一次 Reason 前应压缩；result={result:?}, real_request_tokens={:?}, tracker={:?}, summary_calls={}", *model.request_tokens.lock(), ctx.compact.token_tracker.read().estimated_context_tokens(), summary.0.load(Ordering::SeqCst));
    assert_eq!(
        summary.0.load(Ordering::SeqCst),
        1,
        "超过窗口时必须在同循环内 Full"
    );
    assert_eq!(model.request_tokens.lock().len(), 2);
    assert!(model.request_tokens.lock()[1] < 95_000);
    if matches!(growth, Growth::BeforeModel) {
        assert_eq!(
            late_calls.load(Ordering::SeqCst),
            2,
            "补检不能重新运行 before_model"
        );
    }
    let transcript = std::mem::take(&mut *ctx.session.transcript.write());
    transcript.flush_persistence().await.unwrap();
}

/// [回归测试] 最近 assistant 的可见正文 8k + 工具 4k 将输入从 90k 推到 102k。
#[tokio::test]
async fn test_compact_accounts_for_committed_assistant_growth() {
    assert_growth_compacted_before_next_reason(Growth::Assistant).await;
}

/// [回归测试] 用户在工具运行时追加 12k 输入，同次循环 Receive 必须更新预算。
#[tokio::test]
async fn test_compact_accounts_for_received_user_growth() {
    assert_growth_compacted_before_next_reason(Growth::User).await;
}

/// [回归测试] 后台结果经 canonical reminder 注入，同次循环必须计入模型输入。
#[tokio::test]
async fn test_compact_accounts_for_received_reminder_growth() {
    assert_growth_compacted_before_next_reason(Growth::Reminder).await;
}

/// 相同 fixture 的阳性对照：12k 工具结果已被 tracker 识别，应能成功 Full。
#[tokio::test]
async fn test_compact_tool_growth_control() {
    assert_growth_compacted_before_next_reason(Growth::ToolControl).await;
}

/// [回归测试] before_model 发生在 Compact 后；新增输入必须在最终请求前补检。
#[tokio::test]
async fn test_compact_accounts_for_before_model_growth() {
    assert_growth_compacted_before_next_reason(Growth::BeforeModel).await;
}

async fn assert_usable_summary_commits(raw: &'static str) {
    let bound = make_bound_session().await;
    let ctx = StageContext::builder(
        bound.session.start_turn(),
        bound.session.transcript(),
        bound.session.queue().clone(),
    )
    .with_compact_llm(Arc::new(ScriptedMarkupSummary(raw)))
    .with_context_budget(ContextBudget::new(100_000))
    .with_compact_config(CompactConfig::default())
    .build();
    let original = BaseMessage::human("original task requiring preservation");
    let id = original.id();
    ctx.session.transcript.write().append(original);
    ctx.compact.token_tracker.write().accumulate(&TokenUsage {
        input_tokens: 109_000,
        output_tokens: 0,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    });
    let result =
        peri_agent::agent::stages::compact::run_compact(peri_agent::agent::stages::CompactInput {
            context: ctx.clone(),
            has_tool_calls: false,
        })
        .await
        .unwrap();
    let transcript = std::mem::take(&mut *ctx.session.transcript.write());
    transcript.flush_persistence().await.unwrap();
    assert!(result.compacted);
    assert!(transcript.flags(id).excluded, "有效摘要允许替换旧模型视图");
    assert!(
        transcript.entries().iter().any(|entry| entry.id() == id),
        "canonical 原文仍保留"
    );
    let visible = transcript.visible_messages();
    assert_eq!(visible.len(), 1);
    assert!(visible[0].content().contains("USABLE_PROGRESS"));
    assert!(!visible[0].content().contains("internal only"));
}

/// 有摘要正文的嵌套思考响应必须仍可提交，不能因严格判空拒绝全部响应。
#[tokio::test]
async fn test_nested_thinking_with_usable_summary_commits() {
    assert_usable_summary_commits(
        "<thinking><analysis>internal only</analysis></thinking><summary>USABLE_PROGRESS</summary>",
    )
    .await;
}

/// 无标签自然语言摘要仍是支持的模型响应。
#[tokio::test]
async fn test_plain_text_usable_summary_commits() {
    assert_usable_summary_commits("USABLE_PROGRESS").await;
}

struct ScriptedMarkupSummary(&'static str);

#[derive(Default)]
struct AttachmentReasoner(parking_lot::Mutex<Vec<Vec<BaseMessage>>>);

#[async_trait::async_trait]
impl ReactLLM for AttachmentReasoner {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _: &[&dyn BaseTool],
        _: Option<StreamingContext>,
    ) -> AgentResult<Reasoning> {
        self.0.lock().push(messages.to_vec());
        Ok(Reasoning::with_answer("", "attachment received"))
    }
}

async fn assert_binary_attachment_reaches_first_reason(cold_document: bool) {
    use peri_agent::messages::{ContentBlock, DocumentSource, ImageSource, MessageContent};
    let bound = make_bound_session().await;
    // 固定编码载荷只测试传输和预算边界，脚本模型不解码图片或 PDF。
    let data = "AAAA".repeat(175_000);
    let block = if cold_document {
        ContentBlock::Document {
            source: DocumentSource::Base64 {
                media_type: "application/pdf".into(),
                data,
            },
            title: Some("attached report".into()),
        }
    } else {
        ContentBlock::Image {
            source: ImageSource::Base64 {
                media_type: "image/png".into(),
                data,
            },
        }
    };
    let attachment = BaseMessage::human(MessageContent::blocks(vec![
        ContentBlock::text("Inspect this attachment."),
        block,
    ]));
    let session = if cold_document {
        bound
            .session
            .transcript()
            .write()
            .append(attachment.clone());
        let old = std::mem::take(&mut *bound.session.transcript().write());
        old.flush_persistence().await.unwrap();
        let reopened = SessionResourcesImpl::open(bound._db.path().join("threads.db"))
            .await
            .unwrap();
        let snapshot = reopened
            .load_session_snapshot(&bound.thread_id)
            .await
            .unwrap();
        let restored = Session::new(
            Arc::from(bound._repo.path().to_string_lossy().as_ref()),
            FrozenContext::builder().build(),
            Some(bound.thread_id.clone()),
        );
        let transcript = restored.transcript();
        let mut guard = transcript.write();
        *guard = peri_agent::session::MessageTranscript::new()
            .with_own_payloads(snapshot.payloads)
            .with_persistence(bound.resources.clone(), bound.thread_id.clone());
        guard.set_flags_batch(snapshot.flags);
        drop(guard);
        restored.queue().push(QueuedMessage::prompt(
            MessageSource::UserInput,
            BaseMessage::human("Continue inspecting the attachment."),
        ));
        restored
    } else {
        bound.session.queue().push(QueuedMessage::prompt(
            MessageSource::UserInput,
            attachment.clone(),
        ));
        bound.session.clone()
    };
    let model = Arc::new(AttachmentReasoner::default());
    let summary = Arc::new(SummaryModel(AtomicUsize::new(0)));
    let ctx = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(model.clone())
    .with_compact_llm(summary.clone())
    .with_context_budget(ContextBudget::new(100_000))
    .with_compact_config(CompactConfig::default())
    .build();
    let result = run_react_loop(ctx.clone(), 3).await;
    assert!(matches!(result, LoopResult::Completed), "{result:?}");
    assert_eq!(
        summary.0.load(Ordering::SeqCst),
        0,
        "二进制base64传输大小不能触发提前Full并排除附件"
    );
    {
        let requests = model.0.lock();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0]
                .iter()
                .any(|message| message.id() == attachment.id()
                    && message.message_content() == attachment.message_content()),
            "首个Reason必须收到完整原始附件"
        );
    }
    assert!(
        !ctx.session
            .transcript
            .read()
            .flags(attachment.id())
            .excluded
    );
    let transcript = std::mem::take(&mut *ctx.session.transcript.write());
    transcript.flush_persistence().await.unwrap();
}

/// [回归测试] 约500KB截图不能按base64字符估出175k输入而在首次Reason前被压缩。
#[tokio::test]
async fn test_binary_attachment_fresh_image_reaches_first_reason() {
    assert_binary_attachment_reaches_first_reason(false).await;
}

/// [回归测试] SQLite冷恢复的大附件也不能因base64长度在首次Reason前被排除。
#[tokio::test]
async fn test_binary_attachment_cold_document_reaches_first_reason() {
    assert_binary_attachment_reaches_first_reason(true).await;
}

#[async_trait::async_trait]
impl peri_model::Model for ScriptedMarkupSummary {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
    async fn stream(
        &self,
        _: ModelRequest,
        _: CancellationToken,
    ) -> peri_model::ModelResult<ModelStream> {
        unreachable!()
    }
    async fn complete(
        &self,
        _: ModelRequest,
        _: CancellationToken,
    ) -> peri_model::ModelResult<ModelResponse> {
        ModelResponse::new(
            ModelMessage::assistant_text(self.0),
            peri_model::StopReason::EndTurn,
            None,
            None,
        )
    }
}

/// [回归测试] 嵌套 analysis 的闭合标签不是摘要，不能据此隐藏完整历史。
#[tokio::test]
async fn test_nested_analysis_only_response_cannot_replace_history() {
    let bound = make_bound_session().await;
    let ctx = StageContext::builder(
        bound.session.start_turn(),
        bound.session.transcript(),
        bound.session.queue().clone(),
    )
    .with_compact_llm(Arc::new(ScriptedMarkupSummary(
        "<analysis><analysis>internal only</analysis></analysis>",
    )))
    .with_context_budget(ContextBudget::new(100_000))
    .with_compact_config(CompactConfig::default())
    .build();
    let original = BaseMessage::human("original task requiring preservation");
    let id = original.id();
    ctx.session.transcript.write().append(original);
    ctx.compact.token_tracker.write().accumulate(&TokenUsage {
        input_tokens: 109_000,
        output_tokens: 0,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    });
    let result =
        peri_agent::agent::stages::compact::run_compact(peri_agent::agent::stages::CompactInput {
            context: ctx.clone(),
            has_tool_calls: false,
        })
        .await;
    assert!(
        !ctx.session.transcript.read().flags(id).excluded,
        "嵌套analysis没有摘要正文，不能把闭合标签当作有效摘要并excluded原文；visible={:?}",
        ctx.session.transcript.read().visible_messages()
    );
    assert!(matches!(
        result,
        Err(AgentError::CompactRetriesExhausted { attempts: 3, .. })
    ));
}
