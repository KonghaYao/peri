//! 手动 `/compact` → 冷启动 → 首个真实请求的恢复入口重算（临时 SQLite，确定性模型）。
//!
//! 手动路径采样时没有目录快照；首个请求必须按最终目录补齐可执行入口，且服务端
//! owner id 与宿主 task id 不得互换。
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_acp_types::command::{CommandContext, DependencyBag};
use peri_acp_types::event::{EventSink, EventSinkError, ExecutorEvent};
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session_resources::{
    FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources,
};
use peri_acp_types::tasks::{
    BgRegistryEvent, BgShellHandle, BgTaskKind, BgTaskRegistration, ExternalTaskOwner,
    OnBgCompleteFn, TaskManager, TaskRegistrationTicket, TaskSnapshot,
};
use peri_acp_types::thread::CancelPolicy;
use peri_acp_types::workspace::SessionBinding;
use peri_agent::agent::compact_v2::{CompactConfig, HardMemorySource, RuntimeHardMemory};
use peri_agent::agent::react::{Reasoning, StreamingContext};
use peri_agent::agent::stages::{ReasonInput, StageContext};
use peri_agent::agent::ReactLLM;
use peri_agent::messages::MessageContent;
use peri_agent::session::tool_catalog::SessionToolCatalog;
use peri_agent::session::{MessageTranscript, Session};
use peri_agent::tools::BaseTool;
use peri_model::{
    Model, ModelCapabilities, ModelMessage, ModelRequest, ModelResponse, ModelResult, ModelStream,
    StopReason,
};
use tokio_util::sync::CancellationToken;

// ─── 运行时任务夹具：一个仍在跑的服务端 workflow ─────────────────────────────

struct StaticTasks {
    snapshot: TaskSnapshot,
}

impl TaskManager for StaticTasks {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_arc_any(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync> {
        self
    }
    fn set_event_sender(&self, _: tokio::sync::mpsc::UnboundedSender<BgRegistryEvent>, _: String) {}
    fn active_count(&self) -> usize {
        self.snapshot.tasks.len()
    }
    fn register(&self, _: BgTaskRegistration) -> Result<TaskRegistrationTicket, String> {
        Err("static fixture".into())
    }
    fn snapshot(&self) -> TaskSnapshot {
        self.snapshot.clone()
    }
    fn complete(
        &self,
        _: &TaskRegistrationTicket,
        _: peri_acp_types::event::BackgroundTaskResult,
    ) -> bool {
        false
    }
    fn settle_completed(
        &self,
        _: &TaskRegistrationTicket,
        _: peri_acp_types::event::BackgroundTaskResult,
        _: OnBgCompleteFn,
    ) -> Result<bool, String> {
        Ok(false)
    }
    fn retry_pending_deliveries(&self) -> usize {
        0
    }
    fn cancel(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn cancel_all(&self) {}
    fn spawn_shell(
        &self,
        _: String,
        _: String,
        _: Option<u64>,
        _: Option<OnBgCompleteFn>,
    ) -> Result<BgShellHandle, Box<dyn std::error::Error + Send + Sync>> {
        Err("static fixture".into())
    }
    fn finalize_bg_shell(
        &self,
        _: Option<&OnBgCompleteFn>,
        _: TaskRegistrationTicket,
        _: String,
        _: bool,
        _: String,
        _: u64,
        _: bool,
        _: Option<peri_acp_types::event::ShellOutput>,
    ) {
    }
}

fn running_workflow() -> TaskSnapshot {
    TaskSnapshot {
        revision: 4,
        tasks: vec![peri_acp_types::tasks::TaskRecord {
            task_id: "mcp-host-workflow".into(),
            kind: BgTaskKind::Workflow,
            summary: "build workflow".into(),
            started_at: "2026-11-11T00:00:00Z".into(),
            status: "running".into(),
            duration_ms: 0,
            output_preview: None,
            initiator_session_id: Some("session".into()),
            child_thread_id: None,
            external_owner: Some(ExternalTaskOwner::workflow("run-server-1")),
        }],
    }
}

// ─── 模型夹具 ────────────────────────────────────────────────────────────────

#[derive(Default)]
struct SummaryModel {
    calls: AtomicUsize,
}

#[async_trait]
impl Model for SummaryModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_tools: false,
            supports_reasoning: false,
            supports_vision: false,
            supports_streaming: false,
        }
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        Err(peri_model::ModelError::cancelled())
    }

    async fn complete(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelResult<ModelResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ModelResponse::try_new(
            ModelMessage::assistant_text("<summary>workflow still running</summary>"),
            StopReason::EndTurn,
            None,
            None,
        )?)
    }
}

#[derive(Debug, Clone)]
struct Captured {
    messages: Vec<BaseMessage>,
}

#[derive(Default)]
struct CapturingLlm {
    requests: Mutex<Vec<Captured>>,
}

#[async_trait]
impl ReactLLM for CapturingLlm {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> peri_agent::error::AgentResult<Reasoning> {
        self.requests.lock().unwrap().push(Captured {
            messages: messages.to_vec(),
        });
        Ok(Reasoning::with_answer("done", "done"))
    }
}

#[derive(Default)]
struct NullSink;

#[async_trait]
impl EventSink for NullSink {
    async fn push_event(&self, _: &str, _: &ExecutorEvent, _: u32) -> Result<(), EventSinkError> {
        Ok(())
    }
    async fn push_done(&self, _: &str, _: &str, _: Option<&str>) {}
}

// ─── 目录夹具 ────────────────────────────────────────────────────────────────

struct CatalogTool {
    name: &'static str,
    original: Option<&'static str>,
    direct: bool,
}

#[async_trait]
impl BaseTool for CatalogTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "catalog fixture"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    fn is_direct(&self) -> bool {
        self.direct
    }
    fn mcp_tool_name(&self) -> Option<&str> {
        self.original
    }
    async fn invoke(
        &self,
        _: serde_json::Value,
        _: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        unreachable!()
    }
}

fn v5_tools() -> BTreeMap<String, Arc<dyn BaseTool>> {
    [
        CatalogTool {
            name: "ToolInvoke",
            original: None,
            direct: true,
        },
        CatalogTool {
            name: "mcp__peri__WorkflowStatus",
            original: Some("WorkflowStatus"),
            direct: false,
        },
        CatalogTool {
            name: "mcp__peri__WorkflowKill",
            original: Some("WorkflowKill"),
            direct: false,
        },
    ]
    .into_iter()
    .map(|tool| (tool.name.to_string(), Arc::new(tool) as Arc<dyn BaseTool>))
    .collect()
}

/// 最终目录：能力关闭（workflow 工具不再可达）。
fn closed_tools() -> BTreeMap<String, Arc<dyn BaseTool>> {
    [CatalogTool {
        name: "Read",
        original: None,
        direct: true,
    }]
    .into_iter()
    .map(|tool| (tool.name.to_string(), Arc::new(tool) as Arc<dyn BaseTool>))
    .collect()
}

// ─── 会话夹具 ────────────────────────────────────────────────────────────────

struct BoundSession {
    resources: Arc<dyn SessionResources>,
    thread_id: String,
    cwd: String,
    _directory: tempfile::TempDir,
}

impl BoundSession {
    async fn open() -> Self {
        let directory = tempfile::tempdir().expect("temp dir");
        let resources: Arc<dyn SessionResources> = Arc::new(
            peri_resources::sessions::SessionResourcesImpl::open(
                directory.path().join("manual.db"),
            )
            .await
            .expect("open store"),
        );
        let workspace = resources
            .resolve_workspace(directory.path())
            .await
            .expect("workspace");
        let thread_id = uuid::Uuid::now_v7().to_string();
        let cwd = workspace.cwd.to_string_lossy().into_owned();
        resources
            .create_session(&NewSession {
                thread_id: thread_id.clone(),
                created_at: "2026-11-11T00:00:00Z".into(),
                meta: NewSessionMeta {
                    title: Some("manual recovery".into()),
                    cwd: cwd.clone(),
                    parent_thread_id: None,
                    hidden: false,
                    cancel_policy: CancelPolicy::default(),
                    snapshot_at_message_id: None,
                },
                binding: SessionBinding::from_workspace(&workspace),
                frozen: FrozenSnapshotBytes::new("{\"version\":1}"),
            })
            .await
            .expect("create session");
        Self {
            resources,
            thread_id,
            cwd,
            _directory: directory,
        }
    }

    /// 冷启动：只从已提交存储重建 transcript。
    async fn restore(&self) -> MessageTranscript {
        let snapshot = self
            .resources
            .load_session_snapshot(&self.thread_id)
            .await
            .expect("load snapshot");
        let mut transcript = MessageTranscript::new().with_own_payloads(snapshot.payloads);
        transcript.set_flags_batch(snapshot.flags);
        transcript
    }
}

async fn manual_compact(bound: &BoundSession) -> peri_acp_types::command::CommandResult {
    let history = vec![
        BaseMessage::human(MessageContent::text("keep working on the build")),
        BaseMessage::ai(MessageContent::text("starting the workflow")),
    ];
    let mut command = CommandContext::new(
        bound.thread_id.clone(),
        history,
        bound.cwd.clone(),
        Arc::new(NullSink),
        CancellationToken::new(),
        DependencyBag::new(),
    );
    command.compact_config = CompactConfig::default();
    command.auxiliary_model = Some(Arc::new(SummaryModel::default()));
    command.session_resources = Some(bound.resources.clone());
    command.thread_id = Some(bound.thread_id.clone());
    command.task_manager = Some(Arc::new(StaticTasks {
        snapshot: running_workflow(),
    }));
    peri_agent::session::exec::compact_pipeline::execute_compact(command).await
}

fn reason_fixture(
    bound: &BoundSession,
    transcript: MessageTranscript,
    tools: BTreeMap<String, Arc<dyn BaseTool>>,
) -> (StageContext, Arc<CapturingLlm>) {
    let cwd: Arc<str> = Arc::from(bound.cwd.as_str());
    let frozen = peri_agent::session::store::FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    let llm = Arc::new(CapturingLlm::default());
    let shared = Arc::new(RwLock::new(tools.clone()));
    let ctx = StageContext::builder(
        turn,
        Arc::new(RwLock::new(transcript)),
        session.queue().clone(),
    )
    .with_llm(llm.clone())
    .with_tools(shared)
    .with_tool_catalog(Arc::new(SessionToolCatalog::new(tools, None)))
    .with_session_context(Arc::new(RwLock::new(HashMap::from([(
        "session_id".to_string(),
        bound.thread_id.clone(),
    )]))))
    .build();
    (ctx, llm)
}

async fn run_reason(ctx: &StageContext) {
    peri_agent::agent::stages::reason::run_reason(ReasonInput {
        context: ctx.clone(),
        has_tool_calls: false,
    })
    .await
    .expect("reason must complete");
}

fn request_text(llm: &CapturingLlm) -> String {
    let requests = llm.requests.lock().unwrap();
    let request = requests.last().expect("request captured");
    request
        .messages
        .iter()
        .map(BaseMessage::content)
        .collect::<Vec<_>>()
        .join("\n")
}

/// 手动 `/compact` 采样没有目录；冷启动后的首个请求按最终目录补齐调用。
#[tokio::test]
async fn manual_compact_first_request_uses_the_final_catalog() {
    let bound = BoundSession::open().await;
    let result = manual_compact(&bound).await;
    assert!(
        !result
            .feedback
            .as_ref()
            .is_some_and(|feedback| feedback.level
                == peri_acp_types::command::FeedbackLevel::Error),
        "手动 compact 必须成功：{:?}",
        result.feedback
    );

    let restored = bound.restore().await;
    let stored = restored
        .visible_messages()
        .iter()
        .find(|message| peri_agent::agent::compact_v2::is_hard_memory_content(&message.content()))
        .expect("硬记忆必须已提交")
        .content();
    assert!(
        stored.contains("not directly available"),
        "手动路径没有目录快照，采样正文只能登记句柄：{stored}"
    );
    assert!(stored.contains("run-server-1"));
    assert!(
        !stored.contains("mcp-host-workflow\" as"),
        "宿主 ID 不得冒充 owner"
    );

    let (ctx, llm) = reason_fixture(&bound, restored, v5_tools());
    run_reason(&ctx).await;

    let text = request_text(&llm);
    assert!(
        text.contains(
            "ToolInvoke({\"name\":\"mcp__peri__WorkflowStatus\",\"arguments\":{\"runId\":\"run-server-1\"}})"
        ),
        "首个请求必须能直接执行：{text}"
    );
    assert!(!text.contains("not directly available"));
    assert_eq!(
        text.matches("mcp__peri__WorkflowStatus\",\"arguments\":{\"runId\":\"run-server-1\"}")
            .count(),
        1,
        "同一目标只生成一条查询入口：{text}"
    );
    assert!(
        !text.contains("\"runId\":\"mcp-host-workflow\""),
        "不得把宿主 task_id 当成服务端 runId"
    );
}

/// 冷启动后能力关闭：请求如实降级，不发出任何旧目录调用。
#[tokio::test]
async fn manual_compact_cold_start_with_closed_capability_degrades() {
    let bound = BoundSession::open().await;
    let result = manual_compact(&bound).await;
    assert!(
        !result
            .feedback
            .as_ref()
            .is_some_and(|feedback| feedback.level
                == peri_acp_types::command::FeedbackLevel::Error),
        "手动 compact 必须成功：{:?}",
        result.feedback
    );

    let (ctx, llm) = reason_fixture(&bound, bound.restore().await, closed_tools());
    run_reason(&ctx).await;

    let text = request_text(&llm);
    assert!(text.contains("run-server-1"), "已登记句柄仍可见");
    assert!(text.contains("WorkflowStatus` is not directly available"));
    assert!(!text.contains("ToolInvoke({"), "能力关闭时不得发出调用");
}

/// 同一份已提交事实在离线重建后仍携带 revision 与作用域。
#[tokio::test]
async fn manual_compact_persists_snapshot_scope_and_revision() {
    let bound = BoundSession::open().await;
    let result = manual_compact(&bound).await;
    assert!(
        !result
            .feedback
            .as_ref()
            .is_some_and(|feedback| feedback.level
                == peri_acp_types::command::FeedbackLevel::Error),
        "手动 compact 必须成功：{:?}",
        result.feedback
    );

    let restored = bound.restore().await;
    let hard_id = restored
        .visible_messages()
        .iter()
        .find(|message| peri_agent::agent::compact_v2::is_hard_memory_content(&message.content()))
        .expect("硬记忆必须已提交")
        .id();
    let directive = restored
        .projection_ref(hard_id)
        .expect("硬记忆必须携带投影事实");
    let facts = directive
        .recovery_memory
        .as_ref()
        .expect("结构化采样事实必须持久化");
    assert_eq!(facts.sampled_revision, 4);
    assert_eq!(facts.scope.as_deref(), Some(bound.thread_id.as_str()));
    assert_eq!(facts.tasks.len(), 1);
    assert_eq!(
        facts.tasks[0]
            .external_owner
            .as_ref()
            .map(|owner| owner.handle.owner_task_id()),
        Some("run-server-1")
    );

    // 运行时来源也能重建同一快照（供装配期诊断）。
    let rebuilt = RuntimeHardMemory::new(
        Some(Arc::new(StaticTasks {
            snapshot: running_workflow(),
        })),
        None,
    )
    .snapshot();
    assert_eq!(rebuilt.revision, 4);
    assert_eq!(rebuilt.tasks.len(), 1);
}
