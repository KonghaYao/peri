//! subagent 统一入口测试（L3 随迁 + 新增）。
//!
//! - C1 身份键契约测试（自 peri-middlewares v2_bridge.rs 随迁，断言语义不重写）
//! - spawn_subagent 用例：thread 父子链落库、frozen copy、agent_status 收尾

use std::collections::HashMap;
use std::str::FromStr;
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
use crate::thread::ThreadId;

fn build_ctx_with(agent_id: Option<AgentId>) -> V2SubagentContext {
    build_v2_subagent_context(
        None,
        Box::new(NullReactLLM),
        MiddlewareChain::new(),
        Vec::new(),
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

/// 内存 mock ThreadStore（断言 thread 父子链落库 + agent_status 收尾；
/// 消息存储真实化——resume 测试前置条件：append → load 往返可见）
struct MockThreadStore {
    threads: Arc<RwLock<Vec<ThreadMeta>>>,
    statuses: Arc<RwLock<Vec<(String, String)>>>,
    messages: Arc<RwLock<HashMap<String, Vec<BaseMessage>>>>,
    /// 一次性开关：置 true 后下一次 load_messages 返回 Err（重建失败回滚测试用）
    fail_load_messages: std::sync::atomic::AtomicBool,
}

impl MockThreadStore {
    fn new() -> Self {
        Self {
            threads: Arc::new(RwLock::new(Vec::new())),
            statuses: Arc::new(RwLock::new(Vec::new())),
            messages: Arc::new(RwLock::new(HashMap::new())),
            fail_load_messages: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

#[async_trait::async_trait]
impl crate::thread::ThreadStore for MockThreadStore {
    async fn create_thread(&self, meta: ThreadMeta) -> anyhow::Result<ThreadId> {
        self.threads.write().push(meta.clone());
        Ok(meta.id)
    }

    async fn append_messages(&self, id: &ThreadId, msgs: &[BaseMessage]) -> anyhow::Result<()> {
        self.messages
            .write()
            .entry(id.clone())
            .or_default()
            .extend(msgs.iter().cloned());
        Ok(())
    }

    async fn load_messages(&self, id: &ThreadId) -> anyhow::Result<Vec<BaseMessage>> {
        // 一次性失败开关：重建失败回滚测试使用（模拟磁盘读取失败）
        if self
            .fail_load_messages
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(anyhow::anyhow!("load failed (test injection)"));
        }
        Ok(self.messages.read().get(id).cloned().unwrap_or_default())
    }

    async fn load_meta(&self, id: &ThreadId) -> anyhow::Result<ThreadMeta> {
        self.threads
            .read()
            .iter()
            .find(|t| &t.id == id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("thread not found"))
    }

    async fn update_meta(&self, _id: &ThreadId, _meta: ThreadMeta) -> anyhow::Result<()> {
        Ok(())
    }

    async fn list_threads(&self) -> anyhow::Result<Vec<ThreadMeta>> {
        Ok(self.threads.read().clone())
    }

    async fn delete_thread(&self, _id: &ThreadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn load_context(&self, _thread_id: &ThreadId) -> anyhow::Result<Vec<BaseMessage>> {
        Ok(Vec::new())
    }

    async fn list_child_threads(&self, parent_id: &ThreadId) -> anyhow::Result<Vec<ThreadMeta>> {
        Ok(self
            .threads
            .read()
            .iter()
            .filter(|t| t.parent_thread_id.as_deref() == Some(parent_id))
            .cloned()
            .collect())
    }

    async fn list_session_threads(&self, _root_id: &ThreadId) -> anyhow::Result<Vec<ThreadMeta>> {
        Ok(self.threads.read().clone())
    }

    async fn update_thread_status(&self, id: &ThreadId, status: &str) -> anyhow::Result<()> {
        // 与真实 store（filesystem.rs:235 / sqlite_store.rs:605）对齐：
        // 1) 先 load_meta 存在性检查（不存在返回 Err，不静默 no-op）
        // 2) 参数字符串必须经 FromStr 解析，非法值返回错误，不静默 fallback
        // 3) 先 push statuses 列表，再同步更新 threads 中 ThreadMeta 的 agent_status
        //    （R-L2：resume 校验「非 active」依赖此读回路径）
        let mut meta = self.load_meta(id).await?;
        let status = AgentStatus::from_str(status)
            .map_err(|e| anyhow::anyhow!("非法 agent_status 值: {:?}", e))?;
        meta.agent_status = status;
        self.statuses
            .write()
            .push((id.clone(), status.as_str().to_string()));
        if let Some(meta) = self.threads.write().iter_mut().find(|t| &t.id == id) {
            meta.agent_status = status;
        }
        Ok(())
    }

    async fn invalidate_context_cache(&self, _thread_id: &ThreadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn delete_messages(
        &self,
        _thread_id: &ThreadId,
        _message_ids: &[crate::messages::MessageId],
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

/// 空链装配器（测试用：无中间件）
struct EmptyChainAssembler;

impl SubagentChainAssembler for EmptyChainAssembler {
    fn assemble(&self, _ctx: &SubagentChainContext) -> MiddlewareChain {
        MiddlewareChain::new()
    }
}

/// MockThreadStore：append → load 消息往返（resume 前置条件：磁盘 transcript 可读回）
#[tokio::test]
async fn test_mock_store_append_load_roundtrip() {
    let store = MockThreadStore::new();
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

/// MockThreadStore：update_thread_status 同步 ThreadMeta.agent_status（R-L2）
#[tokio::test]
async fn test_mock_store_update_status_reads_back() {
    let store = MockThreadStore::new();
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
    let store = Arc::new(MockThreadStore::new());
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
        system_prompt: None,
        error_suggest_registry: None,
        tool_registry_snapshot: None,
        tool_invocation_resolver: None,
        compact_config: None,
        context_budget: None,
        compact_llm: None,
        thread_store: Some(Arc::clone(&store) as Arc<dyn ThreadStore>),
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

    let threads = store.threads.read();
    assert_eq!(threads.len(), 1, "必须创建 1 个 child thread");
    let meta = &threads[0];
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
    let statuses = store.statuses.read();
    assert_eq!(
        statuses.last().map(|(_, s)| s.as_str()),
        Some("done"),
        "agent_status 收尾语义与迁移前一致（Completed → done）"
    );
}

/// spawn_subagent：frozen data 从父 session copy（不重新读取磁盘）
#[tokio::test]
async fn test_spawn_subagent_copies_frozen_from_parent() {
    let store = Arc::new(MockThreadStore::new());
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
        system_prompt: None,
        error_suggest_registry: None,
        tool_registry_snapshot: None,
        tool_invocation_resolver: None,
        compact_config: None,
        context_budget: None,
        compact_llm: None,
        thread_store: Some(Arc::clone(&store) as Arc<dyn ThreadStore>),
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
    let store = Arc::new(MockThreadStore::new());
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
        system_prompt: None,
        error_suggest_registry: None,
        tool_registry_snapshot: None,
        tool_invocation_resolver: None,
        compact_config: None,
        context_budget: None,
        compact_llm: None,
        thread_store: Some(Arc::clone(&store) as Arc<dyn ThreadStore>),
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

    let threads = store.threads.read();
    assert_eq!(threads.len(), 1);
    assert_eq!(
        threads[0].parent_thread_id.as_deref(),
        Some("bg-parent"),
        "parent 缺失时使用 config.parent_thread_id"
    );
    let child_frozen = &spawned.session.store().frozen;
    assert_eq!(child_frozen.claude_md.as_ref(), "bg-claude");
    assert_eq!(child_frozen.skill_summary.as_ref(), "bg-skills");
    let statuses = store.statuses.read();
    assert_eq!(
        statuses.last().map(|(_, s)| s.as_str()),
        Some("done"),
        "收尾 status 仍为 done"
    );
}

// ─── resume_subagent 用例（slice 4/5 重建 + 执行） ─────────────────────────

/// 构造最小 resume config（默认：EchoLLM / Sync / 无 task_manager / 无 cancel_token）
fn resume_config(thread_store: Arc<MockThreadStore>, thread_id: String) -> SubagentResumeConfig {
    resume_config_with(
        thread_store,
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
    thread_store: Arc<MockThreadStore>,
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
        tool_invocation_resolver: None,
        error_suggest_registry: None,
        tool_registry_snapshot: None,
        compact_config: None,
        context_budget: None,
        compact_llm: None,
        thread_store: Arc::clone(&thread_store) as Arc<dyn ThreadStore>,
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

/// 预置可恢复 thread：创建 + 置非 active（status "done"）。
/// 消息由各测试按需 append。
async fn preset_resumable_thread(
    store: &MockThreadStore,
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
    let store = Arc::new(MockThreadStore::new());
    let config = resume_config(Arc::clone(&store), "not-a-uuid".to_string());
    let err = resume_err(None, config).await;
    assert_eq!(err, "resume_subagent: invalid thread id: not-a-uuid");
}

/// resume_subagent：校验分支 1——thread 不存在 → Err
#[tokio::test]
async fn test_resume_subagent_thread_not_found() {
    let store = Arc::new(MockThreadStore::new());
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
    let store = Arc::new(MockThreadStore::new());
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
    let statuses = store.statuses.read();
    assert_eq!(
        statuses.last().map(|(_, s)| s.as_str()),
        Some("done"),
        "恢复执行完成后收尾 done"
    );
}

/// resume_subagent：校验分支 3——parent 链不匹配 → Err
#[tokio::test]
async fn test_resume_subagent_parent_mismatch_rejected() {
    let store = Arc::new(MockThreadStore::new());
    // low-1 后校验顺序为「格式 → 存在性 → status → parent 链」，
    // thread_id 必须为合法 UUID 才能走到 parent 链校验
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
    let config = resume_config(store, id.clone());
    let err = resume_err(Some(&parent), config).await;
    assert_eq!(
        err,
        format!(
            "resume_subagent: parent thread mismatch for {} \
            (该 thread 属于其他父 agent 的上下文, 当前会话无权恢复; \
            并行派发的兄弟 subagent 需由原父 agent 恢复, 或改传 subagent_type 新建)",
            id
        )
    );
}

/// resume_subagent：校验三分支全部通过 → 重建 + 完整执行（thread_id 不变）
#[tokio::test]
async fn test_resume_subagent_validation_passes_and_runs() {
    let store = Arc::new(MockThreadStore::new());
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
    let statuses = store.statuses.read();
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
    let store = Arc::new(MockThreadStore::new());
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
    let statuses = store.statuses.read();
    let seq: Vec<&str> = statuses.iter().map(|(_, s)| s.as_str()).collect();
    assert_eq!(seq, vec!["done", "active", "done"], "status 状态机完整");
}

/// 末条截断（R2-MID-1）：末条为含未配对 tool_calls 的 AI → pop——
/// 不回放进 transcript、不发给 LLM；已配对轮次保留
#[tokio::test]
async fn test_resume_subagent_pops_unpaired_tool_call_ai() {
    let store = Arc::new(MockThreadStore::new());
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
    let store = Arc::new(MockThreadStore::new());
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
    let store = Arc::new(MockThreadStore::new());
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
    let store = Arc::new(MockThreadStore::new());
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;
    store
        .append_messages(&thread_id, &[BaseMessage::human("task")])
        .await
        .unwrap();

    // 第一次恢复：执行前取消 → 循环顶 Interrupted（sync 中断）
    let token = CancellationToken::new();
    let config = resume_config_with(
        store.clone(),
        thread_id.clone(),
        Box::new(EchoLLM),
        SubagentRunMode::Sync,
        None,
        Some(token.clone()),
    );
    token.cancel();
    let spawned1 = SessionFactory::resume_subagent(None, config)
        .await
        .expect("resume 1 ok（中断不是 Err）");
    assert!(spawned1.interrupted, "cancel 前置触发中断");
    {
        let statuses = store.statuses.read();
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
    let statuses = store.statuses.read();
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
    let store = Arc::new(MockThreadStore::new());
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
            if store
                .statuses
                .read()
                .iter()
                .any(|(_, s)| s.as_str() == "active")
            {
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
    let statuses = store.statuses.read();
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
    let store = Arc::new(MockThreadStore::new());
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;

    // 注入一次性 load_messages 失败（重建阶段唯一可失败点）
    store
        .fail_load_messages
        .store(true, std::sync::atomic::Ordering::SeqCst);
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
        let statuses = store.statuses.read();
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

/// parent None 组合（low-1 缺口）：meta.parent_thread_id = Some(x) 且 parent 为
/// None → 跳过 parent 链校验（仅校验存在性），恢复成功
#[tokio::test]
async fn test_resume_subagent_parent_none_skips_chain_check() {
    let store = Arc::new(MockThreadStore::new());
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
        .expect("parent None 时跳过 parent 链校验");
    assert_eq!(spawned.child_thread_id, thread_id);
    assert!(!spawned.interrupted);
    let statuses = store.statuses.read();
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
    let store = Arc::new(MockThreadStore::new());
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
            if store.statuses.read().last().map(|(_, s)| s.as_str()) == Some("done") {
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

/// bg resume cancelled 分支：执行前取消 → bg 中断收尾写 "cancelled"
#[tokio::test]
async fn test_resume_subagent_background_mode_cancelled() {
    let store = Arc::new(MockThreadStore::new());
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;

    let task_manager = Arc::new(TaskManager::new());
    let token = CancellationToken::new();
    let config = resume_config_with(
        store.clone(),
        thread_id.clone(),
        Box::new(EchoLLM),
        SubagentRunMode::Background,
        Some(Arc::clone(&task_manager)),
        Some(token.clone()),
    );
    token.cancel();
    let spawned = SessionFactory::resume_subagent(None, config)
        .await
        .expect("bg resume ok");
    assert!(spawned.task_id.is_some(), "bg 模式必须有 task_id");
    assert!(!spawned.interrupted, "bg 模式返回值恒 false（异步收尾）");

    // bg 中断收尾：cancelled（与 sync 的 error 区分，R-M3）
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if store.statuses.read().last().map(|(_, s)| s.as_str()) == Some("cancelled") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("bg 任务应在超时前收尾 cancelled");
}

/// bg resume 注册失败回滚（review MEDIUM-1，路径 1：task_manager 缺失）：
/// spawn_background_subagent 注册前置失败 → Err 携带 thread_id + status 回滚至
/// 原值（不被 active 卡死）+ 提供 task_manager 后可再次恢复
#[tokio::test]
async fn test_resume_subagent_bg_registration_failure_rolls_back() {
    let store = Arc::new(MockThreadStore::new());
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
        let statuses = store.statuses.read();
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
            if store.statuses.read().last().map(|(_, s)| s.as_str()) == Some("done") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("bg 任务应在超时前完成");
}

/// bg resume 注册失败回滚（review MEDIUM-1，路径 2：register_with_kind 撞
/// per-kind 上限 AGENT_LIMIT=3）→ Err 携带 thread_id + status 回滚至原值
#[tokio::test]
async fn test_resume_subagent_bg_register_cap_rolls_back() {
    let store = Arc::new(MockThreadStore::new());
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;

    let task_manager = Arc::new(TaskManager::new());
    // 预注册 3 个 Agent 占位任务，占满 per-kind 上限
    for i in 0..3 {
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
            })
            .expect("占位任务注册应成功");
    }

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
        err.contains(&thread_id),
        "注册失败错误必须携带 thread_id，got: {}",
        err
    );
    assert!(
        err.contains("Failed to register"),
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
        let statuses = store.statuses.read();
        let seq: Vec<&str> = statuses.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(seq, vec!["done", "active", "done"], "active 后回滚原值");
    }
}
