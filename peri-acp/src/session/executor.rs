//! Shared prompt execution logic.
//!
//! Provides [`run_session_loop`] which encapsulates the common agent execution
//! pipeline used by both TUI (via [`TransportEventSink`]) and stdio (via
//! [`StdioEventSink`]) paths.
//!
//! Compact 由 v2 `stages/compact.rs`（`run_react_loop` 在每轮开头调
//! `compact_v2::run_compact`）统一处理，不再需要外层 loop + resubmit，
//! 也不再经过 CompactMiddleware。

use std::sync::Arc;

use peri_agent::{
    agent::{
        events::{AgentEventHandler, BackgroundTaskResult, ExecutorEvent},
        react::ReactLLM,
        state::AgentState,
        AgentCancellationToken,
    },
    error::AgentError,
    interaction::{ChannelState, UserInteractionBroker},
    messages::{BaseMessage, ContentBlock, MessageContent},
    session::queue::QueuedMessage,
};
use tokio::sync::oneshot;
use tracing::{debug, error};

use crate::event::mapper_v2::{
    observe_event_to_executor, render_event_to_executor, state_event_to_executor,
};
use crate::{
    agent::builder::{self, AcpAgentConfig},
    langfuse::{LangfuseSession, LangfuseTracer},
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

    // 前置创建 bg 通道（BgCommand 等 Immediate 命令依赖）
    let (bg_event_tx_for_cmd, mut bg_event_rx_for_cmd) =
        tokio::sync::mpsc::unbounded_channel::<ExecutorEvent>();
    let (bg_notification_tx_for_cmd, _bg_notification_rx_for_cmd) =
        tokio::sync::mpsc::unbounded_channel();
    let bg_registry_for_cmd = Arc::new(peri_middlewares::subagent::BackgroundTaskRegistry::new(
        bg_notification_tx_for_cmd,
    ));

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
        event_sink: &event_sink,
        session_id: &session_id,
        event_tx: &event_tx,
        cached_llm: cached_llm.as_ref(),
        v2_message_queue: &v2_message_queue,
        async_router: async_router.clone(),
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

// ── Intercept Request parameter object ─────────────────────────────────────

/// 命令拦截请求（参数对象，避免 12 个位置参数）。
struct InterceptRequest<'a> {
    content: &'a MessageContent,
    history: &'a [BaseMessage],
    cwd: &'a str,
    session_id: &'a str,
    cancel: &'a AgentCancellationToken,
    peri_config: &'a Arc<crate::provider::PeriConfig>,
    event_sink: &'a Arc<dyn EventSink>,
    auxiliary_model: &'a Option<Arc<dyn peri_agent::llm::BaseModel>>,
    thread_store: Option<Arc<dyn peri_agent::thread::ThreadStore>>,
    thread_id: Option<String>,
    bg_event_tx: &'a tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
    bg_registry: &'a Arc<peri_middlewares::subagent::BackgroundTaskRegistry>,
    frozen: Option<&'a FrozenSessionData>,
}

/// 命令拦截：检查 content 是否为 Immediate 类型 slash 命令。
///
/// 返回 `Some(PromptResult)` 表示已处理（agent 不构建）；
/// 返回 `None` 表示继续走 agent 管线。
///
/// [TRAP] Immediate 命令路径绕过 agent event pump，必须手动调用 `sink.push_done()`。
/// 否则 TUI 界面永久卡在 loading 状态（issue_2026-05-29-immediate-command-missing-push-done）。
async fn intercept_immediate_command(req: InterceptRequest<'_>) -> Option<PromptResult> {
    let text = req.content.text_content();
    let stripped = text.strip_prefix('/')?;
    if stripped.is_empty() {
        return None;
    }

    let command_registry = crate::session::command::default_command_registry();
    let (cmd, args) = command_registry.find(&text)?;
    if cmd.kind() != crate::session::command::CommandKind::Immediate {
        // Passthrough/Transform → fall through to normal agent flow
        return None;
    }

    tracing::debug!(
        command = %cmd.name(),
        history_len = req.history.len(),
        "Immediate command intercepted"
    );
    let ctx = crate::session::command::CommandContext {
        session_id: req.session_id.to_string(),
        history: req.history.to_vec(),
        cwd: req.cwd.to_string(),
        peri_config: Arc::new(req.peri_config.as_ref().clone()),
        auxiliary_model: req.auxiliary_model.clone(),
        event_sink: req.event_sink.clone(),
        args: args.to_string(),
        cancel_token: req.cancel.clone(),
        thread_store: req.thread_store,
        thread_id: req.thread_id,
        bg_event_sender: Some(req.bg_event_tx.clone()),
        bg_registry: Some(req.bg_registry.clone()),
        frozen_claude_md: req
            .frozen
            .as_ref()
            .and_then(|f| f.claude_md().map(|s| Arc::new(s.to_string()))),
        frozen_claude_local_md: req
            .frozen
            .as_ref()
            .and_then(|f| f.claude_local_md().map(|s| Arc::new(s.to_string()))),
        frozen_skill_summary: req
            .frozen
            .as_ref()
            .and_then(|f| f.skill_summary().map(|s| Arc::new(s.to_string()))),
    };
    let result = tokio::select! {
        r = cmd.execute(ctx) => r,
        _ = req.cancel.cancelled() => {
            tracing::info!(session_id = %req.session_id, "Immediate command cancelled");
            crate::session::command::CommandResult {
                messages: req.history.to_vec(),
                stop_reason: PromptStopReason::Cancelled,
            }
        }
    };
    // Immediate 命令跳过 agent event pump，必须手动发送 push_done
    // 通知 TUI agent 执行完成，否则界面永久卡在 loading 状态。
    req.event_sink.push_done(req.session_id).await;
    Some(PromptResult {
        messages: result.messages,
        ok: true,
        stop_reason: result.stop_reason,
        recall_items: Vec::new(),
    })
}

// ── Spawn Pump Request parameter object ─────────────────────────────────────

/// 事件泵启动请求（参数对象）。
struct SpawnPumpRequest {
    event_rx: tokio::sync::mpsc::UnboundedReceiver<ExecutorEvent>,
    sink: Arc<dyn EventSink>,
    session_id: String,
    effective_context_window: u32,
    langfuse_session: Option<Arc<LangfuseSession>>,
    trace_input: String,
    provider_display_name: String,
}

/// 后台事件泵句柄，通过 oneshot channel 与 pump_done_rx 配对。
struct PumpHandle {
    pump_done_rx: oneshot::Receiver<()>,
}

/// 启动主事件泵任务。
///
/// 任务循环：
/// 1. trace_start → recv events → forward to sink
/// 2. trace_end + push_done → signal pump completion（在 Langfuse flush 之前）
/// 3. Langfuse flush（fire-and-forget，不得阻塞管线）
fn spawn_event_pump(req: SpawnPumpRequest) -> PumpHandle {
    let SpawnPumpRequest {
        mut event_rx,
        sink,
        session_id,
        effective_context_window,
        langfuse_session,
        trace_input,
        provider_display_name,
    } = req;

    let (pump_done_tx, pump_done_rx) = oneshot::channel();

    let langfuse_tracer = langfuse_session
        .as_ref()
        .map(|s| parking_lot::Mutex::new(LangfuseTracer::new(Arc::clone(s), session_id.clone())));
    if langfuse_tracer.is_some() {
        debug!(session_id = %session_id, "Langfuse tracer created for turn");
    }

    tokio::spawn(async move {
        // Start Langfuse trace
        if let Some(ref tracer) = langfuse_tracer {
            tracer.lock().on_trace_start(&trace_input);
        }

        while let Some(exec_event) = event_rx.recv().await {
            // Langfuse tracing
            if let Some(ref tracer) = langfuse_tracer {
                forward_langfuse_event(tracer, &exec_event, &provider_display_name);
            }

            sink.push_event(&session_id, &exec_event, effective_context_window)
                .await;
        }

        // End Langfuse trace and flush
        let langfuse_flush = if let Some(tracer) = langfuse_tracer {
            let handle = tracer.into_inner().on_trace_end(None);
            Some(handle)
        } else {
            None
        };

        // Emit turn-done as an unstable event so the TUI v2 state machine
        // can transition Streaming → Idle. Must come before push_done so
        // TurnDone arrives before AgentDone in the notification channel.
        sink.push_unstable_event(&session_id, "turn-done".into(), serde_json::json!({}))
            .await;
        sink.push_done(&session_id).await;

        // Signal pump completion BEFORE Langfuse flush.
        // Langfuse is telemetry — it must never block the execution pipeline.
        // Without this, a slow/unreachable Langfuse API blocks pump_done_tx,
        // which blocks wait_for_pump(), which blocks run_session_loop() from
        // returning, which holds the prompt_lock and prevents the next prompt
        // from starting. Ctrl+C can't recover because the new prompt's cancel
        // token hasn't been created yet (still waiting on the lock).
        let _ = pump_done_tx.send(());

        // Langfuse flush: fire-and-forget. The spawned task runs independently;
        // worst-case it blocks for ~150s (HTTP 30s × 3 retries + backoff) then
        // logs warnings. The pump has already signaled completion above, so this
        // never blocks the execution pipeline.
        drop(langfuse_flush);
    });

    PumpHandle { pump_done_rx }
}

/// 转发单个 executor 事件到 Langfuse tracer（pump 内的纯函数，便于测试）。
pub(crate) fn forward_langfuse_event(
    tracer: &parking_lot::Mutex<LangfuseTracer>,
    exec_event: &ExecutorEvent,
    provider_display_name: &str,
) {
    match exec_event {
        ExecutorEvent::LlmCallStart {
            step,
            messages,
            tools,
        } => {
            tracer.lock().on_llm_start(*step, messages, tools);
        }
        ExecutorEvent::LlmRequestPayload { step, body } => {
            tracer
                .lock()
                .on_llm_request_payload(*step, std::sync::Arc::clone(body));
        }
        ExecutorEvent::LlmCallEnd {
            step,
            model,
            output,
            usage,
            stop_reason: _,
        } => {
            tracer
                .lock()
                .on_llm_end(*step, model, provider_display_name, output, usage.as_ref());
        }
        ExecutorEvent::ToolStart {
            tool_call_id,
            name,
            input,
            ..
        } => {
            tracer.lock().on_tool_start(tool_call_id, name, input);
        }
        ExecutorEvent::ToolEnd {
            tool_call_id,
            output,
            is_error,
            ..
        } => {
            tracer.lock().on_tool_end(tool_call_id, output, *is_error);
        }
        ExecutorEvent::TextChunk { chunk, .. } => {
            tracer.lock().on_text_chunk(chunk);
        }
        ExecutorEvent::LlmRetrying {
            attempt,
            max_attempts,
            delay_ms,
            error,
        } => {
            tracer
                .lock()
                .on_llm_retrying(*attempt, *max_attempts, *delay_ms, error);
        }
        ExecutorEvent::CompactStarted => {
            tracer.lock().on_compact_start();
        }
        ExecutorEvent::CompactCompleted {
            summary,
            files,
            skills,
            micro_cleared,
            ..
        } => {
            tracer.lock().on_compact_end(
                summary,
                files.len(),
                skills.len(),
                *micro_cleared,
                false,
                "",
            );
        }
        ExecutorEvent::CompactError { message } => {
            tracer.lock().on_compact_end("", 0, 0, 0, true, message);
        }
        _ => {}
    }
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
        event_sink,
        session_id,
        event_tx,
        cached_llm,
        v2_message_queue,
        async_router,
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
async fn build_and_execute_agent_v2(
    cfg: AcpAgentConfig,
    cached_llm: Option<&CachedLlmInstances>,
    pool: &Arc<parking_lot::Mutex<AgentPool>>,
    turn: &TurnConfig<'_>,
    agent_input: peri_agent::agent::react::AgentInput,
    history: Vec<BaseMessage>,
    session_id: &str,
    event_tx: &Arc<parking_lot::Mutex<Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>>>,
    event_sink: &Arc<dyn EventSink>,
    session_manager: Option<SessionManager>,
    v2_queue: &peri_agent::session::MessageQueue,
    _async_router: Option<AsyncRouter>,
) -> ExecOutcome {
    use peri_agent::agent::stages::{run_react_loop, LoopResult};
    use peri_agent::session::queue::{
        MessageKind, MessageSource as V2MessageSource, QueuedMessage,
    };

    // Phase 1: build StageContext（内部消费 AgentComponents；传入会话级共享 v2_queue）
    let (v2_out, new_cache) =
        crate::agent::builder_v2::build_stage_context(cfg, cached_llm, pool, v2_queue);
    if let Some(cache) = new_cache {
        pool.lock().store_llm(cache);
    }

    // Phase 2: bg event pump（复用 V2AgentOutput.bg_event_rx）
    {
        let mut bg_event_rx = v2_out.bg_event_rx;
        let bg_session_id = session_id.to_string();
        let bg_sink = Arc::clone(event_sink);
        let bg_cw = turn.effective_context_window;
        tokio::spawn(async move {
            let mut bg_event_count: u64 = 0;
            while let Some(bg_event) = bg_event_rx.recv().await {
                bg_event_count += 1;
                bg_sink.push_event(&bg_session_id, &bg_event, bg_cw).await;
            }
            tracing::debug!(
                total = bg_event_count,
                "bg-event-pump: all senders dropped, exiting"
            );
        });
    }

    // Phase 3: todo forwarder（同 v1，复用 V2AgentOutput.todo_rx）
    {
        let mut todo_rx = v2_out.todo_rx;
        let tx_for_todo = event_tx.clone();
        tokio::spawn(async move {
            while let Some(todos) = todo_rx.recv().await {
                let entries: Vec<peri_agent::agent::events::TodoEntry> = todos
                    .into_iter()
                    .map(|t| peri_agent::agent::events::TodoEntry {
                        content: t.content,
                        active_form: t.active_form,
                        status: match t.status {
                            peri_middlewares::tools::todo::TodoStatus::Pending => {
                                peri_agent::agent::events::TodoStatus::Pending
                            }
                            peri_middlewares::tools::todo::TodoStatus::InProgress => {
                                peri_agent::agent::events::TodoStatus::InProgress
                            }
                            peri_middlewares::tools::todo::TodoStatus::Completed => {
                                peri_agent::agent::events::TodoStatus::Completed
                            }
                        },
                    })
                    .collect();
                if let Some(tx) = tx_for_todo.lock().as_ref() {
                    let _ = tx.send(ExecutorEvent::TodoUpdate(entries));
                }
            }
        });
    }

    // Phase 4: EventBus forwarder（v2 → v1 ExecutorEvent）
    // 通过 tokio::select! 同时排空 render / state / observe 三层通道，
    // 将 v2 事件经 mapper_v2 映射为 v1 ExecutorEvent，转发到 event_tx。
    //
    // 注意：不直接 push 到 event_sink —— spawn_event_pump 已订阅 event_tx 并
    // 负责推送 sink（含 Langfuse trace + pump_done 同步）。直推会造成 TUI 双重渲染。
    //
    // [TRAP] TurnCompleted 在 render_tx 通道（与同迭代 TextChunk/ToolStarted/
    // ToolEnded 共享 FIFO），不能放回 state_tx：跨通道 biased select! 只保证
    // 单次迭代内的优先级，不保证跨迭代——iter2 的 TextChunk 会先于 iter1 的
    // TurnCompleted 被消费，污染 partial，渲染出"新文本在旧工具之前"的错乱。
    {
        let mut handles = v2_out.event_handles;
        let tx_for_v2 = event_tx.clone();
        tokio::spawn(async move {
            loop {
                // biased + render 优先：保证 Render 通道（含 TurnCompleted）
                // 先于 State 通道被消费。State 通道仅剩 StateSnapshot，无顺序耦合。
                tokio::select! {
                    biased;
                    Some(ev) = handles.render_rx.recv() => {
                        if let Some(exec_ev) = render_event_to_executor(ev) {
                            if let Some(tx) = tx_for_v2.lock().as_ref() {
                                let _ = tx.send(exec_ev);
                            }
                        }
                    }
                    Some(ev) = handles.state_rx.recv() => {
                        if let Some(exec_ev) = state_event_to_executor(ev) {
                            if let Some(tx) = tx_for_v2.lock().as_ref() {
                                let _ = tx.send(exec_ev);
                            }
                        }
                    }
                    ev_res = handles.observe_rx.recv() => {
                        match ev_res {
                            Ok(ev) => {
                                if let Some(exec_ev) = observe_event_to_executor(ev) {
                                    if let Some(tx) = tx_for_v2.lock().as_ref() {
                                        let _ = tx.send(exec_ev);
                                    }
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!(
                                    n,
                                    "[v2] observe_rx lagged, events dropped"
                                );
                            }
                        }
                    }
                    else => break,
                }
            }
        });
    }

    // Phase 5: seed transcript（history 作为 ancestor 之外的自有消息）
    {
        let transcript_arc = v2_out.session.transcript();
        let mut transcript = transcript_arc.write();
        transcript.append_batch(history);
    }

    // Phase 6: push 用户输入到 v2 queue（Receive 阶段消费）
    v2_out.context.queue.push(QueuedMessage::new(
        MessageKind::Prompt,
        V2MessageSource::UserInput,
        BaseMessage::human(agent_input.content),
    ));

    // Phase 6.5: clone recall_buffer 的 Arc，便于 Phase 8.5 在 context 被
    // run_react_loop 消费后仍可访问累积的 recall。
    let recall_buffer = Arc::clone(&v2_out.context.recall_buffer);

    // Phase 6.7: run before_agent middleware hooks
    // v1 在 execute() 开头调用 chain.run_before_agent，让 AgentsMd/Skills/
    // ToolSearch 等中间件缓存贡献数据。v2 在 run_react_loop 前调用以保持兼容。
    if let Err(e) =
        peri_agent::agent::stages::middleware_runner::run_before_agent(&v2_out.context).await
    {
        tracing::warn!(error = %e, "[v2] before_agent hook failed");
    }

    // Phase 7: 运行 v2 ReAct 循环（max_iterations 与 v1 一致 = 500）
    let loop_result = run_react_loop(v2_out.context, 500).await;

    // Phase 8: 从 transcript 提取最终消息列表，构造 AgentState（兼容下游 PromptResult）
    let messages: Vec<BaseMessage> = v2_out
        .session
        .transcript()
        .read()
        .visible_messages()
        .into_iter()
        .cloned()
        .collect();
    let mut agent_state = AgentState::with_messages(turn.cwd.to_string(), messages);
    agent_state.set_context("session_id", session_id);
    agent_state.set_context("run_id", uuid::Uuid::now_v7().to_string());

    // Phase 8.5: 把 v2 recall_buffer（middleware hook 期间累积）灌入 agent_state。
    // 下游 collect_result() 调用 agent_state.drain_recall() 取出 recall_items，
    // 必须先迁移到 agent_state 才能复用 v1 的 drain 路径。
    //
    // v2 路径下 middleware hook 在临时 AgentState 上 push_recall（见
    // middleware_runner::restore_from_agent_state），restore 时 drain 到
    // StageContext.recall_buffer；循环结束后（context 已被 run_react_loop
    // 消费）从 Phase 6.5 clone 的 Arc 取回累积的 recall。
    {
        let recalls: Vec<String> = recall_buffer.write().drain(..).collect();
        for r in recalls {
            agent_state.push_recall(r);
        }
    }

    // Phase 9: 映射 LoopResult → ExecOutcome
    let (ok, stop_reason) = match loop_result {
        LoopResult::Completed => (true, PromptStopReason::EndTurn),
        LoopResult::Interrupted => (false, PromptStopReason::Cancelled),
        LoopResult::Error(ref e) => {
            error!(session_id = %session_id, error = %e, "[v2] loop failed");
            if let Some(tx) = event_tx.lock().as_ref() {
                let _ = tx.send(ExecutorEvent::AgentExecutionFailed {
                    message: e.to_string(),
                });
            }
            let reason = if turn.cancel.is_cancelled() || matches!(e, AgentError::Interrupted) {
                PromptStopReason::Cancelled
            } else if matches!(e, AgentError::MaxIterationsExceeded(_)) {
                PromptStopReason::MaxTurnRequests
            } else {
                PromptStopReason::EndTurn
            };
            (false, reason)
        }
    };

    // Cancel cascade children when this agent is cancelled
    if stop_reason == PromptStopReason::Cancelled {
        if let Some(ref sm) = session_manager {
            if let Some(session) = sm.get_session(session_id) {
                session.cancel_cascade_children();
            }
        }
    }

    ExecOutcome {
        ok,
        stop_reason,
        agent_state,
    }
}

// ── Collect Result Request parameter object ─────────────────────────────────

/// 结果收集请求（参数对象）。
struct CollectRequest<'a> {
    event_tx:
        &'a Arc<parking_lot::Mutex<Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>>>,
    pump_handle: PumpHandle,
    session_id: &'a str,
    exec_outcome: ExecOutcome,
}

/// 最终结果收集：close channel → 等待 pump drain → 提取 recall items。
///
/// 顺序约束：必须先 close event_tx，pump 才能退出 recv 循环；然后等待 pump_done。
async fn collect_result(req: CollectRequest<'_>) -> PromptResult {
    let CollectRequest {
        event_tx,
        pump_handle,
        session_id,
        mut exec_outcome,
    } = req;

    close_channel(event_tx);
    wait_for_pump(pump_handle.pump_done_rx, session_id).await;

    let recall_items = exec_outcome.agent_state.drain_recall();
    PromptResult {
        messages: exec_outcome.agent_state.into_messages(),
        ok: exec_outcome.ok,
        stop_reason: exec_outcome.stop_reason,
        recall_items,
    }
}

fn close_channel(
    event_tx: &Arc<parking_lot::Mutex<Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>>>,
) {
    let mut tx_guard = event_tx.lock();
    *tx_guard = None;
}

async fn wait_for_pump(pump_done_rx: oneshot::Receiver<()>, session_id: &str) {
    match tokio::time::timeout(std::time::Duration::from_secs(10), pump_done_rx).await {
        Ok(Ok(())) => debug!(session_id, "Event pump done"),
        Ok(Err(_)) => error!(session_id, "Event pump done channel closed unexpectedly"),
        Err(_) => error!(
            session_id,
            "Event pump timed out (10s) — Langfuse flush may have blocked push_done"
        ),
    }
}

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
