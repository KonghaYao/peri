//! 生产链序契约测试（ARC-MIDDLEWARE-001 + 2026-07-25 技术债 issue）。
//!
//! 锁定「蓝本（`production_blueprint`）↔ 装配实现（`ProductionChainAssembler`）」
//! 的一一对应：完整序列精确断言 + 条件注册（Hook/MCP/Workflow/Goal）
//! 组合矩阵 + 权限模式不变性。任意中间件被重排、遗漏、重复注册或插入
//! 错误位置时，至少一条测试失败。
//!
//! 链序是行为契约（迁移自 `peri-acp/src/agent/builder.rs`），禁止按名称、
//! 便利性或局部需求重排——修改本文件的期望序列必须先同步修改蓝本。

use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use futures::{stream, TryStreamExt};
use parking_lot::RwLock;
use peri_agent::agent::workflow::WorkflowAgentContext;
use peri_agent::session::subagent::SubagentLlmSource;
use peri_agent::{
    agent::{
        async_tasks::TaskManager,
        events::{AgentEventHandler, ExecutorEvent},
        react::ReactLLM,
        AgentCancellationToken,
    },
    goal::{GoalController, GoalViewSnapshot},
    interaction::{InteractionContext, InteractionResponse, UserInteractionBroker},
    session::factory::{build_middleware_chain, production_blueprint, ChainSlot},
    tools::BaseTool,
};
use peri_model::{
    Model, ModelCapabilities, ModelMessage, ModelRequest, ModelResponse, ModelResult, ModelStream,
    ModelStreamEvent, StopReason,
};
use peri_resources::workflow::protocol::{AgentRunParams, AgentRunResult};
use peri_resources::workflow::runner::AgentExecutor;

use crate::{
    assembly::{
        default_workflow_middleware_factory, default_workflow_middleware_factory_with_pool,
        AssemblyContext, OnBgCompleteFn, ProductionChainAssembler, SystemPromptBuilder,
    },
    hooks::{HookEvent, HookType, RegisteredHook},
    mcp::{McpClientHandle, McpClientPool},
    permission::{PermissionMode, SharedPermissionMode},
    tool_search::ToolSearchIndex,
    tools::TodoItem,
};
use peri_acp_types::agents::AgentOverrides;
use peri_acp_types::execution_admission::{
    AdmissionOutcome, AdmissionRequest, EntryOutcome, EntryReceipt, EntryRequest,
    ExecutionAdmissionError, ExecutionAdmissionPort, SettlementOutcome, SettlementReceipt,
    SettlementRequest,
};
use peri_acp_types::session_resources::{
    work::{WorkAction, WorkAdmission, WorkCommand, WorkDecision, WorkQuery},
    ControlAttempt, FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources,
};
use peri_acp_types::workspace::SessionBinding;
use peri_resources::sessions::SessionResourcesImpl;

// ── fakes ─────────────────────────────────────────────────────────────────────

struct FakeBroker;

#[async_trait]
impl UserInteractionBroker for FakeBroker {
    async fn request(&self, _ctx: InteractionContext) -> InteractionResponse {
        InteractionResponse::Rejected
    }
}

struct FakeDynamicDeployment;

#[async_trait]
impl peri_acp_types::ports::DynamicMcpDeploymentPort for FakeDynamicDeployment {
    async fn execute(
        &self,
        _session_id: &str,
        _action: peri_acp_types::dynamic_mcp::CanonicalDynamicMcpAction,
    ) -> Result<
        peri_acp_types::dynamic_mcp::DynamicMcpResponse,
        peri_acp_types::dynamic_mcp::DynamicMcpFailure,
    > {
        unimplemented!("assembly contract does not execute DynamicMCP")
    }

    fn register_catalog(
        &self,
        _session_id: &str,
        _tools: Vec<peri_acp_types::dynamic_mcp::DynamicMcpCatalogTool>,
    ) -> Result<(), peri_acp_types::dynamic_mcp::DynamicMcpFailure> {
        Ok(())
    }

    fn capability(
        &self,
        _session_id: &str,
    ) -> Arc<dyn peri_acp_types::ports::SessionMcpCapabilityPort> {
        struct Empty;
        impl peri_acp_types::ports::SessionMcpCapabilityPort for Empty {
            fn snapshot(&self) -> Arc<peri_acp_types::dynamic_mcp::SessionMcpCapabilitySnapshot> {
                Arc::new(Default::default())
            }

            fn bind_projection(
                &self,
                _static_pool: Arc<dyn peri_acp_types::ports::McpPoolPort>,
                _skill_registry: Arc<peri_acp_types::mcp_skills::McpSkillRegistry>,
                _command_registry: Arc<peri_acp_types::command_registry::CommandRegistry>,
            ) -> Arc<dyn peri_acp_types::ports::SessionMcpProjectionLease> {
                struct Lease;
                impl peri_acp_types::ports::SessionMcpProjectionLease for Lease {
                    fn as_any(&self) -> &dyn std::any::Any {
                        self
                    }

                    fn refresh(&self) -> bool {
                        true
                    }

                    fn close(&self) {}
                }
                Arc::new(Lease)
            }
        }
        Arc::new(Empty)
    }

    fn close_registration(
        &self,
        _session_id: &str,
    ) -> Arc<dyn peri_acp_types::ports::SessionCloseRegistration> {
        struct Noop;
        #[async_trait]
        impl peri_acp_types::ports::SessionCloseRegistration for Noop {
            async fn revoke_and_cleanup(
                &self,
            ) -> peri_acp_types::dynamic_mcp::DynamicMcpShutdownReport {
                peri_acp_types::dynamic_mcp::DynamicMcpShutdownReport::Complete
            }
        }
        Arc::new(Noop)
    }

    fn begin_shutdown(&self) {}

    async fn close_session(
        &self,
        _session_id: &str,
    ) -> peri_acp_types::dynamic_mcp::DynamicMcpShutdownReport {
        peri_acp_types::dynamic_mcp::DynamicMcpShutdownReport::Complete
    }

    async fn shutdown(&self) -> peri_acp_types::dynamic_mcp::DynamicMcpShutdownReport {
        peri_acp_types::dynamic_mcp::DynamicMcpShutdownReport::Complete
    }
}

struct FakeEventHandler;

impl AgentEventHandler for FakeEventHandler {
    fn on_event(&self, _event: ExecutorEvent) {}
}

struct FakeLlm;

#[async_trait]
impl ReactLLM for FakeLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[peri_agent::messages::BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<peri_agent::agent::react::StreamingContext>,
    ) -> peri_agent::error::AgentResult<peri_agent::agent::react::Reasoning> {
        unimplemented!("契约测试不调用 LLM")
    }
}

struct FakeModel;

#[async_trait]
impl Model for FakeModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_tools: false,
            supports_reasoning: false,
            supports_vision: false,
            supports_streaming: true,
        }
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> ModelResult<ModelStream> {
        unimplemented!("契约测试不调用模型")
    }
}

struct CancelGateModel {
    entered: Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
}

impl Clone for CancelGateModel {
    fn clone(&self) -> Self {
        Self {
            entered: self.entered.clone(),
        }
    }
}

#[derive(Clone)]
struct CompletedWorkflowModel;

fn prepared_workflow_model_call<M: Model + Clone + 'static>(
    model: M,
    request: ModelRequest,
) -> ModelResult<peri_model::PreparedModelCall> {
    let checkpoint = serde_json::json!({
        "provider": "workflow-test",
        "model": "workflow-fixture",
        "endpoint": "https://workflow.test.invalid/stream",
        "credentialRef": "fixture:no-credential",
        "body": serde_json::to_value(&request).unwrap(),
    });
    Ok(peri_model::PreparedModelCall::new(
        checkpoint,
        move |cancellation| {
            let stream_cancellation = cancellation.clone();
            let events =
                stream::once(async move { model.stream(request, stream_cancellation).await })
                    .try_flatten();
            Ok(ModelStream::with_parent_cancellation(events, cancellation))
        },
    ))
}

#[async_trait]
impl Model for CompletedWorkflowModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_streaming: true,
            ..ModelCapabilities::default()
        }
    }

    fn prepare_stream(&self, request: ModelRequest) -> ModelResult<peri_model::PreparedModelCall> {
        prepared_workflow_model_call(self.clone(), request)
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> ModelResult<ModelStream> {
        let response = ModelResponse::new(
            ModelMessage::assistant_text("workflow done"),
            StopReason::EndTurn,
            None,
            Some("workflow-forwarder-failure".into()),
        )?;
        Ok(ModelStream::with_parent_cancellation(
            stream::iter(vec![Ok(ModelStreamEvent::Completed(response))]),
            cancellation,
        ))
    }
}

#[async_trait]
impl Model for CancelGateModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_streaming: true,
            ..ModelCapabilities::default()
        }
    }

    fn prepare_stream(&self, request: ModelRequest) -> ModelResult<peri_model::PreparedModelCall> {
        prepared_workflow_model_call(self.clone(), request)
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> ModelResult<ModelStream> {
        if let Some(entered) = self.entered.lock().unwrap().take() {
            let _ = entered.send(());
        }
        Ok(ModelStream::with_parent_cancellation(
            stream::pending::<ModelResult<ModelStreamEvent>>(),
            cancellation,
        ))
    }
}

struct FakeGoalController;

#[async_trait]
impl GoalController for FakeGoalController {
    async fn create_goal(&self, _objective: String) -> Result<(), String> {
        Ok(())
    }
    async fn complete_goal(&self) -> Result<(), String> {
        Ok(())
    }
    async fn block_goal(&self, _reason: String) -> Result<(), String> {
        Ok(())
    }
    async fn clear_goal(&self) -> Result<(), String> {
        Ok(())
    }
    fn snapshot(&self) -> GoalViewSnapshot {
        unimplemented!("契约测试不调用 goal snapshot")
    }
}

struct FakeAgentExecutor;

#[async_trait]
impl AgentExecutor for FakeAgentExecutor {
    async fn execute(&self, _params: AgentRunParams) -> AgentRunResult {
        unimplemented!("契约测试不执行 workflow")
    }
}

// ── 装配上下文构造 ────────────────────────────────────────────────────────────

/// 最小装配上下文（全部条件关闭，权限模式 Default）。
fn base_context() -> AssemblyContext {
    let (todo_tx, _todo_rx) = tokio::sync::mpsc::channel::<Vec<TodoItem>>(8);
    let (bg_event_tx, _bg_rx) = tokio::sync::mpsc::unbounded_channel::<ExecutorEvent>();
    let shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>> =
        Arc::new(RwLock::new(BTreeMap::new()));
    let llm_factory: Arc<dyn Fn(Option<&str>) -> SubagentLlmSource + Send + Sync> =
        Arc::new(|_model_alias| SubagentLlmSource::prebuilt(Box::new(FakeLlm)));
    let system_builder: SystemPromptBuilder =
        Arc::new(|_overrides: Option<&AgentOverrides>, _cwd: &str| String::new());
    let on_bg_complete: Option<OnBgCompleteFn> = None;

    AssemblyContext {
        agent_catalog: Arc::new(crate::host_ports::NoopAgentCatalog),
        cwd: "/tmp/contract-test".to_string(),
        cancel: AgentCancellationToken::new(),
        broker: Arc::new(FakeBroker),
        permission_mode: SharedPermissionMode::new(PermissionMode::Default),
        model_name: "contract-model".to_string(),
        provider_name: "contract-provider".to_string(),
        auxiliary_model: None,
        auto_classifier_model: Arc::new(tokio::sync::Mutex::new(
            Box::new(FakeModel) as Box<dyn Model>
        )),
        preload_skills: Vec::new(),
        plugin_skill_roots: Vec::new(),
        plugin_loaded: Vec::new(),
        hook_groups: Vec::new(),
        session_start_source: None,
        mcp_skill_registry: None,
        command_registry: None,
        cron_scheduler: None,
        mcp_pool: None,
        dynamic_mcp: None,
        dynamic_mcp_projection: Arc::new(parking_lot::Mutex::new(None)),
        session_id: "session-contract-test".to_string(),
        tool_search_index: Arc::new(ToolSearchIndex::new()),
        shared_tools,
        workflow_executor: None,
        workflow_middleware: None,
        event_handler: Arc::new(FakeEventHandler),
        task_manager: Arc::new(TaskManager::new()),
        bg_event_tx,
        on_bg_complete,
        langfuse_bridge: None,
        session_resources: None,
        parent_thread_id: None,
        register_runtime: None,
        deregister_runtime: None,
        child_handler_factory: None,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        system_prompt_for_sub: String::new(),
        llm_factory,
        system_builder,
        todo_tx,
        goal_controller: None,
        meta_harness_disabled: std::collections::HashSet::new(),
        agent_overrides: None,
        language: None,
    }
}

/// 装配并返回链上中间件名称序列。
fn assemble_names(ctx: &AssemblyContext) -> Vec<String> {
    let out = build_middleware_chain(&ProductionChainAssembler, ctx);
    out.chain.names().into_iter().map(String::from).collect()
}

#[test]
fn skills_face_closed_suppresses_automatic_preload_but_keeps_explicit_list() {
    let mut ctx = base_context();
    ctx.meta_harness_disabled
        .insert("SkillsMiddleware".to_string());
    let names = assemble_names(&ctx);
    assert!(!names.iter().any(|name| name == "SkillPreloadMiddleware"));

    ctx.preload_skills.push("declared-skill".to_string());
    let names = assemble_names(&ctx);
    assert!(names.iter().any(|name| name == "SkillPreloadMiddleware"));
}

fn make_hook() -> RegisteredHook {
    RegisteredHook {
        hook: HookType::Command {
            command: "echo hi".to_string(),
            shell: None,
            timeout: None,
            status_message: None,
            once: false,
            async_run: false,
            async_rewake: false,
            matcher: None,
            condition: None,
        },
        event: HookEvent::PreToolUse,
        matcher: None,
        plugin_name: "test-plugin".to_string(),
        plugin_id: "test-plugin-id".to_string(),
        plugin_root: PathBuf::from("/tmp/test-plugin"),
        plugin_data_dir: PathBuf::from("/tmp/test-plugin-data"),
        plugin_options: Default::default(),
    }
}

/// deployment pool：两个 builtin 实例（`web` / `artifact`）的假「已连接」handle，
/// 工具清单与注册表一致。用于断言 builtin 能力的三个工具面（A6）。
fn pool_with_builtin_instances() -> Arc<McpClientPool> {
    let pool = Arc::new(McpClientPool::new_empty());
    for instance in peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES {
        let tools: Vec<rmcp::model::Tool> = instance
            .tools
            .iter()
            .map(|tool| {
                serde_json::from_value(serde_json::json!({
                    "name": tool.original_name,
                    "description": "builtin tool",
                    "inputSchema": { "type": "object", "properties": {} }
                }))
                .unwrap()
            })
            .collect();
        pool.clients.write().insert(
            instance.name.to_string(),
            make_connected_handle(instance.name, tools),
        );
    }
    pool
}

fn make_connected_handle(server: &str, tools: Vec<rmcp::model::Tool>) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        name: server.to_string(),
        version: None,
        cache_version: None,
        peer: None,
        tools,
        resources: vec![],
        status: crate::mcp::ClientStatus::Connected,
        oauth_status: Default::default(),
        source: Some(peri_acp_types::plugin::ConfigSource::Builtin {
            instance: server.to_string(),
        }),
        url: None,
        skills_capable: false,
    })
}

/// 装配并返回链上中间件 collect_tools 的工具名集合。
fn assemble_tool_names(ctx: &AssemblyContext) -> Vec<String> {
    let out = build_middleware_chain(&ProductionChainAssembler, ctx);
    out.chain
        .collect_tools(&ctx.cwd)
        .into_iter()
        .map(|t| t.name().to_string())
        .collect()
}

/// workflow 上下文（可注入 broker / permission_mode 的矩阵变体，D3 对拍用）。
fn workflow_context_with_approval(
    disabled: &[&str],
    broker_present: bool,
    mode_present: bool,
) -> WorkflowAgentContext {
    let mut ctx = workflow_context_with_disabled(disabled);
    if broker_present {
        ctx.broker = Some(Arc::new(FakeBroker));
    }
    if mode_present {
        ctx.permission_mode = Some(SharedPermissionMode::new(PermissionMode::Default));
    }
    ctx
}

fn workflow_context_with_disabled(disabled: &[&str]) -> WorkflowAgentContext {
    let model_factory: peri_agent::agent::workflow::factory::WorkflowModelFactory =
        Arc::new(|_model, _max_tokens, _observer| unimplemented!("契约测试不调用"));
    let prompt_builder: peri_agent::agent::workflow::factory::WorkflowAgentPromptBuilder =
        Arc::new(|_, _, _, _| String::new());
    let fallback: peri_agent::agent::workflow::factory::WorkflowSystemPromptFallback =
        Arc::new(|_, _, _| String::new());
    let forwarder: peri_agent::session::exec::executor_helpers::ForwarderLauncherFn =
        Arc::new(|_, _, _| tokio::spawn(async {}));
    WorkflowAgentContext {
        cwd: "/tmp/contract-test".to_string(),
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        mcp_skill_registry: None,
        session_id: None,
        session_resources: None,
        execution_admission_port: None,
        compact_config: None,
        cancel: None,
        system_prompt: None,
        broker: None,
        permission_mode: None,
        frozen_date: None,
        frozen_language: None,
        progress_tx: None,
        subagent_ctx_builder: None,
        agent_prompt_builder: prompt_builder,
        model_factory,
        middleware_factory: default_workflow_middleware_factory(),
        system_prompt_fallback: fallback,
        forwarder_launcher: forwarder,
        publish_hook: None,
        langfuse_hooks: None,
        langfuse_event_handler: None,
        meta_harness_disabled: disabled.iter().map(|s| s.to_string()).collect(),
    }
}

struct WorkflowExecutionFixture {
    _directory: tempfile::TempDir,
    resources: Arc<dyn SessionResources>,
    cwd: String,
    parent_id: String,
}

impl WorkflowExecutionFixture {
    async fn new(parent_id: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let resources: Arc<dyn SessionResources> = Arc::new(
            SessionResourcesImpl::open(directory.path().join("threads.db"))
                .await
                .unwrap(),
        );
        let workspace = resources.resolve_workspace(directory.path()).await.unwrap();
        let cwd = workspace.cwd.to_string_lossy().into_owned();
        resources
            .create_session(&NewSession {
                thread_id: parent_id.to_owned(),
                created_at: peri_time::now_utc_rfc3339(),
                meta: NewSessionMeta {
                    title: None,
                    cwd: cwd.clone(),
                    parent_thread_id: None,
                    hidden: false,
                    cancel_policy: Default::default(),
                    snapshot_at_message_id: None,
                },
                binding: SessionBinding::from_workspace(&workspace),
                frozen: FrozenSnapshotBytes::new("{\"version\":1}"),
            })
            .await
            .unwrap();
        let snapshot = resources
            .load_session_work(&WorkQuery {
                session_id: parent_id.to_owned(),
                limit: 1,
            })
            .await
            .unwrap();
        let receipt = resources
            .apply_work_mutation(&WorkCommand {
                session_id: parent_id.to_owned(),
                recipient_lifecycle: snapshot.control.lifecycle,
                mutation_id: format!("fixture-workflow-owners:{parent_id}"),
                action: WorkAction::BindResourceOwners {
                    expected_revision: snapshot.state.revision,
                    connections_json:
                        "{\"workspace\":{\"url\":\"https://workspace.test.invalid/mcp\"}}".into(),
                    authorization_ref: "fixture-workflow-authorization".into(),
                },
            })
            .await
            .unwrap();
        assert_eq!(receipt.decision, WorkDecision::Accepted);
        Self {
            _directory: directory,
            resources,
            cwd,
            parent_id: parent_id.to_owned(),
        }
    }

    fn bind(&self, context: &mut WorkflowAgentContext) {
        context.cwd = self.cwd.clone();
        context.session_id = Some(self.parent_id.clone());
        context.session_resources = Some(self.resources.clone());
        context.execution_admission_port =
            Some(Arc::new(WorkflowFixtureAdmission(self.resources.clone())));
    }
}

struct WorkflowFixtureAdmission(Arc<dyn SessionResources>);

#[async_trait]
impl ExecutionAdmissionPort for WorkflowFixtureAdmission {
    async fn admit(
        &self,
        request: AdmissionRequest,
    ) -> Result<AdmissionOutcome, ExecutionAdmissionError> {
        let snapshot = request.snapshot;
        let Some(candidate) = snapshot.candidates.first() else {
            return Ok(AdmissionOutcome::Blocked {
                reason: "fixture has no durable work".into(),
            });
        };
        Ok(AdmissionOutcome::Admitted {
            admission: WorkAdmission {
                session_id: snapshot.session_id,
                admission_id: request.request_id,
                instance_id: "fixture-sdk-instance".into(),
                generation_id: "fixture-sdk-generation".into(),
                lifecycle: snapshot.control.lifecycle,
                control_generation: snapshot.control.control_generation,
                work_id: candidate.work_id.clone(),
                work_revision: candidate.work_revision,
                execution: ControlAttempt {
                    turn_id: peri_acp_types::session::TurnId::new(),
                    attempt_id: peri_acp_types::identity::AttemptId::new(),
                },
            },
        })
    }

    async fn entered(
        &self,
        request: EntryRequest,
    ) -> Result<EntryOutcome, ExecutionAdmissionError> {
        let snapshot = self
            .0
            .load_session_work(&WorkQuery {
                session_id: request.admission.session_id.clone(),
                limit: 1,
            })
            .await
            .map_err(|error| ExecutionAdmissionError::Protocol(error.to_string()))?;
        let registration = &snapshot.state.admissions[&request.admission.admission_id];
        assert_eq!(registration.admission, request.admission);
        assert_eq!(
            registration.entering_receipt.as_ref().unwrap().mutation_id,
            request.entry_evidence_id
        );
        Ok(EntryOutcome::Applied {
            receipt: EntryReceipt {
                admission: request.admission,
                entry_evidence_id: request.entry_evidence_id,
            },
        })
    }

    async fn settle(
        &self,
        request: SettlementRequest,
    ) -> Result<SettlementOutcome, ExecutionAdmissionError> {
        assert!(request.proof.validates(&request.admission));
        Ok(SettlementOutcome::Applied {
            receipt: SettlementReceipt {
                admission: request.admission,
                evidence_id: request.proof.evidence_id().to_owned(),
            },
        })
    }
}

/// 槽位 → middleware `name()` 返回值映射（与 assembly.rs 装配分支一一对应）。
fn slot_middleware_name(slot: &ChainSlot) -> &'static str {
    match slot {
        ChainSlot::DefaultSystemPrompt => "DefaultSystemPromptMiddleware",
        ChainSlot::Lang => "LangMiddleware",
        ChainSlot::AgentsMd => "AgentsMdMiddleware",
        ChainSlot::Plugin => "PluginMiddleware",
        ChainSlot::Skills => "SkillsMiddleware",
        ChainSlot::SkillPreload => "SkillPreloadMiddleware",
        ChainSlot::AtMention => "AtMentionMiddleware",
        ChainSlot::Image => "ImageMiddleware",
        ChainSlot::GitAttribution => "GitAttributionMiddleware",
        ChainSlot::Todo => "TodoMiddleware",
        ChainSlot::Hook => "HookMiddleware",
        ChainSlot::Permission => "PermissionMiddleware",
        ChainSlot::AskUser => "HumanInTheLoopMiddleware",
        ChainSlot::SubAgent => "SubAgentMiddleware",
        ChainSlot::Mcp => "McpMiddleware",
        ChainSlot::Workflow => "WorkflowMiddleware",
        ChainSlot::ToolSearch => "ToolSearch",
        ChainSlot::Goal => "GoalMiddleware",
    }
}

#[path = "assembly_test_baseline.rs"]
mod baseline;
#[path = "assembly_test_meta.rs"]
mod meta;
#[path = "assembly_test_sections.rs"]
mod sections;
#[path = "assembly_test_workflow.rs"]
mod workflow;
