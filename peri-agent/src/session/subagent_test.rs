//! subagent 统一入口测试（L3 随迁 + 新增）。
//!
//! - C1 身份键契约测试（自 peri-middlewares v2_bridge.rs 随迁，断言语义不重写）
//! - spawn_subagent 用例：thread 父子链落库、frozen copy、agent_status 收尾

use std::sync::Arc;

use parking_lot::RwLock;
use peri_acp_types::thread::AgentStatus;

use super::*;
use crate::agent::stages::NullReactLLM;
use crate::messages::ToolCallRequest;
use crate::session::subagent::{
    agent_id_from_child_thread, build_v2_subagent_context, ForkDirectiveKind, SessionFactory,
    SubagentCancelPolicy, SubagentResumeConfig, SubagentRunMode, SubagentSpawnConfig,
};
use crate::session::test_resources::mock::{MockSessionResources, ResumeLoadGate};
use crate::thread::ThreadId;
use peri_acp_types::session_resources::{
    ChildSnapshot, FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionMetaPatch,
};
use peri_acp_types::workspace::{SessionBinding, SESSION_BINDING_VERSION};

#[test]
fn subagent_failure_keeps_child_identity_and_typed_model_diagnostic() {
    let failure = crate::session::subagent::SubagentFailure::new(
        "child-123",
        "explorer",
        crate::error::AgentError::ModelError(peri_model::ModelError::http_status(
            429,
            "anthropic",
            Some("req-123"),
        )),
    );

    assert_eq!(failure.child_thread_id(), "child-123");
    assert_eq!(failure.agent_name(), "explorer");
    let diagnostic = failure.diagnostic().expect("typed model diagnostic");
    assert_eq!(diagnostic.status(), Some(429));
    assert_eq!(diagnostic.provider(), Some("anthropic"));
    assert_eq!(diagnostic.request_id(), Some("req-123"));
    assert!(failure.to_string().contains("child_thread_id: child-123"));
    assert!(!failure.to_string().contains("req-123"));
}

fn build_ctx_with(agent_id: Option<AgentId>) -> V2SubagentContext {
    build_v2_subagent_context(
        None,
        Box::new(NullReactLLM),
        MiddlewareChain::new(),
        Vec::new(),
        Arc::new(|_| true),
        None,
        "/tmp",
        CancellationToken::new(),
        None,
        None,
        None,
        None,
        None,
        None,
        agent_id,
    )
}

/// C1: 传入的外部 AgentId 必须成为 session agent_id（身份键统一）
#[test]
fn test_build_v2_subagent_context_uses_passed_agent_id() {
    let fixed =
        AgentId::from_uuid(uuid::Uuid::parse_str("00000000-0000-7000-8000-000000000001").unwrap());
    let ctx = build_ctx_with(Some(fixed));
    assert_eq!(
        ctx.context.session.agent_id, fixed,
        "StageContext.session.agent_id 必须等于传入的 AgentId"
    );
    assert_eq!(
        ctx.agent_id, ctx.context.session.agent_id,
        "V2SubagentContext.agent_id 必须与 session agent_id 一致（事件侧归属键）"
    );
}

/// C1: None 兜底路径内部生成 AgentId（测试/workflow 场景）
#[test]
fn test_build_v2_subagent_context_fallback_generates_agent_id() {
    let ctx = build_ctx_with(None);
    assert_eq!(
        ctx.agent_id, ctx.context.session.agent_id,
        "None 兜底路径两键仍须一致"
    );
}

/// C1: event_bus 与 context.runtime.event_bus 是同一 Arc（补发事件同通道）
#[test]
fn test_v2_subagent_context_exposes_event_bus() {
    let ctx = build_ctx_with(None);
    assert!(
        Arc::ptr_eq(&ctx.event_bus, &ctx.context.runtime.event_bus),
        "V2SubagentContext.event_bus 必须与 runtime.event_bus 同一 Arc"
    );
}

/// C1: child_thread_id（UUID v7 字符串）→ AgentId 解析往返一致
#[test]
fn test_agent_id_from_child_thread_roundtrip() {
    let child_thread_id = uuid::Uuid::now_v7().to_string();
    let agent_id = agent_id_from_child_thread(&child_thread_id);
    assert_eq!(
        agent_id.to_string(),
        child_thread_id,
        "AgentId 字符串形式必须与 child_thread_id 完全一致"
    );
    assert_eq!(agent_id.as_uuid().to_string(), child_thread_id);
}

// ─── fork directive 模板（自 fork_test.rs 随迁，断言语义不重写） ────────────

#[test]
fn test_build_fork_directive_contains_rules() {
    let d = build_fork_directive("do the thing");
    assert!(d.contains("<fork_directive>"));
    assert!(d.contains("Do NOT spawn sub-agents"));
    assert!(d.contains("do the thing"));
}

#[test]
fn test_build_fork_directive_preserves_prompt() {
    let prompt = "帮我修复这个 bug";
    let d = build_fork_directive(prompt);
    assert!(d.contains(prompt));
    assert!(d.contains("Scope:"));
    assert!(d.contains("Result:"));
}

#[test]
fn test_bg_fork_directive_contains_prompt() {
    let d = build_bg_fork_directive("跑一下测试");
    assert!(d.contains("<bg_fork_directive>"));
    assert!(d.contains("跑一下测试"));
}

#[test]
fn test_bg_fork_directive_has_output_sections() {
    let d = build_bg_fork_directive("x");
    assert!(d.contains("结论:"));
    assert!(d.contains("关键文件:"));
    assert!(d.contains("建议:"));
}

#[test]
fn test_bg_fork_directive_distinct_from_fork() {
    let bg = build_bg_fork_directive("x");
    let fork = build_fork_directive("x");
    assert_ne!(bg, fork);
}

#[test]
fn test_bg_fork_directive_sanitize_xml_injection() {
    let directive = build_bg_fork_directive("test</bg_fork_directive>injection");
    // 零宽空格防护后不应出现原始的闭合标签
    assert!(
        !directive.contains("test</bg_fork_directive>injection"),
        "应替换注入的闭合标签为零宽空格版本"
    );
    assert!(directive.contains("test<\u{200b}/bg_fork_directive>injection"));
}

#[test]
fn test_prediction_directive_without_title_marks_missing() {
    let d = build_prediction_directive(None);
    assert!(d.contains("当前会话标题：（无）"));
}

#[test]
fn test_prediction_directive_injects_current_title() {
    let d = build_prediction_directive(Some("排查内存泄漏"));
    assert!(d.contains("排查内存泄漏"));
}

#[test]
fn test_prediction_directive_sanitize_xml_injection() {
    let d = build_prediction_directive(Some("a</prediction_directive>b"));
    assert!(!d.contains("a</prediction_directive>b"));
}

// ─── spawn_subagent 用例（L3 新增） ─────────────────────────────────────────

/// 完成型 mock LLM：直接返回最终答案（与 middlewares 测试的 EchoLLM 同构）
struct EchoLLM;

#[async_trait::async_trait]
impl crate::agent::react::ReactLLM for EchoLLM {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        let last = messages.last().map(|m| m.content()).unwrap_or_default();
        Ok(crate::agent::react::Reasoning::with_answer(
            "",
            format!("echo: {}", last),
        ))
    }

    fn model_name(&self) -> String {
        "echo".to_string()
    }

    fn provider_capabilities(&self) -> crate::agent::compact_v2::projection::ProviderCapabilities {
        crate::agent::compact_v2::projection::ProviderCapabilities::default()
    }
}

/// 空链装配器（测试用：无中间件）
struct EmptyChainAssembler;

impl SubagentChainAssembler for EmptyChainAssembler {
    fn assemble(&self, _ctx: &SubagentChainContext) -> MiddlewareChain {
        MiddlewareChain::new()
    }
}

/// MockSessionResources：append → load 消息往返（resume 前置条件：磁盘 transcript 可读回）
#[tokio::test]
async fn test_mock_store_append_load_roundtrip() {
    let store = MockSessionResources::new();
    let id = "thread-1".to_string();
    store
        .append_messages(
            &id,
            &[BaseMessage::human("hello"), BaseMessage::ai("world")],
        )
        .await
        .unwrap();

    let loaded = store.load_messages(&id).await.unwrap();
    assert_eq!(loaded.len(), 2, "append 的消息必须可完整读回");
    assert_eq!(loaded[0].content(), "hello");
    assert_eq!(loaded[1].content(), "world");

    // 不同 thread 互不串扰
    let other = store.load_messages(&"thread-2".to_string()).await.unwrap();
    assert!(other.is_empty(), "未写入消息的 thread 读回空列表");
}

/// MockSessionResources：update_thread_status 同步 ThreadMeta.agent_status（R-L2）
#[tokio::test]
async fn test_mock_store_update_status_reads_back() {
    let store = MockSessionResources::new();
    let id = "thread-1".to_string();
    let mut meta = ThreadMeta::new("/tmp");
    meta.id = id.clone();
    store.create_thread(meta).await.unwrap();

    // 预置状态为 active（ThreadMeta 默认）
    let loaded = store.load_meta(&id).await.unwrap();
    assert!(loaded.agent_status.is_active(), "新 thread 默认 active");

    // update → load_meta 读回新状态
    store.update_thread_status(&id, "done").await.unwrap();
    let loaded = store.load_meta(&id).await.unwrap();
    assert_eq!(loaded.agent_status, AgentStatus::Done);

    // 非法状态值直接报错、不静默 fallback（与真实 store 语义一致）
    let err = store.update_thread_status(&id, "bogus").await.unwrap_err();
    assert!(
        err.to_string().contains("非法 agent_status"),
        "非法状态必须返回错误，got: {}",
        err
    );
}

/// spawn_subagent：thread 父子链正确落库（parent_thread_id 挂链、hidden、
/// cancel_policy 与意图一致、thread_id = agent_id）
#[tokio::test]
async fn test_spawn_subagent_creates_child_thread_with_parent_link() {
    let store = MockSessionResources::new();
    // child 落库的前置条件：父会话已绑定（有 binding 与 frozen）+ 本会话 root 的执行所有权。
    let lease = store.register_bound_session("parent-thread-1", "/tmp/work");
    let parent = Session::new(
        Arc::from("/tmp/work"),
        FrozenContext::builder()
            .claude_md("frozen-claude")
            .skill_summary("frozen-skills")
            .date("2026-08-05")
            .build(),
        Some("parent-thread-1".into()),
    );

    let config = SubagentSpawnConfig {
        agent_name: "test-agent".to_string(),
        prompt: "do something".to_string(),
        parent_messages: Vec::new(),
        cancel_policy: SubagentCancelPolicy::Independent,
        max_iterations: 200,
        fork_directive_kind: None,
        run_mode: SubagentRunMode::Sync,
        skill_names: Vec::new(),
        llm: Box::new(EchoLLM),
        chain_assembler: Arc::new(EmptyChainAssembler),
        tools: Vec::new(),
        tool_filter: Arc::new(|_| true),
        system_prompt: None,
        error_suggest_registry: None,
        tool_registry_snapshot: None,
        tool_invocation_resolver: None,
        compact_config: None,
        context_budget: None,
        compact_llm: None,
        session_resources: Some(
            Arc::clone(&store) as Arc<dyn peri_acp_types::session_resources::SessionResources>
        ),
        execution_owner: Some(lease),
        event_handler: None,
        bg_event_sender: None,
        task_manager: None,
        on_bg_complete: None,
        langfuse_bridge: None,
        on_subagent_start: None,
        on_subagent_stop: None,
        register_runtime: None,
        deregister_runtime: None,
        parent_agent_id: None,
        cancel_token: None,
        cwd: None,
        parent_thread_id: None,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        frozen_date: None,
    };

    let spawned = SessionFactory::spawn_subagent(Some(&parent), config)
        .await
        .expect("spawn ok");

    // 夹具另登记了父会话行：child 必须按 id 定位，不能按登记位置取。
    let threads = store.threads();
    let child = threads
        .iter()
        .find(|meta| meta.id == spawned.child_thread_id)
        .expect("必须创建 child thread");
    assert_eq!(
        threads
            .iter()
            .filter(|meta| meta.parent_thread_id.is_some())
            .count(),
        1,
        "必须创建 1 个 child thread"
    );
    let meta = child;
    assert_eq!(meta.id, spawned.child_thread_id, "thread_id = agent_id");
    assert_eq!(
        meta.parent_thread_id.as_deref(),
        Some("parent-thread-1"),
        "parent_thread_id 父子链正确挂链"
    );
    assert!(meta.hidden, "child thread 必须 hidden");
    assert_eq!(
        meta.cancel_policy,
        peri_acp_types::thread::CancelPolicy::Independent
    );
    assert_eq!(meta.title.as_deref(), Some("test-agent"));
    assert_eq!(
        spawned.session.store().thread_id.as_deref(),
        Some(spawned.child_thread_id.as_str()),
        "子 session thread_id = child_thread_id"
    );

    // agent_status 收尾（NullReactLLM 直接完成 → done）
    let statuses = store.statuses();
    assert_eq!(
        statuses.last().map(|(_, s)| s.as_str()),
        Some("done"),
        "agent_status 收尾语义与迁移前一致（Completed → done）"
    );
}

/// spawn_subagent：主 agent 场景（`store().thread_id` 恒 None）——parent id
/// 经 `SubagentHost.parent_thread_id` 注入时必须正确落库（与 spawn 落盘父子链
/// 同源；resume 已不做 parent 链校验，父子链仅作落盘记录）
#[tokio::test]
async fn test_spawn_subagent_main_agent_via_host_writes_parent_link() {
    let store = MockSessionResources::new();
    let lease = store.register_bound_session("main-context-thread", "/tmp/work");
    // 主 agent 样子：store().thread_id = None + host.parent_thread_id = ctx.thread_id
    let parent = Session::new(
        Arc::from("/tmp/work"),
        FrozenContext::builder().build(),
        None,
    );
    parent.set_subagent_host(SubagentHost {
        parent_thread_id: Some("main-context-thread".to_string()),
        ..Default::default()
    });

    let config = SubagentSpawnConfig {
        agent_name: "host-agent".to_string(),
        prompt: "do something".to_string(),
        parent_messages: Vec::new(),
        cancel_policy: SubagentCancelPolicy::Independent,
        max_iterations: 200,
        fork_directive_kind: None,
        run_mode: SubagentRunMode::Sync,
        skill_names: Vec::new(),
        llm: Box::new(EchoLLM),
        chain_assembler: Arc::new(EmptyChainAssembler),
        tools: Vec::new(),
        tool_filter: Arc::new(|_| true),
        system_prompt: None,
        error_suggest_registry: None,
        tool_registry_snapshot: None,
        tool_invocation_resolver: None,
        compact_config: None,
        context_budget: None,
        compact_llm: None,
        session_resources: Some(
            Arc::clone(&store) as Arc<dyn peri_acp_types::session_resources::SessionResources>
        ),
        execution_owner: Some(lease),
        event_handler: None,
        bg_event_sender: None,
        task_manager: None,
        on_bg_complete: None,
        langfuse_bridge: None,
        on_subagent_start: None,
        on_subagent_stop: None,
        register_runtime: None,
        deregister_runtime: None,
        parent_agent_id: None,
        cancel_token: None,
        cwd: None,
        parent_thread_id: None, // 生产路径 host 注入；cfg 为 None 时不得影响
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        frozen_date: None,
    };

    let _ = SessionFactory::spawn_subagent(Some(&parent), config)
        .await
        .expect("spawn ok");

    let threads = store.threads();
    let child = threads
        .iter()
        .find(|meta| meta.parent_thread_id.is_some())
        .expect("必须创建 child thread");
    assert_eq!(
        threads
            .iter()
            .filter(|meta| meta.parent_thread_id.is_some())
            .count(),
        1,
        "必须创建 1 个 child thread"
    );
    assert_eq!(
        child.parent_thread_id.as_deref(),
        Some("main-context-thread"),
        "parent id 经 host 注入正确落库（store().thread_id 为 None 时）"
    );
}

/// spawn_subagent：frozen data 从父 session copy（不重新读取磁盘）
#[tokio::test]
async fn test_spawn_subagent_copies_frozen_from_parent() {
    let store = MockSessionResources::new();
    let lease = store.register_bound_session("parent-thread-2", "/tmp/work");
    let parent = Session::new(
        Arc::from("/tmp/work"),
        FrozenContext::builder()
            .claude_md("frozen-claude")
            .skill_summary("frozen-skills")
            .date("2026-08-05")
            .build(),
        Some("parent-thread-2".into()),
    );

    let config = SubagentSpawnConfig {
        agent_name: "fork".to_string(),
        prompt: "continue".to_string(),
        parent_messages: vec![BaseMessage::human("hello")],
        cancel_policy: SubagentCancelPolicy::Cascade,
        max_iterations: 200,
        fork_directive_kind: Some(ForkDirectiveKind::Fork),
        run_mode: SubagentRunMode::Sync,
        skill_names: Vec::new(),
        llm: Box::new(EchoLLM),
        chain_assembler: Arc::new(EmptyChainAssembler),
        tools: Vec::new(),
        tool_filter: Arc::new(|_| true),
        system_prompt: None,
        error_suggest_registry: None,
        tool_registry_snapshot: None,
        tool_invocation_resolver: None,
        compact_config: None,
        context_budget: None,
        compact_llm: None,
        session_resources: Some(
            Arc::clone(&store) as Arc<dyn peri_acp_types::session_resources::SessionResources>
        ),
        execution_owner: Some(lease),
        event_handler: None,
        bg_event_sender: None,
        task_manager: None,
        on_bg_complete: None,
        langfuse_bridge: None,
        on_subagent_start: None,
        on_subagent_stop: None,
        register_runtime: None,
        deregister_runtime: None,
        parent_agent_id: None,
        cancel_token: None,
        cwd: None,
        parent_thread_id: None,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        frozen_date: None,
    };

    let spawned = SessionFactory::spawn_subagent(Some(&parent), config)
        .await
        .expect("spawn ok");

    // 子 session frozen copy：claude_md / skill_summary / date 与父一致
    let child_frozen = &spawned.session.store().frozen;
    assert_eq!(child_frozen.claude_md.as_ref(), "frozen-claude");
    assert_eq!(child_frozen.skill_summary.as_ref(), "frozen-skills");
    assert_eq!(child_frozen.date.as_ref(), "2026-08-05");
    assert_eq!(
        spawned.session.store().cwd.as_ref(),
        "/tmp/work",
        "cwd 从父 session 继承"
    );

    // fork 路径：parent_messages 注入 transcript（子 agent 看到父会话上下文）
    let tx = spawned.session.transcript();
    let guard = tx.read();
    let messages = guard.visible_messages();
    assert!(
        messages.iter().any(|m| m.content() == "hello"),
        "parent_messages 必须注入子 transcript"
    );
    // 且子 session transcript 绑定了持久化（thread_id 即 child_thread_id）
    assert!(
        guard.persist_tx_handle().is_some(),
        "subagent transcript 必须绑定 with_persistence"
    );
}

/// spawn_subagent：parent 为 None（/bg 命令等无 session 路径）时用 config 回退值
#[tokio::test]
async fn test_spawn_subagent_without_parent_uses_config_fallback() {
    let store = MockSessionResources::new();
    let lease = store.register_bound_session("bg-parent", "/tmp/bg");
    let config = SubagentSpawnConfig {
        agent_name: "fork".to_string(),
        prompt: "bg task".to_string(),
        parent_messages: Vec::new(),
        cancel_policy: SubagentCancelPolicy::Independent,
        max_iterations: 200,
        fork_directive_kind: Some(ForkDirectiveKind::Bg),
        run_mode: SubagentRunMode::Sync,
        skill_names: Vec::new(),
        llm: Box::new(EchoLLM),
        chain_assembler: Arc::new(EmptyChainAssembler),
        tools: Vec::new(),
        tool_filter: Arc::new(|_| true),
        system_prompt: None,
        error_suggest_registry: None,
        tool_registry_snapshot: None,
        tool_invocation_resolver: None,
        compact_config: None,
        context_budget: None,
        compact_llm: None,
        session_resources: Some(
            Arc::clone(&store) as Arc<dyn peri_acp_types::session_resources::SessionResources>
        ),
        execution_owner: Some(lease),
        event_handler: None,
        bg_event_sender: None,
        task_manager: None,
        on_bg_complete: None,
        langfuse_bridge: None,
        on_subagent_start: None,
        on_subagent_stop: None,
        register_runtime: None,
        deregister_runtime: None,
        parent_agent_id: None,
        cancel_token: None,
        cwd: Some("/tmp/bg".to_string()),
        parent_thread_id: Some("bg-parent".to_string()),
        frozen_claude_md: Some("bg-claude".to_string()),
        frozen_claude_local_md: None,
        frozen_skill_summary: Some("bg-skills".to_string()),
        frozen_date: Some("2026-08-05".to_string()),
    };

    let spawned = SessionFactory::spawn_subagent(None, config)
        .await
        .expect("spawn ok");

    let threads = store.threads();
    let child = threads
        .iter()
        .find(|meta| meta.parent_thread_id.is_some())
        .expect("必须创建 child thread");
    assert_eq!(
        threads
            .iter()
            .filter(|meta| meta.parent_thread_id.is_some())
            .count(),
        1
    );
    assert_eq!(
        child.parent_thread_id.as_deref(),
        Some("bg-parent"),
        "parent 缺失时使用 config.parent_thread_id"
    );
    let child_frozen = &spawned.session.store().frozen;
    assert_eq!(child_frozen.claude_md.as_ref(), "bg-claude");
    assert_eq!(child_frozen.skill_summary.as_ref(), "bg-skills");
    let statuses = store.statuses();
    assert_eq!(
        statuses.last().map(|(_, s)| s.as_str()),
        Some("done"),
        "收尾 status 仍为 done"
    );
}

// ─── resume_subagent 用例（slice 4/5 重建 + 执行） ─────────────────────────

/// 构造最小 resume config（默认：EchoLLM / Sync / 无 task_manager / 无 cancel_token）
fn resume_config(
    session_resources: Arc<MockSessionResources>,
    thread_id: String,
) -> SubagentResumeConfig {
    resume_config_with(
        session_resources,
        thread_id,
        Box::new(EchoLLM),
        SubagentRunMode::Sync,
        None,
        None,
    )
}

/// 构造带自定义装配/运行参数的 resume config
#[allow(clippy::too_many_arguments)]
fn resume_config_with(
    session_resources: Arc<dyn peri_acp_types::session_resources::SessionResources>,
    thread_id: String,
    llm: Box<dyn ReactLLM + Send + Sync>,
    run_mode: SubagentRunMode,
    task_manager: Option<Arc<TaskManager>>,
    cancel_token: Option<CancellationToken>,
) -> SubagentResumeConfig {
    SubagentResumeConfig {
        thread_id,
        prompt: None,
        agent_name: None,
        run_mode,
        max_iterations: 200,
        llm,
        chain_assembler: Arc::new(EmptyChainAssembler),
        tools: Vec::new(),
        tool_filter: Arc::new(|_| true),
        tool_invocation_resolver: None,
        error_suggest_registry: None,
        tool_registry_snapshot: None,
        compact_config: None,
        context_budget: None,
        compact_llm: None,
        session_resources: Arc::clone(&session_resources)
            as Arc<dyn peri_acp_types::session_resources::SessionResources>,
        event_handler: None,
        bg_event_sender: None,
        task_manager,
        on_bg_complete: None,
        langfuse_bridge: None,
        on_subagent_start: None,
        on_subagent_stop: None,
        register_runtime: None,
        deregister_runtime: None,
        parent_agent_id: None,
        cancel_token,
        cwd: None,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        frozen_date: None,
    }
}

/// 记录型 mock LLM：记录每次收到的消息列表并返回固定答案
/// （断言 resume 重放的 transcript 内容 / 末条截断行为）
#[derive(Clone)]
struct RecordingLLM {
    received: Arc<RwLock<Vec<Vec<BaseMessage>>>>,
    answer: String,
}

impl RecordingLLM {
    fn new() -> Self {
        Self {
            received: Arc::new(RwLock::new(Vec::new())),
            answer: "recorded-answer".to_string(),
        }
    }
}

#[async_trait::async_trait]
impl crate::agent::react::ReactLLM for RecordingLLM {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        self.received.write().push(messages.to_vec());
        Ok(crate::agent::react::Reasoning::with_answer(
            "",
            self.answer.clone(),
        ))
    }

    fn model_name(&self) -> String {
        "recording".to_string()
    }

    fn provider_capabilities(&self) -> crate::agent::compact_v2::projection::ProviderCapabilities {
        crate::agent::compact_v2::projection::ProviderCapabilities::default()
    }
}

/// 门控 mock LLM：首次 generate_reasoning 阻塞，直到测试侧 `release_tx.send(())`
/// 放行。oneshot 有信号缓冲——即使 send 先于 LLM 的 await 发生也不会丢失唤醒。
/// 用于让 resume 执行进入稳定挂起状态（并发互斥 / bg 注册断言）。
#[derive(Clone)]
struct GateLLM {
    /// 首次调用等待的放行接收端（首次调用 take 后为 None）
    gate: Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
    /// 已调用次数（测试侧轮询确认挂起生效）
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl GateLLM {
    fn new() -> (Self, tokio::sync::oneshot::Sender<()>) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        (
            Self {
                gate: Arc::new(std::sync::Mutex::new(Some(rx))),
                calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            },
            tx,
        )
    }

    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl crate::agent::react::ReactLLM for GateLLM {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            let rx = {
                let mut guard = self.gate.lock().expect("gate mutex poisoned");
                guard.take()
            };
            if let Some(rx) = rx {
                let _ = rx.await;
            }
        }
        Ok(crate::agent::react::Reasoning::with_answer(
            "",
            "gated-answer",
        ))
    }

    fn model_name(&self) -> String {
        "gate".to_string()
    }

    fn provider_capabilities(&self) -> crate::agent::compact_v2::projection::ProviderCapabilities {
        crate::agent::compact_v2::projection::ProviderCapabilities::default()
    }
}

struct CancelGateLLM {
    entered: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

impl CancelGateLLM {
    fn new() -> (Self, tokio::sync::oneshot::Receiver<()>) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        (
            Self {
                entered: std::sync::Mutex::new(Some(entered_tx)),
            },
            entered_rx,
        )
    }
}

#[async_trait::async_trait]
impl crate::agent::react::ReactLLM for CancelGateLLM {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        if let Some(entered) = self.entered.lock().unwrap().take() {
            let _ = entered.send(());
        }
        std::future::pending().await
    }
}

/// 预置可恢复 thread：创建 + 置非 active（status "done"）。
/// 消息由各测试按需 append。
async fn preset_resumable_thread(
    store: &MockSessionResources,
    thread_id: &str,
    parent_thread_id: Option<&str>,
) {
    let mut meta = ThreadMeta::new("/tmp/work");
    meta.id = thread_id.to_string();
    meta.parent_thread_id = parent_thread_id.map(|s| s.to_string());
    store.create_thread(meta).await.unwrap();
    store
        .update_thread_status(&thread_id.to_string(), "done")
        .await
        .unwrap();
}

/// 断言 resume_subagent 返回 Err 并取回错误文本（SubagentSpawned 无 Debug，
/// 不能直接用 unwrap_err）
async fn resume_err(parent: Option<&Arc<Session>>, config: SubagentResumeConfig) -> String {
    match SessionFactory::resume_subagent(parent, config).await {
        Err(e) => e.to_string(),
        Ok(_) => panic!("resume_subagent 应返回 Err（校验失败或重建失败）"),
    }
}

/// resume_subagent：校验分支 0——非 UUID thread_id → Err（review low-1：
/// 重建阶段 agent_id_from_child_thread 会对非 UUID panic，入口统一拒绝）
#[tokio::test]
async fn test_resume_subagent_invalid_thread_id_rejected() {
    let store = MockSessionResources::new();
    let config = resume_config(Arc::clone(&store), "not-a-uuid".to_string());
    let err = resume_err(None, config).await;
    assert_eq!(err, "resume_subagent: invalid thread id: not-a-uuid");
}

/// resume_subagent：校验分支 1——thread 不存在 → Err
#[tokio::test]
async fn test_resume_subagent_thread_not_found() {
    let store = MockSessionResources::new();
    // 合法 UUID 但未创建（low-1 后非 UUID 会先被格式校验拦截，测不到 not found）
    let id = uuid::Uuid::now_v7().to_string();
    let config = resume_config(Arc::clone(&store), id.clone());
    let err = resume_err(None, config).await;
    assert_eq!(err, format!("resume_subagent: thread not found: {}", id));
}

/// resume_subagent：校验分支 2——agent_status 为 active（未正常收尾）→ Err；
/// update_thread_status 置 done 后 load_meta 读回新状态（R-L2），恢复可通过校验
/// 并完整执行
#[tokio::test]
async fn test_resume_subagent_active_thread_rejected() {
    let store = MockSessionResources::new();
    let id = uuid::Uuid::now_v7().to_string();
    let mut meta = ThreadMeta::new("/tmp");
    meta.id = id.clone();
    meta.parent_thread_id = Some("parent-thread-1".to_string());
    store.create_thread(meta).await.unwrap();

    // 预置 active（ThreadMeta 默认）→ 拒绝
    let config = resume_config(Arc::clone(&store), id.clone());
    let err = resume_err(None, config).await;
    assert_eq!(
        err,
        format!(
            "resume_subagent: thread {} is still active \
            (thread 仍处于运行态: 可能仍在执行, 或上次异常退出未收尾; \
            若确认无执行中任务, 可改用 Agent(subagent_type: ...) 新建)",
            id
        )
    );

    // update_thread_status → load_meta 读回新状态（R-L2：mock 同步 agent_status）
    store.update_thread_status(&id, "done").await.unwrap();
    let meta = store.load_meta(&id).await.unwrap();
    assert_eq!(meta.agent_status, AgentStatus::Done);

    // 非 active 后校验通过 → 完整执行（EchoLLM 完成 → 收尾 done）
    let config = resume_config(Arc::clone(&store), id.clone());
    let spawned = SessionFactory::resume_subagent(None, config)
        .await
        .expect("非 active 后可恢复");
    assert_eq!(spawned.child_thread_id, id);
    assert!(!spawned.interrupted);
    let statuses = store.statuses();
    assert_eq!(
        statuses.last().map(|(_, s)| s.as_str()),
        Some("done"),
        "恢复执行完成后收尾 done"
    );
}

/// resume_subagent：parent 链不匹配不再拒绝（parent 链校验已移除）——
/// meta.parent_thread_id 与父 session thread_id 不一致时仍可恢复成功。
/// thread_id 即恢复凭证，不做所有权校验（曾误判拒绝兄弟 subagent 恢复）。
#[tokio::test]
async fn test_resume_subagent_parent_mismatch_not_rejected() {
    let store = MockSessionResources::new();
    let id = uuid::Uuid::now_v7().to_string();
    let mut meta = ThreadMeta::new("/tmp");
    meta.id = id.clone();
    meta.parent_thread_id = Some("other-parent".to_string()); // 与父 session 不一致
    store.create_thread(meta).await.unwrap();
    store.update_thread_status(&id, "done").await.unwrap();

    let parent = Session::new(
        Arc::from("/tmp/work"),
        FrozenContext::builder().build(),
        Some("parent-thread-2".into()),
    );
    let config = resume_config(store.clone(), id.clone());
    let spawned = SessionFactory::resume_subagent(Some(&parent), config)
        .await
        .expect("parent 链不匹配不再拒绝恢复");
    assert_eq!(spawned.child_thread_id, id, "thread_id 不变");
    let statuses = store.statuses();
    assert_eq!(
        statuses.last().map(|(_, s)| s.as_str()),
        Some("done"),
        "恢复完成后收尾 done"
    );
}

/// resume_subagent：主 agent 场景——TUI 主 session 的 `store().thread_id` 恒为
/// None（parent id 仅经 `SubagentHost.parent_thread_id` 注入），resume 成功。
/// （parent 链校验已移除，本测试保留为主 agent 路径的恢复成功回归）
#[tokio::test]
async fn test_resume_subagent_main_agent_via_host_parent_id() {
    let store = MockSessionResources::new();
    let id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &id, Some("main-context-thread")).await;

    // 主 agent 样子：store().thread_id = None + host.parent_thread_id = ctx.thread_id
    let parent = Session::new(
        Arc::from("/tmp/work"),
        FrozenContext::builder().build(),
        None,
    );
    parent.set_subagent_host(SubagentHost {
        parent_thread_id: Some("main-context-thread".to_string()),
        ..Default::default()
    });

    let config = resume_config(Arc::clone(&store), id.clone());
    let spawned = SessionFactory::resume_subagent(Some(&parent), config)
        .await
        .expect("主 agent 场景恢复应成功");
    assert_eq!(spawned.child_thread_id, id, "thread_id 不变");
    let statuses = store.statuses();
    assert_eq!(
        statuses.last().map(|(_, s)| s.as_str()),
        Some("done"),
        "恢复完成后收尾 done"
    );
}

/// resume_subagent：校验全部通过 → 重建 + 完整执行（thread_id 不变）
#[tokio::test]
async fn test_resume_subagent_validation_passes_and_runs() {
    let store = MockSessionResources::new();
    let id = uuid::Uuid::now_v7().to_string();
    let mut meta = ThreadMeta::new("/tmp");
    meta.id = id.clone();
    meta.parent_thread_id = Some("parent-thread-3".to_string());
    store.create_thread(meta).await.unwrap();
    store.update_thread_status(&id, "done").await.unwrap();

    let parent = Session::new(
        Arc::from("/tmp/work"),
        FrozenContext::builder().build(),
        Some("parent-thread-3".into()),
    );
    let config = resume_config(store.clone(), id.clone());
    let spawned = SessionFactory::resume_subagent(Some(&parent), config)
        .await
        .expect("校验通过后恢复执行");
    assert_eq!(spawned.child_thread_id, id, "thread_id 不变");
    assert!(!spawned.interrupted);
    let statuses = store.statuses();
    assert_eq!(
        statuses.last().map(|(_, s)| s.as_str()),
        Some("done"),
        "恢复完成后收尾 done"
    );
}

/// 重建正确性：transcript 完整重放（消息数/顺序）、thread_id 不变、
/// status 状态机 done → active → done、cwd 取 meta.cwd、frozen 从父 copy
#[tokio::test]
async fn test_resume_subagent_replays_transcript_and_preserves_thread_id() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    let parent_id = "parent-thread-r1";
    preset_resumable_thread(&store, &thread_id, Some(parent_id)).await;
    let original_msgs = vec![
        BaseMessage::human("task-1"),
        BaseMessage::ai("answer-1"),
        BaseMessage::human("task-2"),
    ];
    store
        .append_messages(&thread_id, &original_msgs)
        .await
        .unwrap();

    let parent = Session::new(
        Arc::from("/tmp/work"),
        FrozenContext::builder()
            .claude_md("frozen-claude")
            .skill_summary("frozen-skills")
            .date("2026-08-05")
            .build(),
        Some(parent_id.into()),
    );
    let config = resume_config(store.clone(), thread_id.clone());
    let spawned = SessionFactory::resume_subagent(Some(&parent), config)
        .await
        .expect("resume ok");

    // transcript 完整重放（顺序断言：旧消息 → 隐式 continue prompt → AI echo）
    let tx = spawned.session.transcript();
    let guard = tx.read();
    let msgs: Vec<BaseMessage> = guard.visible_messages().into_iter().cloned().collect();
    assert_eq!(msgs.len(), 5, "3 条旧消息 + prompt + echo");
    assert_eq!(msgs[0].content(), "task-1");
    assert_eq!(msgs[1].content(), "answer-1");
    assert_eq!(msgs[2].content(), "task-2");
    assert_eq!(
        msgs[3].content(),
        "Continue your previous task where you left off.",
        "prompt 缺省注入隐式 continue 常量"
    );
    assert_eq!(
        msgs[4].content(),
        "echo: Continue your previous task where you left off.",
        "EchoLLM 消费 queue 中 prompt 后回显"
    );

    // thread_id 不变（= 恢复目标，不新建）
    assert_eq!(spawned.child_thread_id, thread_id);
    assert_eq!(
        spawned.session.store().thread_id.as_deref(),
        Some(thread_id.as_str()),
        "重建 session 的 thread_id 固定为恢复目标"
    );

    // cwd 取 meta.cwd（thread 创建时固化），frozen 从父 copy（ARC-FROZEN-001）
    assert_eq!(spawned.session.store().cwd.as_ref(), "/tmp/work");
    let child_frozen = &spawned.session.store().frozen;
    assert_eq!(child_frozen.claude_md.as_ref(), "frozen-claude");
    assert_eq!(child_frozen.skill_summary.as_ref(), "frozen-skills");
    assert_eq!(child_frozen.date.as_ref(), "2026-08-05");

    // status 状态机：预置 done → 恢复置 active → 完成收尾 done
    let statuses = store.statuses();
    let seq: Vec<&str> = statuses.iter().map(|(_, s)| s.as_str()).collect();
    assert_eq!(seq, vec!["done", "active", "done"], "status 状态机完整");
}

/// 末条截断（R2-MID-1）：末条为含未配对 tool_calls 的 AI → pop——
/// 不回放进 transcript、不发给 LLM；已配对轮次保留
#[tokio::test]
async fn test_resume_subagent_pops_unpaired_tool_call_ai() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;

    // 完整配对轮次 + 末条未配对 AI（崩溃窗口残留形态：AI 已落盘、Tool 未落盘）
    let paired_ai = BaseMessage::ai_with_tool_calls(
        "paired-think",
        vec![ToolCallRequest::new(
            "t1",
            "read_file",
            serde_json::json!({}),
        )],
    );
    let unpaired_ai = BaseMessage::ai_with_tool_calls(
        "unpaired-think",
        vec![ToolCallRequest::new(
            "t2",
            "read_file",
            serde_json::json!({}),
        )],
    );
    let tool_result = BaseMessage::tool_result("t1", "ok");
    store
        .append_messages(
            &thread_id,
            &[
                BaseMessage::human("task"),
                paired_ai.clone(),
                tool_result.clone(),
                unpaired_ai.clone(),
            ],
        )
        .await
        .unwrap();

    let llm = RecordingLLM::new();
    let config = resume_config_with(
        store.clone(),
        thread_id.clone(),
        Box::new(llm.clone()),
        SubagentRunMode::Sync,
        None,
        None,
    );
    let spawned = SessionFactory::resume_subagent(None, config)
        .await
        .expect("resume ok");
    assert!(!spawned.interrupted);

    // transcript 层面：末条未配对 AI 被 pop，已配对轮次保留
    let tx = spawned.session.transcript();
    let guard = tx.read();
    let msgs: Vec<BaseMessage> = guard.visible_messages().into_iter().cloned().collect();
    assert!(
        !msgs.iter().any(|m| m.id() == unpaired_ai.id()),
        "末条含 tool_calls 的 AI 必须被 pop"
    );
    assert!(
        msgs.iter().any(|m| m.id() == paired_ai.id()),
        "已配对轮次的 AI 保留"
    );
    assert!(
        msgs.iter().any(|m| m.id() == tool_result.id()),
        "已配对轮次的 Tool 结果保留"
    );

    // LLM 视角：同样不含被 pop 消息（且收到重放 + prompt）
    let received = llm.received.read();
    assert_eq!(received.len(), 1, "单轮 LLM 调用");
    assert!(
        !received[0].iter().any(|m| m.id() == unpaired_ai.id()),
        "被 pop 的消息不得发给 LLM"
    );
    assert!(
        received[0].iter().any(|m| m.id() == paired_ai.id()),
        "已配对轮次发给 LLM（重放语义）"
    );
    assert_eq!(
        received[0].len(),
        4,
        "human + paired AI + tool result + prompt"
    );
}

/// 末条保留（R2-MID-1）：完整配对轮次（末条 = Tool）→ 不 pop，
/// 已完成轮次（含副作用）完整重放，避免 LLM 重复执行工具副作用
#[tokio::test]
async fn test_resume_subagent_keeps_complete_tool_round() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;

    let paired_ai = BaseMessage::ai_with_tool_calls(
        "paired-think",
        vec![ToolCallRequest::new(
            "t1",
            "read_file",
            serde_json::json!({}),
        )],
    );
    let tool_result = BaseMessage::tool_result("t1", "ok");
    store
        .append_messages(
            &thread_id,
            &[
                BaseMessage::human("task"),
                paired_ai.clone(),
                tool_result.clone(),
            ],
        )
        .await
        .unwrap();

    let config = resume_config(store.clone(), thread_id.clone());
    let spawned = SessionFactory::resume_subagent(None, config)
        .await
        .expect("resume ok");

    let tx = spawned.session.transcript();
    let guard = tx.read();
    let msgs: Vec<BaseMessage> = guard.visible_messages().into_iter().cloned().collect();
    assert!(
        msgs.iter().any(|m| m.id() == paired_ai.id()),
        "末条为 Tool 时不得 pop 其前的 AI（完整配对轮次保留）"
    );
    assert!(
        msgs.iter().any(|m| m.id() == tool_result.id()),
        "末条 Tool 保留"
    );
    assert_eq!(msgs.len(), 5, "human + AI + tool + prompt + echo");
}

/// prompt 两分支（显式）：resume 带新 prompt → 原样追加为 Human 指令
/// （不套 fork directive），EchoLLM 消费并回显；不注入隐式 continue
#[tokio::test]
async fn test_resume_subagent_new_prompt_appended() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;
    store
        .append_messages(&thread_id, &[BaseMessage::human("old-task")])
        .await
        .unwrap();

    let mut config = resume_config(store.clone(), thread_id.clone());
    config.prompt = Some("do the new thing".to_string());
    let spawned = SessionFactory::resume_subagent(None, config)
        .await
        .expect("resume ok");

    let tx = spawned.session.transcript();
    let guard = tx.read();
    let msgs: Vec<BaseMessage> = guard.visible_messages().into_iter().cloned().collect();
    assert!(
        msgs.iter().any(|m| m.content() == "do the new thing"),
        "新 prompt 原样追加进 transcript"
    );
    let last_ai = extract_last_ai_text(&spawned.session);
    assert!(
        last_ai.contains("do the new thing"),
        "追加指令被 LLM 消费，got: {}",
        last_ai
    );
    assert!(
        !last_ai.contains("Continue your previous task"),
        "显式 prompt 时不注入隐式 continue"
    );
}

/// 中断 → 恢复 → 完成（R-M3 实际语义）：sync 中断收尾写 "error"，
/// 恢复完成后写 "done"；thread_id 全程不变
#[tokio::test]
async fn test_resume_subagent_interrupted_then_resumed_completes() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;
    store
        .append_messages(&thread_id, &[BaseMessage::human("task")])
        .await
        .unwrap();

    // 第一次恢复：进入 Reason 后取消，覆盖 stage-local Interrupted 规范化。
    let token = CancellationToken::new();
    let (gate, entered_rx) = CancelGateLLM::new();
    let config = resume_config_with(
        store.clone(),
        thread_id.clone(),
        Box::new(gate),
        SubagentRunMode::Sync,
        None,
        Some(token.clone()),
    );
    let first_resume =
        tokio::spawn(async move { SessionFactory::resume_subagent(None, config).await });
    entered_rx.await.expect("sync subagent 必须进入 Reason");
    token.cancel();
    let spawned1 = first_resume
        .await
        .expect("sync resume task 不得 panic")
        .expect("resume 1 ok（中断不是 Err）");
    assert!(spawned1.interrupted, "Reason 内 cancel 必须是 Interrupted");
    {
        let statuses = store.statuses();
        assert_eq!(
            statuses.last().map(|(_, s)| s.as_str()),
            Some("error"),
            "R-M3：sync 中断收尾写 error"
        );
    }

    // 第二次恢复（换正常 token）：完成 → done
    let config = resume_config(store.clone(), thread_id.clone());
    let spawned2 = SessionFactory::resume_subagent(None, config)
        .await
        .expect("resume 2 ok");
    assert!(!spawned2.interrupted);
    assert_eq!(spawned2.child_thread_id, thread_id, "thread_id 不变");
    let statuses = store.statuses();
    assert_eq!(
        statuses.last().map(|(_, s)| s.as_str()),
        Some("done"),
        "恢复完成后收尾 done"
    );
}

/// 并发 resume 互斥（R-M1）：两个任务同时 resume 同一 thread_id，
/// 仅一个成功进入执行（第二个在锁内看到 active 被拒）
#[tokio::test]
async fn test_resume_subagent_concurrent_resume_mutex() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;

    let (gate, release_tx) = GateLLM::new();
    let store1 = Arc::clone(&store);
    let thread_id1 = thread_id.clone();
    let gate1 = gate.clone();
    let t1 = tokio::spawn(async move {
        let config = resume_config_with(
            store1.clone(),
            thread_id1,
            Box::new(gate1.clone()),
            SubagentRunMode::Sync,
            None,
            None,
        );
        SessionFactory::resume_subagent(None, config).await
    });

    // 等待 t1 完成「校验 → 置 active」（锁内置位；随后 t1 进入执行并被 gate 挂起）
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if store.statuses().iter().any(|(_, s)| s.as_str() == "active") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("t1 应完成置 active");

    // 第二个并发 resume：锁内看到 active → 拒绝
    let store2 = Arc::clone(&store);
    let thread_id2 = thread_id.clone();
    let t2 = tokio::spawn(async move {
        let config = resume_config(store2.clone(), thread_id2);
        SessionFactory::resume_subagent(None, config).await
    });
    let t2_res = t2.await.expect("t2 task ok");
    match t2_res {
        Err(e) => assert!(
            e.to_string().contains("still active"),
            "并发 resume 必须被 active 拒绝，got: {}",
            e
        ),
        Ok(_) => panic!("第二个并发 resume 不得进入执行（R-M1 互斥）"),
    }

    // 放行 t1 → 完成（oneshot 有缓冲，send 先于 LLM await 也不丢）
    let _ = release_tx.send(());
    let spawned = t1.await.expect("t1 task ok").expect("t1 resume ok");
    assert!(!spawned.interrupted);
    assert_eq!(spawned.child_thread_id, thread_id);
    let statuses = store.statuses();
    assert_eq!(
        statuses.last().map(|(_, s)| s.as_str()),
        Some("done"),
        "胜出方正常收尾 done"
    );
}

/// 重建失败回滚（R-M1）：load_messages 失败 → status 回滚至原值
/// （不被 active 卡死，可再次恢复）
#[tokio::test]
async fn test_resume_subagent_rolls_back_status_on_rebuild_failure() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;

    // 注入「认领后 history 装载」失败：第 1 次快照读取是认领前的绑定分类，
    // 第 2 次才是 own history 装载。
    store.fail_snapshot_load_at(2);
    let config = resume_config(store.clone(), thread_id.clone());
    let err = resume_err(None, config).await;
    assert!(
        err.contains("failed to load messages"),
        "重建失败错误必须带原因，got: {}",
        err
    );

    // status 回滚至原值（done），未被 active 卡死
    let meta = store.load_meta(&thread_id).await.unwrap();
    assert_eq!(
        meta.agent_status,
        AgentStatus::Done,
        "重建失败必须回滚 status 至原值"
    );
    {
        let statuses = store.statuses();
        let seq: Vec<&str> = statuses.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(seq, vec!["done", "active", "done"], "active 后回滚原值");
    }

    // 回滚后可再次恢复成功（不残留互斥态）
    let config = resume_config(store.clone(), thread_id.clone());
    let spawned = SessionFactory::resume_subagent(None, config)
        .await
        .expect("回滚后可再次恢复");
    assert_eq!(spawned.child_thread_id, thread_id);
}

/// parent None 组合：meta.parent_thread_id = Some(x) 且调用方无 parent session
/// （/bg 命令等路径）→ 恢复成功（无 parent 链校验，仅存在性 + status 校验）
#[tokio::test]
async fn test_resume_subagent_parent_none_skips_chain_check() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    // meta 声明了父链，但调用方无 parent session（/bg 命令等路径）
    preset_resumable_thread(&store, &thread_id, Some("orphan-parent")).await;
    store
        .append_messages(&thread_id, &[BaseMessage::human("task")])
        .await
        .unwrap();

    let config = resume_config(store.clone(), thread_id.clone());
    let spawned = SessionFactory::resume_subagent(None, config)
        .await
        .expect("parent None 时无 parent 链校验，恢复成功");
    assert_eq!(spawned.child_thread_id, thread_id);
    assert!(!spawned.interrupted);
    let statuses = store.statuses();
    assert_eq!(
        statuses.last().map(|(_, s)| s.as_str()),
        Some("done"),
        "恢复完成后收尾 done"
    );
}

/// bg resume（slice 5）：Background 模式 → 新 task_id（bg- 前缀）、
/// TaskManager 注册 Running、放行后完成收尾 done + registry 移除
#[tokio::test]
async fn test_resume_subagent_background_mode_done() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;
    store
        .append_messages(&thread_id, &[BaseMessage::human("task")])
        .await
        .unwrap();

    let task_manager = Arc::new(TaskManager::new());
    let (gate, release_tx) = GateLLM::new();
    let config = resume_config_with(
        store.clone(),
        thread_id.clone(),
        Box::new(gate.clone()),
        SubagentRunMode::Background,
        Some(Arc::clone(&task_manager)),
        None,
    );
    let spawned = SessionFactory::resume_subagent(None, config)
        .await
        .expect("bg resume ok");

    // 新 task_id：与 thread_id 分离、bg- 前缀
    let task_id = spawned.task_id.expect("bg 模式必须有 task_id");
    assert!(task_id.starts_with("bg-"), "task_id 格式 bg-{{uuid}}");
    assert_ne!(task_id, thread_id, "task_id 与 thread_id 分离");

    // TaskManager 注册（gate 挂起 LLM，任务仍 Running）
    let tasks = task_manager.list_tasks();
    assert!(
        tasks.iter().any(
            |(id, status, _)| id == &task_id && matches!(status, BackgroundTaskStatus::Running)
        ),
        "bg resume 必须注册 TaskManager，tasks: {:?}",
        tasks
    );

    // 放行 → 完成：status done + registry 移除（complete 后仅保留 Running）。
    // 先确认 LLM 已被调用（挂起生效）再放行，保证任务确实进入执行。
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while gate.calls() == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("bg 任务应进入 LLM 调用");
    let _ = release_tx.send(());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if store.statuses().last().map(|(_, s)| s.as_str()) == Some("done") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("bg 任务应在超时前完成");
    assert_eq!(
        task_manager.active_count(),
        0,
        "bg 完成后 registry 移除任务"
    );
}

/// bg resume cancelled 分支：Reason 内取消 → bg 中断收尾写 "cancelled"
#[tokio::test]
async fn test_resume_subagent_background_mode_cancelled() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;

    let task_manager = Arc::new(TaskManager::new());
    let token = CancellationToken::new();
    let (gate, entered_rx) = CancelGateLLM::new();
    let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
    let completed_tx = Arc::new(std::sync::Mutex::new(Some(completed_tx)));
    let mut config = resume_config_with(
        store.clone(),
        thread_id.clone(),
        Box::new(gate),
        SubagentRunMode::Background,
        Some(Arc::clone(&task_manager)),
        Some(token.clone()),
    );
    config.on_bg_complete = Some(Arc::new(move |result, _kind| {
        if let Some(completed) = completed_tx.lock().unwrap().take() {
            let _ = completed.send(result.clone());
        }
    }));
    let spawned = SessionFactory::resume_subagent(None, config)
        .await
        .expect("bg resume ok");
    assert!(spawned.task_id.is_some(), "bg 模式必须有 task_id");
    assert!(!spawned.interrupted, "bg 模式返回值恒 false（异步收尾）");
    entered_rx
        .await
        .expect("background subagent 必须进入 Reason");
    token.cancel();

    let completed = completed_rx.await.expect("on_bg_complete 必须收到终态");
    assert!(!completed.success, "取消后 background completion 不得成功");
    assert!(
        completed.output.contains("interrupted"),
        "background completion 必须呈现 interrupted: {}",
        completed.output
    );
    assert_eq!(
        store.statuses().last().map(|(_, s)| s.as_str()),
        Some("cancelled"),
        "bg 中断收尾必须写 cancelled"
    );
}

/// bg resume 注册失败回滚（review MEDIUM-1，路径 1：task_manager 缺失）：
/// spawn_background_subagent 注册前置失败 → Err 携带 thread_id + status 回滚至
/// 原值（不被 active 卡死）+ 提供 task_manager 后可再次恢复
#[tokio::test]
async fn test_resume_subagent_bg_registration_failure_rolls_back() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;

    // 不传 task_manager → 注册失败（任务未执行）
    let config = resume_config_with(
        store.clone(),
        thread_id.clone(),
        Box::new(EchoLLM),
        SubagentRunMode::Background,
        None,
        None,
    );
    let err = resume_err(None, config).await;
    assert!(
        err.contains(&thread_id),
        "注册失败错误必须携带 thread_id，got: {}",
        err
    );
    assert!(
        err.contains("no task manager configured"),
        "错误须带注册失败原因，got: {}",
        err
    );

    // status 回滚至原值（done），未被 active 卡死
    let meta = store.load_meta(&thread_id).await.unwrap();
    assert_eq!(
        meta.agent_status,
        AgentStatus::Done,
        "注册失败必须回滚 status 至原值"
    );
    {
        let statuses = store.statuses();
        let seq: Vec<&str> = statuses.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(seq, vec!["done", "active", "done"], "active 后回滚原值");
    }

    // 回滚后可再次恢复（提供 task_manager）→ bg 正常完成收尾 done
    let task_manager = Arc::new(TaskManager::new());
    let config = resume_config_with(
        store.clone(),
        thread_id.clone(),
        Box::new(EchoLLM),
        SubagentRunMode::Background,
        Some(Arc::clone(&task_manager)),
        None,
    );
    let spawned = SessionFactory::resume_subagent(None, config)
        .await
        .expect("回滚后可再次恢复");
    assert_eq!(spawned.child_thread_id, thread_id);
    assert!(spawned.task_id.is_some(), "bg 模式必须有 task_id");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if store.statuses().last().map(|(_, s)| s.as_str()) == Some("done") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("bg 任务应在超时前完成");
}

/// bg resume 不受 Agent 类并发上限阻挡（原 AGENT_LIMIT=3 已移除）：已有 5 个
/// Agent 任务在跑时，第 6 个后台恢复仍必须成功、正常收尾 done，且既有条目不被
/// 丢弃/改写。
#[tokio::test]
async fn test_resume_subagent_bg_beyond_previous_agent_cap() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;
    store
        .append_messages(&thread_id, &[BaseMessage::human("task")])
        .await
        .unwrap();

    let task_manager = Arc::new(TaskManager::new());
    // 5 个在跑 Agent 任务（>3，覆盖已取消的上限）
    for i in 0..5 {
        task_manager
            .register_with_kind(BackgroundTask {
                id: format!("placeholder-{}", i),
                agent_name: "placeholder".to_string(),
                prompt_summary: "placeholder".to_string(),
                status: BackgroundTaskStatus::Running,
                started_at: std::time::Instant::now(),
                chrono_started_at: chrono::Utc::now(),
                kind: BgTaskKind::Agent,
                cancel_handle: BgCancelHandle::Kill(None),
                cancel_token: None,
                pid: None,
                output_preview: None,
                agent_inbox: None,
            })
            .expect("占位任务注册应成功（Agent 类不限额）");
    }
    assert_eq!(task_manager.active_count(), 5);

    let config = resume_config_with(
        store.clone(),
        thread_id.clone(),
        Box::new(EchoLLM),
        SubagentRunMode::Background,
        Some(Arc::clone(&task_manager)),
        None,
    );
    let spawned = SessionFactory::resume_subagent(None, config)
        .await
        .expect("超过 3 个在跑任务时 bg 恢复不得被拒绝");
    assert_eq!(spawned.child_thread_id, thread_id);
    let task_id = spawned.task_id.expect("bg 模式必须有 task_id");
    assert!(task_id.starts_with("bg-"), "task_id 格式 bg-{{uuid}}");

    // 任务完成：status done；registry 收敛回 5 个占位任务（无泄漏/无丢弃）
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if store.statuses().last().map(|(_, s)| s.as_str()) == Some("done") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("bg 任务应在超时前完成");
    assert_eq!(
        task_manager.active_count(),
        5,
        "完成的任务移除后仅剩占位任务"
    );
    assert!(
        task_manager
            .list_tasks()
            .iter()
            .all(|(id, _, _)| id.starts_with("placeholder-")),
        "既有占位条目不得被改写"
    );
}

/// bg resume 执行权前置失败：session execution scope 已关闭时，恢复在 claim
/// （写 active）之前被拒——不产生执行，也不留下 active 脏状态。
///
/// 历史：本用例原以 `register_with_kind` 撞 per-kind 上限（AGENT_LIMIT=3）制造
/// 注册失败；Agent 类后台任务取消并发上限后（放开并行委派），该类注册失败不复
/// 存在，改用 scope 关闭作为可确定复现的执行前失败。执行前失败回滚（active →
/// 原值）的完整契约由 `test_resume_subagent_bg_registration_failure_rolls_back`
/// 覆盖（task_manager 缺失路径）。
#[tokio::test]
async fn test_resume_subagent_bg_scope_closed_rejected_before_claim() {
    use peri_acp_types::tasks::TaskManager as _;
    use peri_acp_types::tasks::TaskShutdownReport;

    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;

    let task_manager = Arc::new(TaskManager::new());
    // 关闭 session execution scope：恢复侧外部执行权申请被拒
    assert_eq!(
        task_manager.shutdown().await,
        TaskShutdownReport::Complete,
        "无在跑任务时空闲关闭应为 Complete"
    );

    let config = resume_config_with(
        store.clone(),
        thread_id.clone(),
        Box::new(EchoLLM),
        SubagentRunMode::Background,
        Some(Arc::clone(&task_manager)),
        None,
    );
    let err = resume_err(None, config).await;
    assert!(
        err.contains("closing"),
        "错误须带执行前失败原因，got: {}",
        err
    );

    // claim 之前失败：thread 从未被写成 active，状态保持原值
    let meta = store.load_meta(&thread_id).await.unwrap();
    assert_eq!(
        meta.agent_status,
        AgentStatus::Done,
        "执行前被拒不得改写 thread 状态"
    );
    {
        let statuses = store.statuses();
        let seq: Vec<&str> = statuses.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(seq, vec!["done"], "claim 之前失败不写 active");
    }
    assert_eq!(task_manager.active_count(), 0, "不得留下后台任务条目");
}

#[path = "subagent/provenance_test.rs"]
mod provenance_tests;
async fn dispatch_resume_fixture(
    config: SubagentResumeConfig,
    cancel: &CancellationToken,
) -> crate::error::AgentResult<crate::agent::stages::tool_dispatch::DispatchOutcome> {
    use crate::agent::react::{Reasoning, ToolCall};
    use crate::agent::stages::{tool_dispatch::dispatch_tools, StageContext};
    use crate::session::{queue::MessageQueue, transcript::MessageTranscript, turn::TurnContext};
    use crate::tools::{BaseTool, ToolContext};

    // A thin BaseTool adapter enters the real SessionFactory; cancellation is
    // driven by the production dispatch pipeline, not a copied select in the test.
    struct ResumeTool(std::sync::Mutex<Option<SubagentResumeConfig>>);
    #[async_trait::async_trait]
    impl BaseTool for ResumeTool {
        fn name(&self) -> &str {
            "ResumeFixture"
        }
        fn description(&self) -> &str {
            "resume cancellation boundary fixture"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            let config = { self.0.lock().unwrap().take() }.expect("one invocation");
            let resumed = SessionFactory::resume_subagent(None, config).await?;
            Ok(resumed.child_thread_id)
        }
    }
    let turn = TurnContext::new(Arc::from("/tmp/work"), Arc::new(cancel.clone()));
    let transcript = Arc::new(parking_lot::RwLock::new(MessageTranscript::new()));
    let ctx = StageContext::new(turn, transcript, MessageQueue::new());
    ctx.runtime.tools.write().insert(
        "ResumeFixture".into(),
        Arc::new(ResumeTool(std::sync::Mutex::new(Some(config)))),
    );
    let reasoning = Reasoning::with_tools(
        "",
        vec![ToolCall::new(
            "resume-call",
            "ResumeFixture",
            serde_json::json!({}),
        )],
    );
    let catalog = ctx
        .runtime
        .tool_catalog
        .pin_working_tools(&ctx.runtime.tools.read())
        .unwrap();
    dispatch_tools(&ctx, &reasoning, &catalog, cancel).await
}

async fn cancel_resume_at_gate(
    config: SubagentResumeConfig,
    cancel: &CancellationToken,
    entered_rx: tokio::sync::oneshot::Receiver<()>,
    store: &MockSessionResources,
    thread_id: &ThreadId,
) {
    let mut dispatch = Box::pin(dispatch_resume_fixture(config, cancel));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::select! {
            entered = entered_rx => entered.expect("resume phase gate must be reached"),
            result = &mut dispatch => panic!("dispatch ended before the phase gate: {}", result.is_ok()),
        }
    })
    .await
    .expect("resume must reach the phase gate");
    assert_eq!(
        store.load_meta(thread_id).await.unwrap().agent_status,
        AgentStatus::Active
    );
    cancel.cancel();
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(2), dispatch)
            .await
            .unwrap(),
        Err(crate::error::AgentError::Interrupted)
    ));
}

async fn wait_for_resume_status(
    store: &MockSessionResources,
    thread_id: &ThreadId,
    status: AgentStatus,
) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let changed = store.status_changed.notified();
            if store.load_meta(thread_id).await.unwrap().agent_status == status {
                break;
            }
            changed.await;
        }
    })
    .await
    .expect("cancelled resume must finalize persisted status before the thread can resume");
}

#[tokio::test]
async fn test_resume_load_cancelled_by_dispatch_restores_previous_status() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (_release_tx, release_rx) = tokio::sync::oneshot::channel();
    let load_dropped = Arc::new(AtomicBool::new(false));
    *store.load_gate.lock().unwrap() = Some(ResumeLoadGate {
        entered: entered_tx,
        release: release_rx,
        dropped: Arc::clone(&load_dropped),
    });
    let llm = RecordingLLM::new();
    let received = Arc::clone(&llm.received);
    let cancel = CancellationToken::new();
    let config = resume_config_with(
        store.clone(),
        thread_id.clone(),
        Box::new(llm),
        SubagentRunMode::Sync,
        None,
        Some(cancel.clone()),
    );
    cancel_resume_at_gate(config, &cancel, entered_rx, &store, &thread_id).await;
    assert!(
        load_dropped.load(Ordering::SeqCst),
        "real dispatch must drop the pending load future"
    );
    assert!(
        received.read().is_empty(),
        "execution must not start during preparation"
    );
    wait_for_resume_status(&store, &thread_id, AgentStatus::Done).await;
    let resumed =
        SessionFactory::resume_subagent(None, resume_config(store.clone(), thread_id.clone()))
            .await
            .expect("the same thread must remain resumable");
    assert_eq!(resumed.child_thread_id, thread_id);
}

#[tokio::test]
async fn test_resume_active_write_cancelled_by_dispatch_finishes_before_rollback() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let write_dropped = Arc::new(AtomicBool::new(false));
    *store.active_write_gate.lock().unwrap() = Some(ResumeLoadGate {
        entered: entered_tx,
        release: release_rx,
        dropped: Arc::clone(&write_dropped),
    });
    let llm = RecordingLLM::new();
    let received = Arc::clone(&llm.received);
    let cancel = CancellationToken::new();
    let config = resume_config_with(
        store.clone(),
        thread_id.clone(),
        Box::new(llm),
        SubagentRunMode::Sync,
        None,
        Some(cancel.clone()),
    );
    cancel_resume_at_gate(config, &cancel, entered_rx, &store, &thread_id).await;
    assert!(
        !write_dropped.load(Ordering::SeqCst),
        "a committed write must retain its owner until it returns"
    );
    assert!(received.read().is_empty());
    assert_eq!(
        store.load_meta(&thread_id).await.unwrap().agent_status,
        AgentStatus::Active
    );
    release_tx.send(()).unwrap();
    wait_for_resume_status(&store, &thread_id, AgentStatus::Done).await;
    assert!(write_dropped.load(Ordering::SeqCst));
    assert!(
        received.read().is_empty(),
        "cancelled preparation must never execute after the write resumes"
    );
    let statuses: Vec<_> = store
        .statuses()
        .iter()
        .map(|(_, status)| status.clone())
        .collect();
    assert_eq!(
        statuses,
        ["done", "active", "done"],
        "rollback follows completion of the active write exactly once"
    );
    let resumed =
        SessionFactory::resume_subagent(None, resume_config(store.clone(), thread_id.clone()))
            .await
            .unwrap();
    assert_eq!(resumed.child_thread_id, thread_id);
}

#[tokio::test]
async fn test_resume_cancelled_during_assembly_never_starts_execution() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    struct CancelDuringAssembly {
        cancel: CancellationToken,
        assembled: Arc<AtomicBool>,
    }
    impl SubagentChainAssembler for CancelDuringAssembly {
        fn assemble(&self, _ctx: &SubagentChainContext) -> MiddlewareChain {
            // build_subagent_session_v2 has already constructed the Session and
            // attached its persisted history when it invokes this real callback.
            self.assembled.store(true, Ordering::SeqCst);
            self.cancel.cancel();
            MiddlewareChain::new()
        }
    }
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;
    let cancel = CancellationToken::new();
    let assembled = Arc::new(AtomicBool::new(false));
    let starts = Arc::new(AtomicUsize::new(0));
    let llm = RecordingLLM::new();
    let received = Arc::clone(&llm.received);
    let mut config = resume_config_with(
        store.clone(),
        thread_id.clone(),
        Box::new(llm),
        SubagentRunMode::Sync,
        None,
        Some(cancel.clone()),
    );
    config.chain_assembler = Arc::new(CancelDuringAssembly {
        cancel: cancel.clone(),
        assembled: assembled.clone(),
    });
    let starts_hook = starts.clone();
    config.on_subagent_start = Some(Arc::new(move |_, _| {
        starts_hook.fetch_add(1, Ordering::SeqCst);
    }));
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        SessionFactory::resume_subagent(None, config),
    )
    .await
    .unwrap()
    .expect("cancelled sync preparation retains the interrupted result contract");
    assert!(result.interrupted);
    assert_eq!(result.child_thread_id, thread_id);
    assert!(result.task_id.is_none());
    assert_eq!(
        result.session.store().thread_id.as_deref(),
        Some(thread_id.as_str())
    );
    assert!(
        assembled.load(Ordering::SeqCst),
        "the cancellation must happen after constructing the real session"
    );
    assert_eq!(
        starts.load(Ordering::SeqCst),
        0,
        "cancelled preparation must not emit lifecycle Start"
    );
    assert!(received.read().is_empty());
    wait_for_resume_status(&store, &thread_id, AgentStatus::Done).await;
    let resumed =
        SessionFactory::resume_subagent(None, resume_config(store.clone(), thread_id.clone()))
            .await
            .unwrap();
    assert_eq!(resumed.child_thread_id, thread_id);
}

#[tokio::test]
async fn test_resume_running_cancelled_by_dispatch_finalizes_claim() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    struct GatedLLM(std::sync::Mutex<Option<ResumeLoadGate>>);
    #[async_trait::async_trait]
    impl crate::agent::react::ReactLLM for GatedLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn crate::tools::BaseTool],
            _streaming: Option<crate::agent::react::StreamingContext>,
        ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
            let gate = { self.0.lock().unwrap().take() }.expect("one LLM call");
            gate.wait().await;
            Ok(crate::agent::react::Reasoning::with_answer(
                "",
                "unreachable",
            ))
        }
        fn model_name(&self) -> String {
            "gated-resume".into()
        }
        fn provider_capabilities(
            &self,
        ) -> crate::agent::compact_v2::projection::ProviderCapabilities {
            crate::agent::compact_v2::projection::ProviderCapabilities::default()
        }
    }
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (_release_tx, release_rx) = tokio::sync::oneshot::channel();
    let llm_dropped = Arc::new(AtomicBool::new(false));
    let starts = Arc::new(AtomicUsize::new(0));
    let stops = Arc::new(AtomicUsize::new(0));
    let cancel = CancellationToken::new();
    let mut config = resume_config_with(
        store.clone(),
        thread_id.clone(),
        Box::new(GatedLLM(std::sync::Mutex::new(Some(ResumeLoadGate {
            entered: entered_tx,
            release: release_rx,
            dropped: llm_dropped.clone(),
        })))),
        SubagentRunMode::Sync,
        None,
        Some(cancel.clone()),
    );
    let starts_hook = starts.clone();
    config.on_subagent_start = Some(Arc::new(move |_, _| {
        starts_hook.fetch_add(1, Ordering::SeqCst);
    }));
    let stops_hook = stops.clone();
    config.on_subagent_stop = Some(Arc::new(move |_, _, _, _| {
        stops_hook.fetch_add(1, Ordering::SeqCst);
    }));
    cancel_resume_at_gate(config, &cancel, entered_rx, &store, &thread_id).await;
    assert!(llm_dropped.load(Ordering::SeqCst));
    assert_eq!(
        starts.load(Ordering::SeqCst),
        1,
        "this cancellation happens after lifecycle Start"
    );
    wait_for_resume_status(&store, &thread_id, AgentStatus::Cancelled).await;
    assert_eq!(
        stops.load(Ordering::SeqCst),
        0,
        "claim cleanup must not invent or duplicate the normal Stop hook"
    );
    let resumed =
        SessionFactory::resume_subagent(None, resume_config(store.clone(), thread_id.clone()))
            .await
            .unwrap();
    assert_eq!(resumed.child_thread_id, thread_id);
}

#[tokio::test]
async fn test_resume_precancelled_background_still_registers_and_completes() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;
    let task_manager = Arc::new(TaskManager::new());
    let token = CancellationToken::new();
    token.cancel();
    let llm = RecordingLLM::new();
    let received = llm.received.clone();
    let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
    let completed_tx = std::sync::Mutex::new(Some(completed_tx));
    let mut config = resume_config_with(
        store.clone(),
        thread_id.clone(),
        Box::new(llm),
        SubagentRunMode::Background,
        Some(task_manager),
        Some(token),
    );
    config.on_bg_complete = Some(Arc::new(move |result, _kind| {
        if let Some(completed) = completed_tx.lock().unwrap().take() {
            let _ = completed.send(result.clone());
        }
    }));
    let spawned = SessionFactory::resume_subagent(None, config)
        .await
        .expect("background cancellation retains real registration and completion");
    assert_eq!(spawned.child_thread_id, thread_id);
    let task_id = spawned
        .task_id
        .expect("background must return a registered task");
    assert!(
        !spawned.interrupted,
        "background interruption is reported asynchronously"
    );
    let completed = tokio::time::timeout(std::time::Duration::from_secs(2), completed_rx)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completed.task_id, task_id);
    assert_eq!(
        completed.child_thread_id.as_deref(),
        Some(thread_id.as_str())
    );
    assert!(!completed.success);
    assert!(received.read().is_empty());
    assert_eq!(
        store.load_meta(&thread_id).await.unwrap().agent_status,
        AgentStatus::Cancelled
    );
    let resumed =
        SessionFactory::resume_subagent(None, resume_config(store.clone(), thread_id.clone()))
            .await
            .unwrap();
    assert_eq!(resumed.child_thread_id, thread_id);
}

/// Newly added provenance reads remain inside the existing real dispatch claim.
#[tokio::test]
async fn test_resume_provenance_read_cancelled_by_dispatch_restores_previous_status() {
    for inherited_read in [true, false] {
        use std::sync::atomic::{AtomicBool, Ordering};
        let store = MockSessionResources::new();
        let thread_id = uuid::Uuid::now_v7().to_string();
        preset_resumable_thread(&store, &thread_id, None).await;
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (_release_tx, release_rx) = tokio::sync::oneshot::channel();
        let load_dropped = Arc::new(AtomicBool::new(false));
        let gate = if inherited_read {
            &store.inherited_load_gate
        } else {
            &store.flags_load_gate
        };
        *gate.lock().unwrap() = Some(ResumeLoadGate {
            entered: entered_tx,
            release: release_rx,
            dropped: Arc::clone(&load_dropped),
        });
        let llm = RecordingLLM::new();
        let received = Arc::clone(&llm.received);
        let cancel = CancellationToken::new();
        let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let start_counter = starts.clone();
        let mut config = resume_config_with(
            store.clone(),
            thread_id.clone(),
            Box::new(llm),
            SubagentRunMode::Sync,
            None,
            Some(cancel.clone()),
        );
        config.on_subagent_start = Some(Arc::new(move |_, _| {
            start_counter.fetch_add(1, Ordering::SeqCst);
        }));
        cancel_resume_at_gate(config, &cancel, entered_rx, &store, &thread_id).await;
        assert!(
            load_dropped.load(Ordering::SeqCst),
            "real dispatch must drop the pending load future"
        );
        assert!(
            received.read().is_empty(),
            "execution must not start during preparation"
        );
        assert_eq!(starts.load(Ordering::SeqCst), 0);
        wait_for_resume_status(&store, &thread_id, AgentStatus::Done).await;
        let resumed =
            SessionFactory::resume_subagent(None, resume_config(store.clone(), thread_id.clone()))
                .await
                .expect("the same thread must remain resumable");
        assert_eq!(resumed.child_thread_id, thread_id);
    }
}

/// Invalid inherited/own overlap must reject reconstruction without stranding Active.
#[tokio::test]
async fn test_resume_provenance_overlap_rolls_back_claim_before_retry() {
    use peri_acp_types::store::{InheritedContext, PersistedPayload};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;
    let message = BaseMessage::human("same id must not belong to both regions");
    store
        .append_messages(&thread_id, std::slice::from_ref(&message))
        .await
        .unwrap();
    store
        .store_inherited_context(
            &thread_id,
            &InheritedContext {
                payloads: vec![PersistedPayload::Message(message)],
                flags: Default::default(),
            },
        )
        .await
        .unwrap();
    let starts = Arc::new(AtomicUsize::new(0));
    let counter = starts.clone();
    let mut config = resume_config(store.clone(), thread_id.clone());
    config.on_subagent_start = Some(Arc::new(move |_, _| {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    let error = resume_err(None, config).await;
    assert!(
        error.contains("inherited context overlaps child own history"),
        "{error}"
    );
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    assert_eq!(
        store.load_meta(&thread_id).await.unwrap().agent_status,
        AgentStatus::Done
    );
    assert_eq!(
        store
            .statuses()
            .iter()
            .map(|(_, status)| status.as_str())
            .collect::<Vec<_>>(),
        ["done", "active", "done"]
    );
    // Repair the corrupt fixture and exercise the same thread's real resume.
    store.clear_inherited();
    let resumed =
        SessionFactory::resume_subagent(None, resume_config(store.clone(), thread_id.clone()))
            .await
            .unwrap();
    assert_eq!(resumed.child_thread_id, thread_id);
}

#[tokio::test]
async fn test_bound_subagent_resume_requires_same_root_but_allows_siblings() {
    // 真门面：绑定、父子链、执行所有权都由真实实现提供（不可用 mock 自证）。
    let repo = crate::session::test_resources::git_repository();
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn peri_acp_types::session_resources::SessionResources> = Arc::new(
        peri_resources::sessions::SessionResourcesImpl::open(db.path().join("sessions.db"))
            .await
            .unwrap(),
    );
    let workspace = store.resolve_workspace(repo.path()).await.unwrap();
    let cwd = workspace.cwd.to_str().unwrap();
    let root_a = workspace.cwd.to_string_lossy().into_owned();
    let (root_a_id, owner_a) = create_bound_root(&store, &workspace, None).await;
    let (root_b_id, owner_b) = create_bound_root(&store, &workspace, None).await;
    // child 的 frozen 必须是 root 已保存快照的逐字节副本（门面在 save_child 内校验）。
    let frozen = FrozenSnapshotBytes::new("{\"version\":1,\"root\":true}");
    let child_id = save_bound_child(&store, &workspace, &root_a_id, &frozen, &owner_a).await;
    let sibling_id = save_bound_child(&store, &workspace, &root_a_id, &frozen, &owner_a).await;
    let caller_b = Session::new(
        Arc::from(cwd),
        FrozenContext::builder().build(),
        Some(root_b_id),
    );
    let mut config = resume_config(MockSessionResources::new(), child_id.clone());
    config.session_resources = Arc::clone(&store);
    let error = resume_err(Some(&caller_b), config).await;
    assert!(error.contains("another root session"), "{error}");
    assert_eq!(
        store
            .load_session_meta(&child_id)
            .await
            .unwrap()
            .agent_status,
        AgentStatus::Done
    );
    let sibling = Session::new(
        Arc::from(cwd),
        FrozenContext::builder().build(),
        Some(sibling_id),
    );
    let mut config = resume_config(MockSessionResources::new(), child_id.clone());
    config.session_resources = Arc::clone(&store);
    SessionFactory::resume_subagent(Some(&sibling), config)
        .await
        .expect("same-root siblings can resume");
    assert_eq!(
        store
            .load_session_meta(&child_id)
            .await
            .unwrap()
            .agent_status,
        AgentStatus::Done
    );
    // 同根兄弟会话位于同一 root 执行代际（本机只有一条 root owner 事实）。
    let _ = root_a;
    owner_a
        .as_ref()
        .expect("root A 仍持有执行权")
        .mark_clean()
        .await
        .unwrap();
    owner_b
        .as_ref()
        .expect("root B 仍持有执行权")
        .mark_clean()
        .await
        .unwrap();
}

/// 真门面：创建一条已绑定的根会话（返回身份与执行所有权）。
async fn create_bound_root(
    store: &Arc<dyn peri_acp_types::session_resources::SessionResources>,
    workspace: &peri_acp_types::workspace::ResolvedWorkspace,
    frozen: Option<FrozenSnapshotBytes>,
) -> (
    ThreadId,
    Option<Arc<dyn peri_acp_types::workspace::SessionExecutionLease>>,
) {
    let session = bound_session(store, workspace, frozen, None);
    let thread_id = session.thread_id.clone();
    let lease = store.create_session(&session).await.unwrap();
    (thread_id, Some(lease))
}

/// 真门面：在 root 的执行所有权下保存一条 child 会话（继承区为空）。
async fn save_bound_child(
    store: &Arc<dyn peri_acp_types::session_resources::SessionResources>,
    workspace: &peri_acp_types::workspace::ResolvedWorkspace,
    root: &ThreadId,
    frozen: &FrozenSnapshotBytes,
    lease: &Option<Arc<dyn peri_acp_types::workspace::SessionExecutionLease>>,
) -> ThreadId {
    let target = bound_session(store, workspace, Some(frozen.clone()), Some(root.clone()));
    let child_id = target.thread_id.clone();
    store
        .save_child(
            &ChildSnapshot {
                target,
                parent_id: root.clone(),
                root_id: root.clone(),
                inherited: Default::default(),
            },
            lease.as_ref().expect("root 执行所有权"),
        )
        .await
        .unwrap();
    store
        .update_session_meta(
            &child_id,
            &SessionMetaPatch {
                status: Some(AgentStatus::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    child_id
}

fn bound_session(
    _store: &Arc<dyn peri_acp_types::session_resources::SessionResources>,
    workspace: &peri_acp_types::workspace::ResolvedWorkspace,
    frozen: Option<FrozenSnapshotBytes>,
    parent: Option<ThreadId>,
) -> NewSession {
    NewSession {
        thread_id: uuid::Uuid::now_v7().to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        meta: NewSessionMeta {
            title: Some("bound fixture".to_owned()),
            cwd: workspace.cwd.to_string_lossy().into_owned(),
            parent_thread_id: parent,
            hidden: false,
            cancel_policy: Default::default(),
            snapshot_at_message_id: None,
        },
        binding: SessionBinding {
            schema_version: SESSION_BINDING_VERSION,
            revision: 1,
            project_id: workspace.project_id,
            workspace_id: workspace.workspace_id,
            cwd_relative_to_workspace: workspace.relative_cwd.clone(),
        },
        frozen: frozen.unwrap_or_else(|| FrozenSnapshotBytes::new("{\"version\":1,\"root\":true}")),
    }
}

#[derive(Clone, Copy)]
enum TailOutcome {
    Completed,
    ModelError,
    Cancelled,
}

struct TailChunkLLM(TailOutcome);

#[async_trait::async_trait]
impl ReactLLM for TailChunkLLM {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        let streaming = streaming.expect("子 agent 必须提供流式事件入口");
        streaming
            .event_bus
            .emit_render(crate::agent::events_v2::RenderEvent::TextChunk {
                turn_id: streaming.turn_id,
                agent_id: streaming.agent_id,
                message_id: peri_acp_types::messages::MessageId::new(),
                chunk: "tail-chunk".into(),
            });
        // 最后一条增量与返回发生在同一 poll，不能靠 sleep 让 forwarder 先运行。
        match self.0 {
            TailOutcome::Completed => Ok(crate::agent::react::Reasoning::with_answer("", "done")),
            TailOutcome::ModelError => Err(crate::error::AgentError::ModelError(
                peri_model::ModelError::http_status(429, "fixture", Some("private-request")),
            )),
            TailOutcome::Cancelled => {
                streaming.cancel.cancel();
                Err(crate::error::AgentError::Interrupted)
            }
        }
    }

    fn model_name(&self) -> String {
        "tail-fixture".into()
    }

    fn provider_capabilities(&self) -> crate::agent::compact_v2::projection::ProviderCapabilities {
        crate::agent::compact_v2::projection::ProviderCapabilities::default()
    }
}

fn tail_spawn_config(
    store: Arc<MockSessionResources>,
    outcome: TailOutcome,
) -> SubagentSpawnConfig {
    // child 落库要求父会话已绑定（`save_child` 继承绑定与 frozen），并需要本会话 root 的
    // 执行所有权；夹具显式构造这两项前置条件，不靠替身默认值。
    let lease = store.register_bound_session("tail-parent", "/tmp/tail-fixture");
    SubagentSpawnConfig {
        agent_name: "tail-agent".into(),
        prompt: "task".into(),
        parent_messages: Vec::new(),
        cancel_policy: SubagentCancelPolicy::Independent,
        max_iterations: 10,
        fork_directive_kind: None,
        run_mode: SubagentRunMode::Sync,
        skill_names: Vec::new(),
        llm: Box::new(TailChunkLLM(outcome)),
        chain_assembler: Arc::new(EmptyChainAssembler),
        tools: Vec::new(),
        tool_filter: Arc::new(|_| true),
        system_prompt: None,
        error_suggest_registry: None,
        tool_registry_snapshot: None,
        tool_invocation_resolver: None,
        compact_config: None,
        context_budget: None,
        compact_llm: None,
        session_resources: Some(store),
        execution_owner: Some(lease),
        event_handler: None,
        bg_event_sender: None,
        task_manager: None,
        on_bg_complete: None,
        langfuse_bridge: None,
        on_subagent_start: None,
        on_subagent_stop: None,
        register_runtime: None,
        deregister_runtime: None,
        parent_agent_id: Some(AgentId::new()),
        cancel_token: None,
        cwd: Some("/tmp/tail-fixture".into()),
        parent_thread_id: Some("tail-parent".into()),
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        frozen_date: None,
    }
}

struct TailPanicBridge(Arc<std::sync::atomic::AtomicUsize>);

impl crate::agent::LangfuseBridgeLike for TailPanicBridge {
    fn process_render_event(&self, event: &crate::agent::events_v2::RenderEvent) {
        if matches!(
            event,
            crate::agent::events_v2::RenderEvent::TextChunk { .. }
        ) {
            panic!("tail forwarding fixture panic");
        }
    }
    fn process_observe_event(&self, event: &crate::agent::events_v2::ObserveEvent) {
        if matches!(
            event,
            crate::agent::events_v2::ObserveEvent::SubagentStop { .. }
        ) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

async fn assert_background_tail_completion(outcome: TailOutcome, panic_forwarder: bool) {
    use crate::agent::events::{BackgroundTaskResult, ExecutorEvent};
    let store = MockSessionResources::new();
    let manager = Arc::new(TaskManager::new());
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
    let event_rx = Arc::new(parking_lot::Mutex::new(event_rx));
    let (complete_tx, complete_rx) = tokio::sync::oneshot::channel();
    let complete_tx = parking_lot::Mutex::new(Some(complete_tx));
    let callback_rx = event_rx.clone();
    let callback_manager = manager.clone();
    let bridge_stops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut config = tail_spawn_config(store.clone(), outcome);
    config.run_mode = SubagentRunMode::Background;
    config.task_manager = Some(manager.clone());
    config.bg_event_sender = Some(event_tx);
    if panic_forwarder {
        config.langfuse_bridge = Some(Arc::new(TailPanicBridge(bridge_stops.clone())));
    }
    config.on_bg_complete = Some(Arc::new(move |result: &BackgroundTaskResult, _| {
        let mut events = Vec::new();
        while let Ok(event) = callback_rx.lock().try_recv() {
            events.push(event);
        }
        complete_tx
            .lock()
            .take()
            .unwrap()
            .send((result.clone(), events, callback_manager.active_count()))
            .unwrap();
    }));
    let spawned = SessionFactory::spawn_subagent(None, config)
        .await
        .expect("后台注册成功");
    let (result, events, active_at_callback) =
        tokio::time::timeout(std::time::Duration::from_secs(5), complete_rx)
            .await
            .expect("关闭 producer 后必须完成排空，不能被 guard 持有死锁")
            .unwrap();
    assert_eq!(active_at_callback, 1, "通知先于 TaskManager 终态");
    assert_eq!(manager.active_count(), 0, "真实执行收尾后任务结束");
    assert_eq!(
        result.child_thread_id.as_deref(),
        Some(spawned.child_thread_id.as_str())
    );
    let stop_index = events
        .iter()
        .position(|e| matches!(e, ExecutorEvent::SubagentStopped { .. }))
        .expect("必须配对 Stopped");
    let completed_index = events
        .iter()
        .position(|e| matches!(e, ExecutorEvent::BackgroundTaskCompleted(_)));
    let expected_success = matches!(outcome, TailOutcome::Completed) && !panic_forwarder;
    let expected_completed = expected_success || matches!(outcome, TailOutcome::Cancelled);
    if expected_completed {
        assert_eq!(
            completed_index,
            Some(stop_index + 1),
            "成功/协作取消时 Stopped 后紧接 Completed"
        );
    } else {
        assert!(
            completed_index.is_none(),
            "模型/转发错误通过 callback 和 TaskManager 交付，不合成 Completed 事件"
        );
    }
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, ExecutorEvent::SubagentStopped { .. }))
            .count(),
        1
    );
    assert_eq!(
        completed_index.unwrap_or(stop_index),
        events.len() - 1,
        "callback 已收到所有事件，终态后不能有尾事件"
    );
    assert!(
        event_rx.lock().try_recv().is_err(),
        "callback 后没有滞留事件"
    );
    if !panic_forwarder {
        let chunk_index = events.iter().position(|e| matches!(e, ExecutorEvent::TextChunk { chunk, source_agent_id, .. } if chunk == "tail-chunk" && source_agent_id.as_deref() == Some(spawned.child_thread_id.as_str()))).expect("callback 必须收到末条增量");
        assert!(chunk_index < stop_index, "末条增量必须先于 Stopped");
    }
    assert_eq!(result.success, expected_success);
    assert!(
        matches!(&events[stop_index], ExecutorEvent::SubagentStopped { is_error, .. } if *is_error != expected_success)
    );
    if let Some(completed_index) = completed_index {
        assert!(
            matches!(&events[completed_index], ExecutorEvent::BackgroundTaskCompleted(completed) if completed.success == expected_success)
        );
    }
    let expected_status = match outcome {
        TailOutcome::Cancelled => "cancelled",
        TailOutcome::Completed if !panic_forwarder => "done",
        _ => "error",
    };
    assert_eq!(
        store.statuses().last().map(|(_, status)| status.as_str()),
        Some(expected_status)
    );
    if matches!(outcome, TailOutcome::ModelError) {
        assert!(
            result.subagent_failure.is_some(),
            "模型错误必须保留 typed failure"
        );
        assert!(result.output.contains("429"));
    }
    if panic_forwarder {
        assert_eq!(
            bridge_stops.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "失败的 forwarder 不得提前发布遥测 Stop"
        );
    }
    if panic_forwarder && matches!(outcome, TailOutcome::Completed) {
        assert!(result.output.contains("An internal error occurred"));
    }
}

/// [回归测试] background 曾丢弃 forwarder handle，callback 可在最后增量之前执行。
#[tokio::test(flavor = "current_thread")]
async fn test_spawn_subagent_background_drains_tail_before_completion() {
    assert_background_tail_completion(TailOutcome::Completed, false).await;
}

/// [回归测试] 模型失败也要排空尾事件；错误终态通过 callback 交付，保持 wire 契约。
#[tokio::test(flavor = "current_thread")]
async fn test_spawn_subagent_background_error_drains_tail_before_completion() {
    assert_background_tail_completion(TailOutcome::ModelError, false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn test_spawn_subagent_background_cancel_drains_tail_before_completion() {
    assert_background_tail_completion(TailOutcome::Cancelled, false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn test_spawn_subagent_background_forwarder_panic_is_failure() {
    assert_background_tail_completion(TailOutcome::Completed, true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn test_spawn_subagent_background_forwarder_panic_preserves_model_failure() {
    assert_background_tail_completion(TailOutcome::ModelError, true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn test_spawn_subagent_sync_forwarder_panic_is_failure() {
    use crate::agent::events::{ExecutorEvent, FnEventHandler};
    let store = MockSessionResources::new();
    let events = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let capture = events.clone();
    let bridge_stops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut config = tail_spawn_config(store.clone(), TailOutcome::Completed);
    config.langfuse_bridge = Some(Arc::new(TailPanicBridge(bridge_stops.clone())));
    config.event_handler = Some(Arc::new(FnEventHandler(move |event| {
        capture.lock().push(event);
    })));
    let error = match SessionFactory::spawn_subagent(None, config).await {
        Ok(_) => panic!("forwarder panic 不得返回成功"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("An internal error occurred"));
    assert!(error.to_string().contains("child_thread_id:"));
    assert_eq!(bridge_stops.load(std::sync::atomic::Ordering::SeqCst), 0);
    let events = events.lock();
    assert!(matches!(
        events.last(),
        Some(ExecutorEvent::SubagentStopped { is_error: true, .. })
    ));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ExecutorEvent::SubagentStopped { .. }))
            .count(),
        1
    );
    assert_eq!(
        store.statuses().last().map(|(_, status)| status.as_str()),
        Some("error")
    );
}

struct TerminalPanicBridge;

impl crate::agent::LangfuseBridgeLike for TerminalPanicBridge {
    fn process_render_event(&self, _event: &crate::agent::events_v2::RenderEvent) {}
    fn process_observe_event(&self, event: &crate::agent::events_v2::ObserveEvent) {
        if matches!(
            event,
            crate::agent::events_v2::ObserveEvent::SubagentStop { .. }
        ) {
            panic!("terminal forwarding fixture panic");
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn test_spawn_subagent_terminal_bridge_panic_is_failure() {
    use crate::agent::events::{ExecutorEvent, FnEventHandler};
    let store = MockSessionResources::new();
    let events = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let capture = events.clone();
    let mut config = tail_spawn_config(store.clone(), TailOutcome::Completed);
    config.langfuse_bridge = Some(Arc::new(TerminalPanicBridge));
    config.event_handler = Some(Arc::new(FnEventHandler(move |event| {
        capture.lock().push(event);
    })));
    let error = match SessionFactory::spawn_subagent(None, config).await {
        Ok(_) => panic!("terminal bridge panic 不得返回成功"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("An internal error occurred"));
    assert!(error.to_string().contains("child_thread_id:"));
    assert!(matches!(
        events.lock().last(),
        Some(ExecutorEvent::SubagentStopped { is_error: true, .. })
    ));
    assert_eq!(
        store.statuses().last().map(|(_, status)| status.as_str()),
        Some("error")
    );
}
