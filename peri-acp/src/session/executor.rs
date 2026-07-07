//! Shared prompt execution logic.
//!
//! Provides [`run_session_loop`] which encapsulates the common agent execution
//! pipeline used by both TUI (via [`TransportEventSink`]) and stdio (via
//! [`StdioEventSink`]) paths.
//!
//! Compact 由 v2 `stages/compact.rs`（`run_react_loop` 在每轮开头调
//! `compact_v2::run_compact`）统一处理，不再需要外层 loop + resubmit，
//! 也不再经过 CompactMiddleware。
//!
//! # 文件结构（EXECUTOR-SPLIT 选项 B）
//!
//! 本文件是 orchestrator，仅保留：
//! - 共享类型：`PromptStopReason` / `PromptResult` / `FrozenSessionData`
//!   / `PromptExecutionContext` / `TurnConfig` / `BuildAgentRequest` / `ExecOutcome`
//! - 入口：`run_session_loop`（编排）+ `build_and_execute_agent`（cfg 组装与 v2 dispatch）
//! - Prediction facade：`execute_prediction` / `extract_prediction_text`
//!
//! 子流程已抽到本模块的子模块 `executor_helpers`：
//! - [`intercept_immediate_command`]：slash 命令拦截
//! - [`spawn_event_pump`]：后台事件泵 + Langfuse tracer
//! - [`forward_langfuse_event`]：单个 executor 事件 → Langfuse tracer
//! - [`build_and_execute_agent_v2`]：v2 stages 装配与 ReAct 循环驱动（9 个 phase）
//! - [`collect_result`] / [`close_channel`] / [`wait_for_pump`]：结果收集
//!
//! `executor_helpers` 是本模块的子模块（声明见文件末尾 `mod executor_helpers;`），
//! 因此可以直接访问本模块的私有项（struct/enum/use 引入的符号）。本模块通过
//! `use executor_helpers::{...};` 把 helper 提升到本模块命名空间，使
//! `executor_test.rs` 的 `super::{intercept_immediate_command, InterceptRequest}`
//! 路径继续可解析。
//!
//! ## Cancel 语义保持
//!
//! - `intercept_immediate_command` 内的 `tokio::select!` 分支顺序原样保留
//!   （`cmd.execute` 与 `cancel.cancelled()` 仍按原 biased 顺序，二者均触发 push_done）
//! - `build_and_execute_agent_v2` 末尾的 cancel cascade 仍在循环失败后触发，
//!   `LoopResult::Error` 分支先发 `AgentExecutionFailed` 事件再判断 stop_reason
//! - `collect_result` 严格 "close → wait_for_pump(10s timeout) → drain recall"

use std::sync::Arc;

use peri_agent::{
    agent::{
        events::{AgentEventHandler, BackgroundTaskResult, ExecutorEvent},
        react::ReactLLM,
        state::AgentState,
        AgentCancellationToken,
    },
    interaction::{ChannelState, UserInteractionBroker},
    messages::{BaseMessage, ContentBlock, MessageContent},
    session::queue::QueuedMessage,
};
use tracing::debug;

use crate::{
    agent::builder::{self, AcpAgentConfig},
    langfuse::LangfuseSession,
    prompt::{build_system_prompt, PromptFeatures},
    provider::LlmProvider,
    session::{
        agent_pool::{AgentPool, CachedLlmInstances},
        agent_runtime::{AgentRuntime, CancelPolicy},
        async_router::AsyncRouter,
        event_sink::EventSink,
        SessionManager,
    },
};

// 引入子流程 helper：intercept_immediate_command / InterceptRequest /
// spawn_event_pump / SpawnPumpRequest / PumpHandle / forward_langfuse_event /
// collect_result / CollectRequest / close_channel / wait_for_pump /
// build_and_execute_agent_v2 在本模块命名空间可见——executor_test.rs 通过
// `super::` 访问的 helper 路径保持不变。
//
// 这些 helper 标 `pub(super)`（仅本模块可见）；其中 `forward_langfuse_event`
// 是 `pub(crate)`（被 `crate::agent::workflow_agent` 跨模块复用），通过下方的
// `pub(crate) use executor_helpers::forward_langfuse_event;` 重导出保持
// `crate::session::executor::forward_langfuse_event` 路径不变。
#[allow(unused_imports)]
use executor_helpers::{
    build_and_execute_agent_v2, close_channel, collect_result, intercept_immediate_command,
    spawn_event_pump, wait_for_pump, CollectRequest, InterceptRequest, PumpHandle,
    SpawnPumpRequest,
};
// 重导出 langfuse 转发器，保持 `crate::session::executor::forward_langfuse_event`
// 路径对 `agent::workflow_agent` 可见（跨模块复用——workflow_agent 自跑独立 langfuse
// tracer pump，事件→tracer 映射与主 executor 完全一致）。
pub(crate) use executor_helpers::forward_langfuse_event;

/// High-level reason why prompt execution stopped, used to derive ACP `StopReason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptStopReason {
    /// Normal completion — the agent finished its turn.
    EndTurn,
    /// The user cancelled via `session/cancel`.
    Cancelled,
    /// The agent reached the maximum number of iterations.
    MaxTurnRequests,
}

/// Result of prompt execution.
pub struct PromptResult {
    /// Updated message history after execution.
    pub messages: Vec<BaseMessage>,
    /// Whether execution succeeded.
    pub ok: bool,
    /// Why the prompt execution stopped.
    pub stop_reason: PromptStopReason,
    /// Recall items collected during execution (for next turn injection).
    pub recall_items: Vec<String>,
}

/// Session-scoped frozen data that locks system prompt stability.
///
/// Populated at session creation time by `session/new`, passed through to
/// every turn's agent build to guarantee the system prompt never changes
/// within a session.
///
/// # v2 迁移
///
/// FrozenSessionData 现在委托给 `peri_agent::session::FrozenContext`
/// 作为不可变数据存储，同时保留 v1 兼容的 accessor 方法。
/// 构造时同时产出 `peri_agent::session::FrozenContext` 供 Session::new() 使用。
#[derive(Clone)]
pub struct FrozenSessionData {
    /// v2 冻结上下文（委托给 peri-agent）
    v2_frozen: peri_agent::session::FrozenContext,
    /// Frozen content of CLAUDE.local.md, None if no file.
    /// v2 FrozenContext 未包含 local_md，保留此处。
    claude_local_md: Option<Arc<str>>,
    /// Whether cwd was a git repo at session creation time.
    is_git_repo: bool,
}

impl FrozenSessionData {
    /// 唯一构造入口：在 `session/new` 时调用，捕获 cwd/language/CLAUDE.md/
    /// skills/system_prompt/date。
    ///
    /// v2：构造 `peri_agent::session::FrozenContext` 作为内部委托，
    /// 同时保留 v1 兼容字段。
    pub fn build(
        cwd: &str,
        language: Option<&str>,
        plugin_skill_roots: &[peri_middlewares::skills::SkillRoot],
        plugin_agent_dirs: &[std::path::PathBuf],
        frozen_date: &str,
    ) -> Self {
        let (claude_md, claude_local_md) =
            peri_middlewares::AgentsMdMiddleware::read_frozen_content(cwd);

        // 一次性读取 disableBundledSkills 并冻结到 frozen_skill_summary
        // （保持系统提示词稳定性：会话内不重读）
        let disable_bundled = peri_middlewares::skills::load_disable_bundled_skills();
        let skill_summary = peri_middlewares::SkillsMiddleware::build_frozen_summary(
            cwd,
            plugin_skill_roots.to_vec(),
            disable_bundled,
        );

        let features = crate::prompt::PromptFeatures::detect();
        let system_prompt = crate::prompt::build_system_prompt(
            None,
            cwd,
            features,
            plugin_agent_dirs,
            Some(frozen_date),
            language,
        );

        let is_git_repo = std::path::Path::new(cwd).join(".git").exists();

        // 构建 v2 FrozenContext
        let v2_frozen = peri_agent::session::FrozenContext {
            system_prompt: Arc::from(system_prompt),
            claude_md: claude_md.clone().map(Arc::from).unwrap_or_default(),
            skill_summary: skill_summary.clone().map(Arc::from).unwrap_or_default(),
            date: Arc::from(frozen_date),
            language: language.map(|l| Arc::from(l.to_string())),
        };

        Self {
            v2_frozen,
            claude_local_md: claude_local_md.map(Arc::from),
            is_git_repo,
        }
    }

    /// v2 冻结上下文引用（供 Session::new() 使用）
    pub fn v2_frozen(&self) -> &peri_agent::session::FrozenContext {
        &self.v2_frozen
    }

    /// 会话内冻结的完整 system prompt 字符串。
    pub fn system_prompt(&self) -> &str {
        &self.v2_frozen.system_prompt
    }

    /// 冻结的 CLAUDE.md 内容（已解析 `@import`），无文件时为 None。
    pub fn claude_md(&self) -> Option<&str> {
        // v2 FrozenContext 始终有值，空字符串表示无文件
        let s = &*self.v2_frozen.claude_md;
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }

    /// 冻结的 CLAUDE.local.md 内容，无文件时为 None。
    pub fn claude_local_md(&self) -> Option<&str> {
        self.claude_local_md.as_deref()
    }

    /// 冻结的 skills summary 字符串，无 skills 时为 None。
    pub fn skill_summary(&self) -> Option<&str> {
        let s = &*self.v2_frozen.skill_summary;
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }

    /// 会话创建日期（YYYY-MM-DD 格式）。
    pub fn date(&self) -> &str {
        &self.v2_frozen.date
    }

    /// 会话创建时 cwd 是否为 git 仓库。
    pub fn is_git_repo(&self) -> bool {
        self.is_git_repo
    }

    /// 会话创建时的语言偏好（如 "zh-CN"、"en"）。None 表示 auto-detect。
    pub fn language(&self) -> Option<&str> {
        self.v2_frozen.language.as_deref()
    }
}

/// Parameter Object for [`run_session_loop`].
///
/// Groups 30 positional parameters into a single struct to eliminate
/// `#[allow(clippy::too_many_arguments)]` and reduce call-site placeholder
/// noise. Construction uses named-field syntax; default values are explicit
/// at each call site (no builder hiding required state).
///
/// # Fields by concern
/// - **Session-level identity & transport**：`provider` / `peri_config` / `cwd`
///   / `session_id` / `cancel` / `event_sink` / `broker` / `permission_mode`
/// - **Per-turn content**：`content` / `frozen` / `history` / `incoming_recalls`
///   / `session_start_source` / `bg_results`
/// - **Middleware chain resources**：`plugin_skill_roots` / `plugin_agent_dirs`
///   / `hook_groups` / `cron_scheduler` / `mcp_pool` / `channel_state`
///   / `tool_search_index` / `shared_tools` / `lsp_servers` / `langfuse_session`
/// - **Session-scoped caches & persistence**：`pool` / `thread_store` / `thread_id`
///   / `session_manager`
pub struct PromptExecutionContext {
    // ── Session-level identity & transport ───────────────────────────────────
    /// 当前激活的 LLM provider（snapshot，每轮从 `Arc<RwLock<>>` 克隆）。
    pub provider: LlmProvider,
    /// 全局 peri 配置（snapshot，每轮从 `Arc<RwLock<>>` 克隆）。
    pub peri_config: Arc<crate::provider::PeriConfig>,
    /// 会话工作目录。
    pub cwd: String,
    /// 会话 ID（用于事件路由、SessionManager 查询、Langfuse trace）。
    pub session_id: String,
    /// 取消令牌（由 SessionManager 管理，clone 后传入 executor）。
    pub cancel: AgentCancellationToken,
    /// 事件出口（TUI 用 TransportEventSink，stdio 用 StdioEventSink）。
    pub event_sink: Arc<dyn EventSink>,
    /// 用户交互 broker（HITL/AskUser 通道）。
    pub broker: Arc<dyn UserInteractionBroker>,
    /// 权限模式共享句柄。
    pub permission_mode: Arc<peri_middlewares::prelude::SharedPermissionMode>,

    // ── Per-turn content ──────────────────────────────────────────────────────
    /// 用户本轮输入。
    pub content: MessageContent,
    /// 会话级 frozen 数据（system prompt 稳定性锚点）。
    pub frozen: Option<FrozenSessionData>,
    /// 现有历史消息（执行前）。
    pub history: Vec<BaseMessage>,
    /// 上一轮 recall 注入项。
    pub incoming_recalls: Vec<String>,
    /// SessionStart matcher：startup / resume / clear / compact。
    /// None 表示不触发 SessionStart。
    pub session_start_source: Option<String>,
    /// 后台任务结果（注入合成的 AgentResult tool_use/tool_result）。
    pub bg_results: Vec<peri_agent::agent::events::BackgroundTaskResult>,

    // ── Middleware chain resources ────────────────────────────────────────────
    /// 插件 skill 根列表（携带 source/plugin_name）。
    pub plugin_skill_roots: Vec<peri_middlewares::skills::SkillRoot>,
    /// 插件 agent 目录列表。
    pub plugin_agent_dirs: Vec<std::path::PathBuf>,
    /// Hook 组（按全局/项目/本地分层）。
    pub hook_groups: Vec<Vec<peri_middlewares::hooks::RegisteredHook>>,
    /// Cron 调度器（共享，跨轮次复用）。
    pub cron_scheduler: Option<Arc<parking_lot::Mutex<peri_middlewares::cron::CronScheduler>>>,
    /// MCP client 池。
    pub mcp_pool: Option<Arc<peri_middlewares::mcp::McpClientPool>>,
    /// Channel broker 共享状态（AskUser 走 channel 时使用）。
    pub channel_state: Option<Arc<ChannelState>>,
    /// 工具搜索索引（Deferred Tools 发现）。
    pub tool_search_index: Arc<peri_middlewares::tool_search::ToolSearchIndex>,
    /// 共享工具表（运行时动态注册的工具）。
    pub shared_tools: Arc<
        parking_lot::RwLock<
            std::collections::HashMap<String, Arc<dyn peri_agent::tools::BaseTool>>,
        >,
    >,
    /// LSP server 配置。
    pub lsp_servers: Vec<peri_lsp::config::LspServerConfig>,
    /// Langfuse 会话级句柄（None 表示禁用遥测）。
    pub langfuse_session: Option<Arc<LangfuseSession>>,

    // ── Session-scoped caches & persistence ───────────────────────────────────
    /// AgentPool（LLM/Compact model 缓存，session 级）。
    pub pool: Arc<parking_lot::Mutex<AgentPool>>,
    /// 持久化存储（None 表示 print 模式不持久化）。
    pub thread_store: Option<Arc<dyn peri_agent::thread::ThreadStore>>,
    /// 当前 thread ID（持久化 + SubAgent 注册）。
    pub thread_id: Option<String>,
    /// SessionManager（用于 cascade cancel 子 agent + register/deregister runtime）。
    pub session_manager: Option<SessionManager>,
    /// Workflow agent 执行器（None = 不启用 workflow 功能）
    pub workflow_executor: Option<Arc<dyn peri_workflow::runner::AgentExecutor>>,
    /// Session 级 WorkflowMiddleware（None = 该会话不启用 workflow 或临时创建）。
    /// session/new 时创建，存入 SessionState/SessionInfo，每轮复用。
    pub workflow_middleware: Option<Arc<peri_middlewares::workflow::WorkflowMiddleware>>,

    // ── Transport-aware async wake ───────────────────────────────────────────
    /// 是否允许主 agent idle 时 await_wake 等异步事件续跑。
    /// TUI（MpscTransport）路径设 true → run_react_loop 在 queue 空时阻塞等异步事件。
    /// stdio/print 路径设 false → run_react_loop 直接退出，保持 PromptResponse 响应性。
    /// 这是 c9dbfb18 移除 run_session_loop 末尾 await_wake 后的替代方案：通过 transport
    /// 分流避免 stdio/IDE 卡死，同时让 TUI 续跑机制恢复。
    pub allow_await_wake: bool,
}

/// Per-turn computed configuration derived from `PromptExecutionContext`.
///
/// Built once at the top of [`run_session_loop`], passed by reference to
/// [`build_and_execute_agent`] to avoid recomputing and to keep the agent
/// builder function signature manageable.
struct TurnConfig<'a> {
    provider: &'a LlmProvider,
    peri_config: &'a Arc<crate::provider::PeriConfig>,
    cwd: &'a str,
    frozen: Option<&'a FrozenSessionData>,
    language: Option<String>,
    cancel: &'a AgentCancellationToken,
    permission_mode: &'a Arc<peri_middlewares::prelude::SharedPermissionMode>,
    broker: &'a Arc<dyn UserInteractionBroker>,
    session_start_source: Option<String>,
    auxiliary_model: Option<Arc<dyn peri_agent::llm::BaseModel>>,
    effective_context_window: u32,
}

/// Shared agent execution pipeline with auto-compact support.
///
/// This is the orchestrator. The actual work is split across four private
/// helpers:
/// - [`intercept_immediate_command`]：slash 命令拦截（Immediate 直接返回，不构建 agent）
/// - [`spawn_event_pump`]：后台事件泵 + Langfuse tracer
/// - [`build_and_execute_agent`]：agent 构建 + 执行 + 状态收集
/// - [`collect_result`]：close channel + 等待 pump drain + recall 提取
///
/// The caller is responsible for:
/// - Session management (storing/retrieving cwd, history, cancel_token)
/// - Choosing the broker (HITL/AskUser handler)
/// - Providing the correct `EventSink` implementation
pub async fn run_session_loop(ctx: PromptExecutionContext) -> PromptResult {
    // 解构 ctx：所有字段一次性 move，避免后续部分 move 导致的借用冲突。
    // 注意：history/content/bg_results 在 move 前先用引用读取（compact_config 等不需要 move）。
    let PromptExecutionContext {
        provider,
        peri_config,
        cwd,
        session_id,
        cancel,
        event_sink,
        broker,
        permission_mode,
        content,
        frozen,
        history,
        incoming_recalls,
        session_start_source,
        bg_results,
        plugin_skill_roots,
        plugin_agent_dirs,
        hook_groups,
        cron_scheduler,
        mcp_pool,
        channel_state,
        tool_search_index,
        shared_tools,
        lsp_servers,
        langfuse_session,
        pool,
        thread_store,
        thread_id,
        session_manager,
        workflow_executor,
        workflow_middleware,
        allow_await_wake,
    } = ctx;

    // Compact config — computed early for command interception and agent building.
    let mut compact_config = peri_config.config.compact.clone().unwrap_or_default();
    compact_config.apply_env_overrides();
    let disable_compact = std::env::var("DISABLE_COMPACT").is_ok()
        || std::env::var("DISABLE_AUTO_COMPACT").is_ok()
        || !compact_config.auto_compact_enabled;

    // 解析会话级共享的 v2 MessageQueue（来自 AcpSession.v2_message_queue）。
    // 缺失时（无 session_manager / session 不存在）退化为独立 MessageQueue，
    // 保持行为可运行——但跨 turn 消息将不可见（仅降级场景）。
    //
    // 在 run_session_loop 开头解析而非 build_and_execute_agent 内部，
    // 是为了让 bg_results / workflow Path B 等会话级注入能在此处统一 push。
    let v2_message_queue = session_manager
        .as_ref()
        .and_then(|sm| sm.get_session(&session_id))
        .map(|s| s.v2_message_queue.clone())
        .unwrap_or_else(peri_agent::session::MessageQueue::new);

    // 解析 session-level SessionInbox（await-wake wrapper）。
    // 用于：(1) executor idle 期间 await_wake 阻塞等待异步事件，
    // (2) AsyncRouter 推送 bg_results/workflow 事件时触发 wake。
    // None 表示不支持 async wake（如 print mode），保持向后兼容。
    let session_inbox = session_manager
        .as_ref()
        .and_then(|sm| sm.session_inbox_for(&session_id));

    // 构建 AsyncRouter（统一异步事件路由到 inbox）。
    // 通过 InboxHandle 推送 Defer 消息并触发 wake Notify，
    // 替代 executor 的直接 v2_message_queue.push（raw，无 wake）。
    let async_router = session_inbox
        .as_ref()
        .map(|inbox| AsyncRouter::new(inbox.handle()));

    // bg_results 通过 AsyncRouter（或回退到 v2 MessageQueue）push（Defer kind）。
    //
    // Defer 是异步延迟结果的正确语义：本轮 Receive 跳过保留，End 阶段 drain
    // 唤醒新 turn，并由 `mod.rs::run_react_loop` 写入 transcript（包裹
    // `<system-reminder>`）。与 WorkflowComplete / cron 等其他异步唤醒路径
    // 走同一套机制——见 `append_messages_to_transcript`。
    if !bg_results.is_empty() {
        tracing::info!(
            count = bg_results.len(),
            "[bg-diag] ctx.bg_results is non-empty, will inject each via AsyncRouter"
        );
        if let Some(ref router) = async_router {
            // v2 路径：通过 AsyncRouter → InboxHandle → push_defer（触发 wake）
            for result in &bg_results {
                router.route_bg_result(result);
            }
        } else {
            // 回退路径：直接 push（无 wake，兼容 print mode / 无 SessionManager）
            use peri_agent::session::queue::{MessageKind as V2Kind, MessageSource as V2Src};
            for result in &bg_results {
                v2_message_queue.push(QueuedMessage::new(
                    V2Kind::Defer,
                    V2Src::SubAgentComplete,
                    BaseMessage::human(MessageContent::text(result.to_notification())),
                ));
            }
        }
    }

    // Auxiliary model — reuse AgentPool cache if available, otherwise create fresh.
    // 共享于 v2 stages/compact.rs（摘要）与 Goal 工具（完成度验证）。
    let cached_llm = {
        let pool_guard = pool.lock();
        if pool_guard.has_valid_cache(&provider) {
            pool_guard.get_cached_llm().cloned()
        } else {
            None
        }
    };
    let auxiliary_model: Option<Arc<dyn peri_agent::llm::BaseModel>> = if disable_compact {
        None
    } else {
        cached_llm
            .as_ref()
            .map(|c| c.auxiliary_model.clone())
            .or_else(|| Some(provider.clone().into_model().into()))
    };

    // Context window (前置计算，供 bg event pump 和 compact 使用)
    let context_window = provider.context_window();
    let context_1m = peri_config.config.context_1m.unwrap_or(false);
    let effective_context_window = if context_1m {
        1_000_000
    } else {
        context_window
    };

    // 前置创建 bg 事件通道（BgCommand 等 Immediate 命令依赖）
    let (bg_event_tx_for_cmd, mut bg_event_rx_for_cmd) =
        tokio::sync::mpsc::unbounded_channel::<ExecutorEvent>();
    // session 级 registry（跨 prompt 存活，由 executor 从 session 获取）
    let bg_registry_for_cmd = session_manager
        .as_ref()
        .and_then(|sm| sm.get_session(&session_id))
        .map(|s| s.background_registry.clone())
        .unwrap_or_else(|| Arc::new(peri_middlewares::subagent::BackgroundTaskRegistry::new()));

    // BgCommand 事件的 bg event pump（必须在命令拦截之前启动，Immediate 命令才能发事件）
    {
        let bg_cmd_sink = Arc::clone(&event_sink);
        let bg_cmd_sid = session_id.clone();
        let bg_cmd_cw = effective_context_window;
        tokio::spawn(async move {
            while let Some(bg_event) = bg_event_rx_for_cmd.recv().await {
                bg_cmd_sink
                    .push_event(&bg_cmd_sid, &bg_event, bg_cmd_cw)
                    .await;
                // bg agent 完成后必须 push_done，否则 TUI 因 SubagentStopped 设置
                // is_loading=true 后永久卡住（与 Immediate 命令路径同模式，需手动
                // 发 peri/agent_event_done 触发 acp_notifier 的 AgentDone→TurnDone）。
                if matches!(bg_event, ExecutorEvent::BackgroundTaskCompleted(_)) {
                    bg_cmd_sink.push_done(&bg_cmd_sid).await;
                }
            }
        });
    }

    // Registry → ACP 事件泵：将 BgRegistryEvent 转换为 ACP unstable 事件
    {
        let (registry_event_tx, mut registry_event_rx) =
            tokio::sync::mpsc::unbounded_channel::<peri_middlewares::subagent::BgRegistryEvent>();
        bg_registry_for_cmd.set_event_sender(registry_event_tx, session_id.clone());
        let registry_sink = Arc::clone(&event_sink);
        let registry_sid = session_id.clone();
        let registry_async_router = async_router.clone();
        tokio::spawn(async move {
            while let Some(event) = registry_event_rx.recv().await {
                tracing::info!(
                    event_type = match &event {
                        peri_middlewares::subagent::BgRegistryEvent::Started { .. } => "Started",
                        peri_middlewares::subagent::BgRegistryEvent::Completed { .. } =>
                            "Completed",
                        peri_middlewares::subagent::BgRegistryEvent::Cancelled { .. } =>
                            "Cancelled",
                    },
                    "[bg-diag] registry event pump: received event"
                );
                let (event_name, payload) = match &event {
                    peri_middlewares::subagent::BgRegistryEvent::Started {
                        task_id,
                        kind,
                        summary,
                        started_at,
                    } => (
                        "bg-task-started",
                        serde_json::json!({
                            "task_id": task_id,
                            "kind": kind,
                            "summary": summary,
                            "started_at": started_at,
                        }),
                    ),
                    peri_middlewares::subagent::BgRegistryEvent::Completed {
                        task_id,
                        success,
                        output_preview,
                        duration_ms,
                        result,
                    } => {
                        // 注入主 agent inbox，触发续跑（若 AsyncRouter 可用）。
                        // 模式参照 workflow Path B（route_workflow_event）。
                        // 注意：此注入只对 Agent 工具 bg 模式有效（主 agent 在
                        // run_session_loop 内）；/bg 命令是 immediate command，
                        // 主 agent 不在 loop，注入对它无效（用户已接受此 trade-off）。
                        tracing::info!(
                            task_id = %task_id,
                            "[bg-diag] registry event pump: Completed branch, calling route_bg_result"
                        );
                        if let Some(ref router) = registry_async_router {
                            router.route_bg_result(result);
                        } else {
                            tracing::info!(
                                "[bg-diag] registry event pump: async_router is None, skip inject"
                            );
                        }

                        (
                            "bg-task-completed",
                            serde_json::json!({
                                "task_id": task_id,
                                "success": success,
                                "output_preview": output_preview,
                                "duration_ms": duration_ms,
                            }),
                        )
                    }
                    peri_middlewares::subagent::BgRegistryEvent::Cancelled { task_id, reason } => (
                        "bg-task-cancelled",
                        serde_json::json!({
                            "task_id": task_id,
                            "reason": reason,
                        }),
                    ),
                };
                registry_sink
                    .push_unstable_event(&registry_sid, event_name.to_string(), payload)
                    .await;
            }
        });
    }

    // Command interception — check if content is a slash command before building agent.
    if let Some(immediate) = intercept_immediate_command(InterceptRequest {
        content: &content,
        history: &history,
        cwd: &cwd,
        session_id: &session_id,
        cancel: &cancel,
        peri_config: &peri_config,
        event_sink: &event_sink,
        auxiliary_model: &auxiliary_model,
        thread_store: thread_store.clone(),
        thread_id: thread_id.clone(),
        bg_event_tx: &bg_event_tx_for_cmd,
        bg_registry: &bg_registry_for_cmd,
        frozen: frozen.as_ref(),
    })
    .await
    {
        return immediate;
    }

    let trace_input = content.text_content();
    let agent_input = if incoming_recalls.is_empty() {
        peri_agent::agent::react::AgentInput::blocks(content)
    } else {
        let reminder_text = format!(
            "<system-reminder>\n{}\n</system-reminder>",
            incoming_recalls.join("\n")
        );
        let mut blocks = content.content_blocks();
        blocks.push(ContentBlock::text(reminder_text));
        peri_agent::agent::react::AgentInput::blocks(MessageContent::blocks(blocks))
    };

    // [v2] Context budget 由 AgentComponents 传给 StageContext，此处不再需要本地变量。

    // Event channel (lives for entire run_session_loop lifetime)
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<ExecutorEvent>();
    let event_tx = Arc::new(parking_lot::Mutex::new(Some(event_tx)));

    // 将会 move 进 BuildAgentRequest 的 middleware resources（无法借用，必须 move）。
    // turn 仍以引用形式借用 provider/peri_config/cwd/cancel/permission_mode/broker。
    let turn = TurnConfig {
        provider: &provider,
        peri_config: &peri_config,
        cwd: &cwd,
        frozen: frozen.as_ref(),
        language: frozen
            .as_ref()
            .and_then(|f| f.language().map(|s| s.to_string()))
            .or_else(|| peri_config.config.language.clone()),
        cancel: &cancel,
        permission_mode: &permission_mode,
        broker: &broker,
        session_start_source,
        auxiliary_model: auxiliary_model.clone(),
        effective_context_window,
    };

    // Main event pump
    let pump_handle = spawn_event_pump(SpawnPumpRequest {
        event_rx,
        sink: Arc::clone(&event_sink),
        session_id: session_id.clone(),
        effective_context_window,
        langfuse_session: langfuse_session.clone(),
        trace_input: trace_input.to_string(),
        provider_display_name: provider.display_name().to_string(),
    });

    // transport-aware: 仅 TUI 路径（allow_await_wake=true）注入 idle_inbox，
    // 让 run_react_loop 在 queue 空时 await_wake 等异步事件。
    // stdio/print 路径 None，保持 run_react_loop 直接退出。
    let idle_inbox = if allow_await_wake {
        session_inbox.as_ref().map(Arc::clone)
    } else {
        None
    };

    // idle_should_wait probe：检查 background_registry 是否有未完成的 bg subagent。
    // TUI 路径注入，run_react_loop 用它 gate await_wake（避免正常对话 loading 卡死）。
    // stdio/print 路径 idle_inbox=None，probe 即使有也不影响（gate 双层保险）。
    let idle_should_wait: Option<Arc<dyn Fn() -> bool + Send + Sync>> = {
        let probe_registry = Arc::clone(&bg_registry_for_cmd);
        Some(Arc::new(move || probe_registry.active_count() > 0))
    };

    // 把会 move 的资源打包成 struct，turn + event_tx + cached_llm 仍借用。
    // 由于 prompt builder 需要的所有资源都在这里 move 进 BuildAgentRequest，
    // 调用方后续不再访问这些字段（session_id 在 collect_result 借用，
    // 此时 BuildAgentRequest 已 drop）。
    let exec_outcome = build_and_execute_agent(BuildAgentRequest {
        turn: &turn,
        agent_input,
        history,
        plugin_skill_roots,
        plugin_agent_dirs,
        hook_groups,
        cron_scheduler,
        mcp_pool,
        channel_state,
        tool_search_index,
        shared_tools,
        lsp_servers,
        langfuse_session,
        pool,
        thread_store,
        thread_id,
        session_manager,
        workflow_executor,
        workflow_middleware: workflow_middleware.as_ref(),
        bg_registry: Arc::clone(&bg_registry_for_cmd),
        event_sink: &event_sink,
        session_id: &session_id,
        event_tx: &event_tx,
        cached_llm: cached_llm.as_ref(),
        v2_message_queue: &v2_message_queue,
        async_router: async_router.clone(),
        idle_inbox,
        idle_should_wait,
    })
    .await;

    let result = collect_result(CollectRequest {
        event_tx: &event_tx,
        pump_handle,
        session_id: &session_id,
        exec_outcome,
    })
    .await;

    result
}

// ── Build Agent Request parameter object ────────────────────────────────────

/// Agent 构建请求（参数对象）。
///
/// `turn` 携带本轮计算出的紧凑配置（provider/config/compact 等），
/// 其余字段是中间件链所需的所有共享资源。
struct BuildAgentRequest<'a> {
    turn: &'a TurnConfig<'a>,
    agent_input: peri_agent::agent::react::AgentInput,
    history: Vec<BaseMessage>,
    // ── 会 move 的中间件资源 ────────────────────────────────────────────────
    plugin_skill_roots: Vec<peri_middlewares::skills::SkillRoot>,
    plugin_agent_dirs: Vec<std::path::PathBuf>,
    hook_groups: Vec<Vec<peri_middlewares::hooks::RegisteredHook>>,
    cron_scheduler: Option<Arc<parking_lot::Mutex<peri_middlewares::cron::CronScheduler>>>,
    mcp_pool: Option<Arc<peri_middlewares::mcp::McpClientPool>>,
    channel_state: Option<Arc<ChannelState>>,
    tool_search_index: Arc<peri_middlewares::tool_search::ToolSearchIndex>,
    shared_tools: Arc<
        parking_lot::RwLock<
            std::collections::HashMap<String, Arc<dyn peri_agent::tools::BaseTool>>,
        >,
    >,
    lsp_servers: Vec<peri_lsp::config::LspServerConfig>,
    langfuse_session: Option<Arc<LangfuseSession>>,
    pool: Arc<parking_lot::Mutex<AgentPool>>,
    thread_store: Option<Arc<dyn peri_agent::thread::ThreadStore>>,
    thread_id: Option<String>,
    session_manager: Option<SessionManager>,
    workflow_executor: Option<Arc<dyn peri_workflow::runner::AgentExecutor>>,
    workflow_middleware: Option<&'a Arc<peri_middlewares::workflow::WorkflowMiddleware>>,
    bg_registry: Arc<peri_middlewares::subagent::BackgroundTaskRegistry>,
    // ── 借用的引用 ──────────────────────────────────────────────────────────
    event_sink: &'a Arc<dyn EventSink>,
    session_id: &'a str,
    event_tx:
        &'a Arc<parking_lot::Mutex<Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>>>,
    cached_llm: Option<&'a CachedLlmInstances>,
    /// 会话级共享 v2 MessageQueue（run_session_loop 解析后透传，
    /// 避免 build_and_execute_agent 重复解析；MessageQueue 内部 Arc 共享）。
    v2_message_queue: &'a peri_agent::session::MessageQueue,
    /// AsyncRouter（统一异步事件路由到 inbox，触发 wake）。
    /// None 表示无 inbox（print mode / 无 SessionManager），回退到直接 push。
    async_router: Option<AsyncRouter>,
    /// Transport-aware idle inbox（await_wake）。TUI 路径 Some，stdio/print 路径 None。
    idle_inbox: Option<Arc<peri_agent::agent::session::SessionInbox>>,
    /// idle_should_wait probe：检查 background_registry.active_count > 0。
    /// gate await_wake，避免正常对话 loading 卡死。
    idle_should_wait: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

/// Agent 执行后的最终输出（state + 停止原因）。
struct ExecOutcome {
    ok: bool,
    stop_reason: PromptStopReason,
    agent_state: AgentState,
}

/// 构建 + 执行 agent。包含：
/// - system prompt 解析（frozen 或 legacy 重建）
/// - SubAgentMiddleware register/deregister 闭包
/// - `build_agent` 调用 + AgentPool 缓存回写
/// - bg event pump + todo 转发 pump 启动
/// - `build_and_execute_agent_v2` 调用 + 错误事件转发
/// - cancel cascade 子 agent
async fn build_and_execute_agent(req: BuildAgentRequest<'_>) -> ExecOutcome {
    let BuildAgentRequest {
        turn,
        agent_input,
        history,
        plugin_skill_roots,
        plugin_agent_dirs,
        hook_groups,
        cron_scheduler,
        mcp_pool,
        channel_state,
        tool_search_index,
        shared_tools,
        lsp_servers,
        langfuse_session: _langfuse_session,
        pool,
        thread_store,
        thread_id,
        session_manager,
        workflow_executor,
        workflow_middleware,
        bg_registry,
        event_sink,
        session_id,
        event_tx,
        cached_llm,
        v2_message_queue,
        async_router,
        idle_inbox,
        idle_should_wait,
    } = req;

    let (
        system_prompt,
        frozen_claude_md,
        frozen_claude_local_md,
        frozen_skill_summary,
        frozen_date,
    ) = if let Some(f) = turn.frozen {
        // 使用 session 创建时冻结的数据，跳过重建
        (
            f.system_prompt().to_string(),
            f.claude_md().map(|s| s.to_string()),
            f.claude_local_md().map(|s| s.to_string()),
            f.skill_summary().map(|s| s.to_string()),
            Some(f.date().to_string()),
        )
    } else {
        // Legacy 路径：未提供 frozen 数据时每轮重建 system prompt。
        //
        // [TRAP] 当前仅 print mode (`-p`, cli_print.rs:207 `frozen: None`) 进入此分支，
        // 单轮执行后退出，因此 "per-turn rebuild" 实际不会发生。
        // SubAgent 不走此路径——它们的 system prompt 由 builder.rs:356-366 的
        // system_builder closure 独立构造。
        //
        // 加 warn! 提升可观测性：如果未来有新调用方忘记传 frozen 数据，
        // 日志会立刻暴露（违反 frozen 不变量 = 第一优先级）。
        tracing::warn!(
            cwd = %turn.cwd,
            "run_session_loop: frozen data 未提供，回退到 per-turn rebuild 路径（仅 print mode 合法）"
        );
        let features = PromptFeatures::detect();
        let sp = build_system_prompt(
            None,
            turn.cwd,
            features,
            &plugin_agent_dirs,
            None,
            turn.language.as_deref(),
        );
        (sp, None, None, None, None)
    };

    // Build register/deregister closures for SubAgentMiddleware
    let register_runtime = session_manager.clone().map(|sm| {
        let sid = session_id.to_string();
        Arc::new(
            move |thread_id: String, cancel_token: AgentCancellationToken, policy: String| {
                if let Some(mut session) = sm.get_session_mut(&sid) {
                    let runtime =
                        AgentRuntime::new(thread_id.clone(), CancelPolicy::from_str(&policy));
                    // Store the provided cancel_token so external cancellation works
                    let rt = AgentRuntime {
                        thread_id,
                        cancel_token,
                        cancel_policy: runtime.cancel_policy,
                        status: runtime.status,
                    };
                    session.active_agents.insert(rt.thread_id.clone(), rt);
                }
            },
        ) as crate::agent::builder::RegisterRuntimeFn
    });
    let deregister_runtime = session_manager.clone().map(|sm| {
        let sid = session_id.to_string();
        Arc::new(move |thread_id: &str| {
            if let Some(mut session) = sm.get_session_mut(&sid) {
                session.active_agents.remove(thread_id);
            }
        }) as crate::agent::builder::DeregisterRuntimeFn
    });

    let event_handler: Arc<dyn AgentEventHandler> =
        Arc::new(peri_agent::agent::events::FnEventHandler({
            let tx = event_tx.clone();
            move |event: ExecutorEvent| {
                if let Some(tx) = tx.lock().as_ref() {
                    let _ = tx.send(event);
                }
            }
        }));

    // Session 级 workflow 完成通知消费者（单次 spawn）。
    // 双路径：
    //   Path A (TUI): 通过 EventSink 直推 BackgroundTaskCompleted → 通知条
    //   Path B (Agent): 通过 AsyncRouter → InboxHandle → push_defer（Defer kind）→ End 阶段唤醒新 turn
    //
    // [NOTE] 自动 continuation 需 TUI 侧处理 BackgroundTaskCompleted 事件（参考 bg task auto-continuation）。
    if let Some(wf_mw) = workflow_middleware {
        // 将 session 级 bg_registry 注入 WorkflowMiddleware（延迟注入，支持内部可变性）
        wf_mw.set_bg_registry(bg_registry.clone());

        // init_notification_buffer() 是 set-once gate：首次返回 true，后续返回 false。
        // WorkflowMiddleware 是 session 级实例（session/new 创建），
        // 因此每个 session 的消费者只 spawn 一次，无跨 session 污染。
        if wf_mw.init_notification_buffer() {
            let wf_mw_for_notify = Arc::clone(wf_mw);
            let notify_sink = Arc::clone(event_sink);
            let notify_sid = session_id.to_string();
            let notify_cw = turn.effective_context_window;
            // AsyncRouter（v2 路径：push_defer + wake Notify）
            // 或回退 v2 queue clone（无 inbox 时直接 push，无 wake）
            let wf_router = async_router.clone();
            let fallback_queue = v2_message_queue.clone();
            tokio::spawn(async move {
                let mut rx = wf_mw_for_notify.subscribe_notifications();
                loop {
                    match rx.recv().await {
                        Ok(task_result) => {
                            // Path B: 通过 AsyncRouter（或回退 v2 queue）push Defer。
                            // AsyncRouter → InboxHandle → push_defer 触发 wake Notify，
                            // 替代直接 notify_queue.push（raw，无 wake）。
                            if let Some(ref router) = wf_router {
                                router.route_workflow_event(
                                    &task_result.run_id,
                                    &task_result.workflow_name,
                                    task_result.duration_ms,
                                    task_result.agent_count,
                                    task_result.tool_calls_count,
                                );
                            } else {
                                // 回退：直接 push（无 wake，兼容无 inbox 场景）
                                let short_id =
                                    &task_result.run_id[..8.min(task_result.run_id.len())];
                                let notif_text = format!(
                                    "[后台任务 {} 已完成] {} ({}ms, {} agents, {} tool calls)",
                                    short_id,
                                    task_result.workflow_name,
                                    task_result.duration_ms,
                                    task_result.agent_count,
                                    task_result.tool_calls_count,
                                );
                                fallback_queue.push(QueuedMessage::new(
                                    peri_agent::session::queue::MessageKind::Defer,
                                    peri_agent::session::queue::MessageSource::WorkflowComplete,
                                    BaseMessage::human(MessageContent::text(notif_text)),
                                ));
                            }

                            // Path A: 发 TUI 通知
                            let bg = BackgroundTaskResult {
                                task_id: task_result.run_id.clone(),
                                agent_name: format!("workflow:{}", task_result.workflow_name),
                                prompt_summary: task_result.workflow_name.clone(),
                                success: task_result.success,
                                output: format!(
                                    "Workflow '{}' finished with status {:?} ({}ms, {} agents, {} tool calls). \
                                     Results in .claude/workflow-runs/{}/state.json",
                                    task_result.workflow_name, task_result.status,
                                    task_result.duration_ms, task_result.agent_count,
                                    task_result.tool_calls_count, task_result.run_id
                                ),
                                tool_calls_count: task_result.tool_calls_count,
                                duration_ms: task_result.duration_ms,
                                child_thread_id: None,
                            };
                            notify_sink
                                .push_event(
                                    &notify_sid,
                                    &ExecutorEvent::BackgroundTaskCompleted(bg),
                                    notify_cw,
                                )
                                .await;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("WF notification consumer lagged by {} messages", n);
                            continue;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            break; // session 结束，自然退出
                        }
                    }
                }
            });
        }
    }

    // 从 session_manager 获取 goal_state（实现 GoalController trait）
    let goal_controller: Option<Arc<dyn peri_agent::goal::GoalController>> = session_manager
        .as_ref()
        .and_then(|sm| sm.goal_state_for(session_id))
        .map(|gs| Arc::new(gs) as Arc<dyn peri_agent::goal::GoalController>);

    let cfg = AcpAgentConfig {
        provider: turn.provider.clone(),
        cwd: turn.cwd.to_string(),
        system_prompt,
        frozen: builder::FrozenData {
            claude_md: frozen_claude_md,
            claude_local_md: frozen_claude_local_md,
            skill_summary: frozen_skill_summary,
            date: frozen_date,
        },
        event_handler,
        cancel: turn.cancel.clone(),
        permission_mode: turn.permission_mode.clone(),
        peri_config: Arc::new(turn.peri_config.as_ref().clone()),
        cron_scheduler,
        agent_overrides: None,
        preload_skills: Vec::new(),
        session_id: Some(session_id.to_string()),
        broker: turn.broker.clone(),
        plugin_skill_roots,
        plugin_agent_dirs,
        hook_groups,
        session_start_source: turn.session_start_source.clone(),
        mcp_pool,
        channel_state,
        tool_search_index,
        shared_tools,
        child_handler_factory: None,
        lsp_servers,
        auxiliary_model: turn.auxiliary_model.clone(),
        thread_persistence: builder::ThreadPersistence {
            store: thread_store,
            parent_thread_id: thread_id,
            register_runtime,
            deregister_runtime,
        },
        goal_controller,
        workflow_executor: workflow_executor.clone(),
        workflow_middleware: workflow_middleware.cloned(),
        background_registry: session_manager
            .as_ref()
            .and_then(|sm| sm.get_session(&session_id))
            .map(|s| s.background_registry.clone()),
    };

    // v2 stages 唯一路径（P5 后 v1 已物理删除，PERI_USE_V1 不再生效）。
    // v2 MessageQueue 已由 run_session_loop 解析并透传（避免重复解析 + 统一 bg_results/Path B 注入）。
    return build_and_execute_agent_v2(
        cfg,
        cached_llm,
        &pool,
        turn,
        agent_input,
        history,
        session_id,
        event_tx,
        event_sink,
        session_manager,
        v2_message_queue,
        async_router,
        idle_inbox,
        idle_should_wait,
    )
    .await;
}

/// 通过 [`crate::agent::builder_v2::build_stage_context`] 构造 StageContext，
/// 再由 [`peri_agent::agent::stages::run_react_loop`] 驱动循环（P5 后的单一执行路径）。
///
/// 关键设计：
/// - LLM/middleware 装配由 `build_agent` 完成（构造 `AgentComponents`）
/// - 工具执行由 `stages/tool_dispatch` 完成（每轮从 `shared_tools` 取）
/// - 事件出口：v2 stages 通过 EventBus emit 三层事件（Render/State/Observe），
///   本函数 spawn forwarder 将其映射为 `ExecutorEvent`，复用 event_tx / pump 管线
/// - 历史消息：seed 到 transcript；用户输入：作为 Prompt push 到 v2 queue
///
/// 调用前已完成 AcpAgentConfig 构造（含 register/deregister、event_handler、
/// workflow 消费者 spawn、goal_controller）。所有副作用与 v1 一致。
#[allow(clippy::too_many_arguments)]
// ── Prediction facade ───────────────────────────────────────────────────────

/// 预测失败原因，用于决定是否发送通知及日志级别。
#[derive(Debug)]
pub enum PredictionError {
    /// 30s 超时（首次冷启动可能较慢）。
    Timeout,
    /// Agent 执行返回错误。
    Failed(String),
}

/// Facade：基于现有对话历史预测用户下一步输入。
///
/// 此函数封装了 TUI 之前在 `acp_server/mod.rs` 内联的 Prediction 构造逻辑
/// （`BaseModelReactLLM::new` + `RetryableLLM::new`，直接调 `generate_reasoning`），
/// 避免违反 CLAUDE.md [TRAP]：
///
/// > Agent 构建和执行统一通过 `peri_acp::session::executor::run_session_loop()`。
/// > 禁止在 TUI 层直接构建 Agent。
///
/// 构建一个 1 轮、无工具、无中间件的最小 LLM 调用，注入 `history`（应已过滤 System
/// 消息并限制条数），30 秒超时后返回文本或 [`PredictionError`]。
///
/// 调用方负责发送 `peri/prediction_ready` 通知（保留在 TUI 层以便复用 transport）。
pub async fn execute_prediction(
    provider: crate::provider::LlmProvider,
    history: Vec<BaseMessage>,
    cwd: &str,
) -> Result<String, PredictionError> {
    debug!(
        msg_count = history.len(),
        cwd, "Prediction facade: starting"
    );

    // 直接复用已构建的 LlmProvider（绕过 from_config）
    let base_llm = peri_agent::llm::BaseModelReactLLM::new(provider.into_model());
    let llm = peri_agent::llm::RetryableLLM::new(base_llm, peri_agent::llm::RetryConfig::default());

    // execute_prediction 是 1-turn 无工具无中间件的最小 LLM 调用，
    // 不需要构造完整 v2 stages。直接构造 messages 调
    // ReactLLM::generate_reasoning 一次。
    let directive = peri_middlewares::subagent::build_prediction_directive();
    let mut messages: Vec<BaseMessage> = Vec::with_capacity(history.len() + 2);
    messages.push(BaseMessage::system(directive));
    for msg in &history {
        // 历史 System 已被调用方过滤（仅 Human/Ai/Tool），直接 append
        messages.push(msg.clone());
    }
    messages.push(BaseMessage::human("请根据以上对话预测用户下一步输入"));

    debug!("Prediction facade: calling LLM directly");
    // 30 秒超时（首次冷启动可能较慢）
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        llm.generate_reasoning(&messages, &[], None),
    )
    .await;

    match result {
        Ok(Ok(reasoning)) => {
            // 优先取 final_answer，回落到 source_message 文本
            let text = reasoning
                .final_answer
                .clone()
                .or_else(|| {
                    reasoning
                        .source_message
                        .as_ref()
                        .map(|m| m.content().to_string())
                })
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_default();
            if text.is_empty() {
                debug!("Prediction facade: LLM returned empty text");
            } else {
                debug!(%text, "Prediction facade: ready");
            }
            Ok(text)
        }
        Ok(Err(e)) => {
            debug!(error = %e, "Prediction facade: LLM failed");
            Err(PredictionError::Failed(e.to_string()))
        }
        Err(_) => {
            debug!("Prediction facade: timed out (30s)");
            Err(PredictionError::Timeout)
        }
    }
}

/// 从 agent 执行后的 state 中提取最后一条非空 AI 消息文本。
///
/// 纯函数（不持有 lock、不 await），便于单元测试。文本两侧空白会被裁剪。
pub fn extract_prediction_text(messages: &[BaseMessage]) -> String {
    messages
        .iter()
        .rev()
        .find_map(|m| {
            if matches!(m, BaseMessage::Ai { .. }) {
                let t = m.content();
                let trimmed = t.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            } else {
                None
            }
        })
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "executor_test.rs"]
mod tests;

#[cfg(test)]
#[path = "executor_prediction_test.rs"]
mod prediction_tests;

// 子流程 helper 子模块（EXECUTOR-SPLIT 选项 B）。
// executor.rs 是单文件而非目录，因此需 `#[path]` 显式指定同目录兄弟文件路径。
// 作为本模块的子模块，可直接访问本模块的私有项（struct/enum/use 引入的符号）。
#[path = "executor_helpers.rs"]
mod executor_helpers;
