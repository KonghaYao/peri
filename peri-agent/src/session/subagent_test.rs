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

#[path = "subagent/test_helpers.rs"]
mod test_helpers;
use test_helpers::AdmittedSessionFactory;

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
    assert!(failure.to_string().contains("req-123"));
}

fn build_ctx_with(agent_id: Option<AgentId>) -> V2SubagentContext {
    build_v2_subagent_context(
        None,
        Box::new(NullReactLLM),
        Arc::new(MiddlewareChain::new()),
        Vec::new(),
        Arc::new(|_| true),
        None,
        "/tmp",
        CancellationToken::new(),
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

/// 完成型 mock 模型：回显最后一条消息（Model 形态，经生产 bridge 装配）
#[derive(Clone)]
struct EchoLLM;

impl EchoLLM {
    async fn respond(
        &self,
        request: peri_model::ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        let _ = cancellation;
        use crate::session::test_resources::mock::model as fixture;
        let messages = fixture::base_messages(&request);
        let last = messages.last().map(|m| m.content()).unwrap_or_default();
        fixture::text_events(format!("echo: {}", last))
    }
}
crate::fixture_model_impl!(EchoLLM);

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
    let mut meta = ThreadMeta::new_at("/tmp", peri_time::now_wall());
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
    // child 落库的前置条件：父会话已绑定（有 binding 与 frozen）。
    store.register_bound_session("parent-thread-1", "/tmp/work");
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
        llm: crate::session::test_resources::mock::model::fixture_source(
            std::sync::Arc::new(EchoLLM),
            "fixture-scripted",
        ),
        chain_assembler: Arc::new(EmptyChainAssembler),
        tools: Vec::new(),
        tool_filter: Arc::new(|_| true),
        system_prompt: None,
        tool_invocation_resolver: None,
        compact_config: None,
        context_budget: None,
        compact_llm: None,
        session_resources: Some(
            Arc::clone(&store) as Arc<dyn peri_acp_types::session_resources::SessionResources>
        ),
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
        parent_invocation_id: None,
        cancel_token: None,
        cwd: None,
        parent_thread_id: None,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        frozen_date: None,
    };

    let spawned = AdmittedSessionFactory::spawn_subagent(Some(&parent), config)
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
    store.register_bound_session("main-context-thread", "/tmp/work");
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
        llm: crate::session::test_resources::mock::model::fixture_source(
            std::sync::Arc::new(EchoLLM),
            "fixture-scripted",
        ),
        chain_assembler: Arc::new(EmptyChainAssembler),
        tools: Vec::new(),
        tool_filter: Arc::new(|_| true),
        system_prompt: None,
        tool_invocation_resolver: None,
        compact_config: None,
        context_budget: None,
        compact_llm: None,
        session_resources: Some(
            Arc::clone(&store) as Arc<dyn peri_acp_types::session_resources::SessionResources>
        ),
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
        parent_invocation_id: None,
        cancel_token: None,
        cwd: None,
        parent_thread_id: None, // 生产路径 host 注入；cfg 为 None 时不得影响
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        frozen_date: None,
    };

    let _ = AdmittedSessionFactory::spawn_subagent(Some(&parent), config)
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
    store.register_bound_session("parent-thread-2", "/tmp/work");
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
        llm: crate::session::test_resources::mock::model::fixture_source(
            std::sync::Arc::new(EchoLLM),
            "fixture-scripted",
        ),
        chain_assembler: Arc::new(EmptyChainAssembler),
        tools: Vec::new(),
        tool_filter: Arc::new(|_| true),
        system_prompt: None,
        tool_invocation_resolver: None,
        compact_config: None,
        context_budget: None,
        compact_llm: None,
        session_resources: Some(
            Arc::clone(&store) as Arc<dyn peri_acp_types::session_resources::SessionResources>
        ),
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
        parent_invocation_id: None,
        cancel_token: None,
        cwd: None,
        parent_thread_id: None,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        frozen_date: None,
    };

    let spawned = AdmittedSessionFactory::spawn_subagent(Some(&parent), config)
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
    store.register_bound_session("bg-parent", "/tmp/bg");
    let config = SubagentSpawnConfig {
        agent_name: "fork".to_string(),
        prompt: "bg task".to_string(),
        parent_messages: Vec::new(),
        cancel_policy: SubagentCancelPolicy::Independent,
        max_iterations: 200,
        fork_directive_kind: Some(ForkDirectiveKind::Bg),
        run_mode: SubagentRunMode::Sync,
        skill_names: Vec::new(),
        llm: crate::session::test_resources::mock::model::fixture_source(
            std::sync::Arc::new(EchoLLM),
            "fixture-scripted",
        ),
        chain_assembler: Arc::new(EmptyChainAssembler),
        tools: Vec::new(),
        tool_filter: Arc::new(|_| true),
        system_prompt: None,
        tool_invocation_resolver: None,
        compact_config: None,
        context_budget: None,
        compact_llm: None,
        session_resources: Some(
            Arc::clone(&store) as Arc<dyn peri_acp_types::session_resources::SessionResources>
        ),
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
        parent_invocation_id: None,
        cancel_token: None,
        cwd: Some("/tmp/bg".to_string()),
        parent_thread_id: Some("bg-parent".to_string()),
        frozen_claude_md: Some("bg-claude".to_string()),
        frozen_claude_local_md: None,
        frozen_skill_summary: Some("bg-skills".to_string()),
        frozen_date: Some("2026-08-05".to_string()),
    };

    let spawned = AdmittedSessionFactory::spawn_subagent(None, config)
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
        crate::session::test_resources::mock::model::fixture_source(
            std::sync::Arc::new(EchoLLM),
            "fixture-scripted",
        ),
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
    llm: SubagentLlmSource,
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
        parent_invocation_id: None,
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

impl RecordingLLM {
    async fn respond(
        &self,
        request: peri_model::ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        let _ = cancellation;
        use crate::session::test_resources::mock::model as fixture;
        self.received.write().push(fixture::base_messages(&request));
        fixture::text_events(self.answer.clone())
    }
}
crate::fixture_model_impl!(RecordingLLM);

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

impl GateLLM {
    async fn respond(
        &self,
        _request: peri_model::ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        let _ = cancellation;
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            let rx = {
                let mut guard = self.gate.lock().expect("gate mutex poisoned");
                guard.take()
            };
            if let Some(rx) = rx {
                let _ = rx.await;
            }
        }
        crate::session::test_resources::mock::model::text_events("gated-answer")
    }
}
crate::fixture_model_impl!(GateLLM);

#[derive(Clone)]
struct CancelGateLLM {
    entered: Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
}

impl CancelGateLLM {
    fn new() -> (Self, tokio::sync::oneshot::Receiver<()>) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        (
            Self {
                entered: Arc::new(std::sync::Mutex::new(Some(entered_tx))),
            },
            entered_rx,
        )
    }

    async fn respond(
        &self,
        _request: peri_model::ModelRequest,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        if let Some(entered) = self.entered.lock().unwrap().take() {
            let _ = entered.send(());
        }
        // 挂起直到 bridge 因取消 abort 该流（future drop）。
        std::future::pending::<Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>>>().await
    }
}
crate::fixture_model_impl!(CancelGateLLM);

/// 预置可恢复 thread：创建 + 置非 active（status "done"）。
/// 消息由各测试按需 append。
async fn preset_resumable_thread(
    store: &MockSessionResources,
    thread_id: &str,
    parent_thread_id: Option<&str>,
) {
    let mut meta = ThreadMeta::new_at("/tmp/work", peri_time::now_wall());
    meta.id = thread_id.to_string();
    meta.parent_thread_id = parent_thread_id.map(|s| s.to_string());
    store.create_resumable_thread(meta).await.unwrap();
    store
        .update_thread_status(&thread_id.to_string(), "done")
        .await
        .unwrap();
}

/// 断言 resume_subagent 返回 Err 并取回错误文本（SubagentSpawned 无 Debug，
/// 不能直接用 unwrap_err）
async fn resume_err(parent: Option<&Arc<Session>>, config: SubagentResumeConfig) -> String {
    match AdmittedSessionFactory::resume_subagent(parent, config).await {
        Err(e) => e.to_string(),
        Ok(_) => panic!("resume_subagent 应返回 Err（校验失败或重建失败）"),
    }
}

#[path = "subagent/bound_and_tail_test.rs"]
mod bound_and_tail_cases;
/// resume_subagent：校验分支 0——非 UUID thread_id → Err（review low-1：
/// 重建阶段 agent_id_from_child_thread 会对非 UUID panic，入口统一拒绝）
#[path = "subagent/resume_cases_test.rs"]
mod resume_cases;
#[path = "subagent/resume_dispatch_test.rs"]
mod resume_dispatch_cases;

#[path = "subagent/close_lifecycle_test.rs"]
mod close_lifecycle_cases;

#[path = "subagent/child_wire_capture_test.rs"]
mod child_wire_capture_test;
#[path = "subagent/provenance_test.rs"]
mod provenance_tests;
async fn create_bound_root(
    store: &Arc<dyn peri_acp_types::session_resources::SessionResources>,
    workspace: &peri_acp_types::workspace::ResolvedWorkspace,
    frozen: Option<FrozenSnapshotBytes>,
) -> ThreadId {
    let session = bound_session(store, workspace, frozen, None);
    let thread_id = session.thread_id.clone();
    store.create_session(&session).await.unwrap();
    thread_id
}

/// 真门面：保存一条已绑定的 child 会话（继承区为空）。
async fn save_bound_child(
    store: &Arc<dyn peri_acp_types::session_resources::SessionResources>,
    workspace: &peri_acp_types::workspace::ResolvedWorkspace,
    root: &ThreadId,
    frozen: &FrozenSnapshotBytes,
) -> ThreadId {
    let target = bound_session(store, workspace, Some(frozen.clone()), Some(root.clone()));
    let child_id = target.thread_id.clone();
    store
        .save_child(&ChildSnapshot {
            target,
            parent_id: root.clone(),
            root_id: root.clone(),
            inherited: Default::default(),
        })
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
    crate::session::test_resources::mock::history::seed_saved_fixture_runtime(
        store.clone(),
        &child_id,
        frozen.clone(),
        root,
    )
    .await;
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
            workspace_id: workspace.execution_registration_id,
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

#[derive(Clone)]
struct TailChunkLLM(TailOutcome);

impl TailChunkLLM {
    async fn respond(
        &self,
        _request: peri_model::ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        use crate::session::test_resources::mock::model as fixture;
        match self.0 {
            TailOutcome::Completed => vec![
                fixture::lead_chunk("tail-chunk"),
                fixture::completed_event("done"),
            ],
            TailOutcome::ModelError => fixture::error_events(
                vec![fixture::lead_chunk("tail-chunk")],
                peri_model::ModelError::http_status(429, "fixture", Some("private-request")),
            ),
            TailOutcome::Cancelled => {
                // 不直接 cancel 本 token：ModelStream 会在 poll 事件之前先报取消，
                // 已产出的末条增量会被丢弃。以 Err(cancelled) 终止同样表达取消，
                // 且保证 `TextDelta` 先被 bridge 读到（tail drain 语义）。
                let _ = &cancellation;
                fixture::error_events(
                    vec![fixture::lead_chunk("tail-chunk")],
                    peri_model::ModelError::cancelled(),
                )
            }
        }
    }
}
crate::fixture_model_impl!(TailChunkLLM);
