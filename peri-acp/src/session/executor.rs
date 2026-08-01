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

use tokio::sync::oneshot as exec_oneshot;

use chrono::Local;
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

use peri_acp_types::event_data::PredictionAction;

use crate::{
    agent::builder::{self},
    langfuse::{LangfuseSession, LangfuseTracer},
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
// spawn_event_pump / SpawnPumpRequest / PumpHandle /
// collect_result / CollectRequest / close_channel / wait_for_pump /
// build_and_execute_agent_v2 在本模块命名空间可见——executor_test.rs 通过
// `super::` 访问的 helper 路径保持不变。
//
// 这些 helper 标 `pub(super)`（仅本模块可见）。
#[allow(unused_imports)]
use executor_helpers::{
    build_and_execute_agent_v2, close_channel, collect_result, intercept_immediate_command,
    spawn_event_pump, wait_for_pump, CollectRequest, InterceptRequest, PumpHandle,
    SpawnPumpRequest,
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
    /// Whether a Full Compact committed during this turn replaced the prior visible history.
    pub history_replaced_by_compaction: bool,
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
        permission_mode: peri_middlewares::prelude::PermissionMode,
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

        let features = crate::prompt::PromptFeatures::detect(permission_mode);
        let template = crate::prompt::PromptTemplate::new();
        let env = crate::prompt::PromptEnv::with_frozen_date(cwd, frozen_date);
        let system_prompt = template.render(&env, &features, plugin_agent_dirs, language);

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

    /// 会话创建时的语言偏好（如 "zh-CN"、"en"）。None 表示 auto-detect。
    pub fn language(&self) -> Option<&str> {
        self.v2_frozen.language.as_deref()
    }
}

/// Session-scoped context shared across all executor pipeline functions.
///
/// Replaces [`PromptExecutionContext`].
/// Fields grouped by subsystem for clarity.
#[allow(dead_code)]
pub struct SessionContext {
    // ── config: provider & global configuration ────────────────────────────
    pub provider: LlmProvider,
    pub peri_config: Arc<crate::provider::PeriConfig>,
    pub cwd: String,

    // ── session: session identity & transport ──────────────────────────────
    pub session_id: String,
    pub cancel: AgentCancellationToken,
    pub broker: Arc<dyn UserInteractionBroker>,
    pub permission_mode: Arc<peri_middlewares::prelude::SharedPermissionMode>,

    // ── infra: session-level infrastructure ────────────────────────────────
    pub session_manager: Option<SessionManager>,
    pub pool: Arc<parking_lot::Mutex<AgentPool>>,
    pub thread_store: Option<Arc<dyn peri_agent::thread::ThreadStore>>,
    pub thread_id: Option<String>,

    // ── middleware: middleware chain resources ─────────────────────────────
    pub plugin_skill_roots: Vec<peri_middlewares::skills::SkillRoot>,
    pub plugin_agent_dirs: Vec<std::path::PathBuf>,
    pub plugin_loaded: Vec<peri_middlewares::plugin::LoadedPlugin>,
    pub hook_groups: Vec<Vec<peri_middlewares::hooks::RegisteredHook>>,
    pub cron_scheduler: Option<Arc<parking_lot::Mutex<peri_middlewares::cron::CronScheduler>>>,
    pub mcp_pool: Option<Arc<peri_middlewares::mcp::McpClientPool>>,
    pub channel_state: Option<Arc<ChannelState>>,
    pub tool_search_index: Arc<peri_middlewares::tool_search::ToolSearchIndex>,
    pub shared_tools: Arc<
        parking_lot::RwLock<
            std::collections::BTreeMap<String, Arc<dyn peri_agent::tools::BaseTool>>,
        >,
    >,
    pub lsp_servers: Vec<peri_lsp::config::LspServerConfig>,

    // ── workflow: workflow agents ──────────────────────────────────────────
    pub workflow_executor: Option<Arc<dyn peri_workflow::runner::AgentExecutor>>,
    pub workflow_middleware: Option<Arc<peri_middlewares::workflow::WorkflowMiddleware>>,

    // ── turn: per-turn metadata ────────────────────────────────────────────
    pub session_start_source: Option<String>,

    // ── transport: transport-aware flags ───────────────────────────────────
    pub allow_await_wake: bool,

    /// v2 事件发送通道（替代原 event::v2_channel 全局 OnceLock）。
    /// TUI 入口置入，None 表示无 v2 消费方（如 stdio 模式）。
    pub v2_event_tx:
        Option<tokio::sync::mpsc::UnboundedSender<peri_agent::agent::events_v2_mapper::V2Event>>,
}

/// Per-turn computed configuration derived from [`SessionContext`].
///
/// Built once at the top of [`run_session_loop`], passed by reference to
/// [`build_and_execute_agent`] to avoid recomputing and to keep the agent
/// builder function signature manageable.
#[allow(dead_code)] // 字段逐步迁移到 SessionContext，完成后删除
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
    auxiliary_model: Option<Arc<dyn peri_model::Model>>,
    effective_context_window: u32,
}

/// Per-turn data passed alongside [`SessionContext`] to [`run_session_loop`].
///
/// Separated from session-level fields to clarify lifecycle: these values are
/// specific to a single prompt invocation and are not reused across turns.
pub struct TurnInput {
    /// 事件出口（TUI 用 TransportEventSink，stdio 用 StdioEventSink）。
    pub event_sink: Arc<dyn EventSink>,
    /// 用户本轮输入。
    pub content: MessageContent,
    /// 会话级 frozen 数据（system prompt 稳定性锚点）。
    pub frozen: Option<FrozenSessionData>,
    /// 现有历史消息（执行前）。
    pub history: Vec<BaseMessage>,
    /// 上一轮 recall 注入项。
    pub incoming_recalls: Vec<String>,
    /// 后台任务结果（注入合成的 AgentResult tool_use/tool_result）。
    pub bg_results: Vec<peri_agent::agent::events::BackgroundTaskResult>,
    /// Langfuse 会话级句柄（None 表示禁用遥测）。
    pub langfuse_session: Option<Arc<LangfuseSession>>,
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
pub async fn run_session_loop(ctx: SessionContext, turn: TurnInput) -> PromptResult {
    let TurnInput {
        event_sink,
        content,
        frozen,
        history,
        incoming_recalls,
        bg_results,
        langfuse_session,
    } = turn;

    // Compact config — computed early for command interception and agent building.
    let mut compact_config = ctx.peri_config.config.compact.clone().unwrap_or_default();
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
    let v2_message_queue = ctx
        .session_manager
        .as_ref()
        .and_then(|sm| sm.get_session(&ctx.session_id))
        .map(|s| s.v2_message_queue.clone())
        .unwrap_or_default();

    // 解析 session-level SessionInbox（await-wake wrapper）。
    // 用于：(1) executor idle 期间 await_wake 阻塞等待异步事件，
    // (2) AsyncRouter 推送 bg_results/workflow 事件时触发 wake。
    // None 表示不支持 async wake（如 print mode），保持向后兼容。
    let session_inbox = ctx
        .session_manager
        .as_ref()
        .and_then(|sm| sm.session_inbox_for(&ctx.session_id));

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
        let pool_guard = ctx.pool.lock();
        if pool_guard.has_valid_cache(&ctx.provider) {
            pool_guard.get_cached_llm().cloned()
        } else {
            None
        }
    };
    let auxiliary_model: Option<Arc<dyn peri_model::Model>> = if disable_compact {
        None
    } else {
        cached_llm
            .as_ref()
            .map(|c| c.auxiliary_model.clone())
            .or_else(|| Some(ctx.provider.clone().into_model().into()))
    };

    // Context window (前置计算，供 bg event pump 和 compact 使用)
    let context_window = ctx.provider.context_window();
    let context_1m = ctx.provider.context_1m();
    let effective_context_window = if context_1m {
        1_000_000
    } else {
        context_window
    };

    // 前置创建 bg 事件通道（BgCommand 等 Immediate 命令依赖）
    let (bg_event_tx_for_cmd, mut bg_event_rx_for_cmd) =
        tokio::sync::mpsc::unbounded_channel::<ExecutorEvent>();
    // session 级 registry（跨 prompt 存活，由 executor 从 session 获取）
    let bg_registry_for_cmd = ctx
        .session_manager
        .as_ref()
        .and_then(|sm| sm.get_session(&ctx.session_id))
        .map(|s| s.background_registry.clone())
        .unwrap_or_else(|| Arc::new(peri_middlewares::subagent::BackgroundTaskRegistry::new()));

    // BgCommand 事件的 bg event pump（必须在命令拦截之前启动，Immediate 命令才能发事件）
    {
        let bg_cmd_sink = Arc::clone(&event_sink);
        let bg_cmd_sid = ctx.session_id.clone();
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
                    bg_cmd_sink.push_done(&bg_cmd_sid, "end_turn").await;
                }
            }
        });
    }

    // Registry → ACP 事件泵：将 BgRegistryEvent 转换为 ACP unstable 事件
    {
        let (registry_event_tx, mut registry_event_rx) =
            tokio::sync::mpsc::unbounded_channel::<peri_middlewares::subagent::BgRegistryEvent>();
        bg_registry_for_cmd.set_event_sender(registry_event_tx, ctx.session_id.clone());
        let registry_sink = Arc::clone(&event_sink);
        let registry_sid = ctx.session_id.clone();
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
                        result: _result,
                    } => {
                        // route_bg_result 现在在 spawner 中同步执行
                        // （在 bg_registry.complete() 之前），
                        // 不再需要 registry 事件泵异步注入
                        tracing::info!(
                            task_id = %task_id,
                            "[bg-diag] registry event pump: Completed (route_bg_result now sync in spawner)"
                        );

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
        cwd: &ctx.cwd,
        session_id: &ctx.session_id,
        cancel: &ctx.cancel,
        peri_config: &ctx.peri_config,
        event_sink: &event_sink,
        auxiliary_model: &auxiliary_model,
        thread_store: ctx.thread_store.clone(),
        thread_id: ctx.thread_id.clone(),
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

    // 将会 move 的 middleware resources（无法借用，必须 move）。
    // turn 仍以引用形式借用 provider/peri_config/cwd/cancel/permission_mode/broker。
    let turn = TurnConfig {
        provider: &ctx.provider,
        peri_config: &ctx.peri_config,
        cwd: &ctx.cwd,
        frozen: frozen.as_ref(),
        language: frozen
            .as_ref()
            .and_then(|f| f.language().map(|s| s.to_string()))
            .or_else(|| ctx.peri_config.config.language.clone()),
        cancel: &ctx.cancel,
        permission_mode: &ctx.permission_mode,
        broker: &ctx.broker,
        session_start_source: ctx.session_start_source.clone(),
        auxiliary_model: auxiliary_model.clone(),
        effective_context_window,
    };

    // Lift Langfuse tracer creation to inject it into both
    // spawn_event_pump (pump head/tail) and build_and_execute_agent_v2 (forwarder).
    let langfuse_tracer: Option<Arc<parking_lot::Mutex<LangfuseTracer>>> =
        langfuse_session.as_ref().map(|s| {
            let session_clone = Arc::clone(s);
            let config = session_clone.config.clone();
            let session: std::sync::Arc<dyn crate::langfuse::LangfuseSessionLike> = session_clone;
            Arc::new(parking_lot::Mutex::new(LangfuseTracer::new(
                session,
                ctx.session_id.clone(),
                config,
            )))
        });

    // Main event pump
    let (stop_reason_tx, stop_reason_rx) = exec_oneshot::channel::<PromptStopReason>();
    let pump_handle = spawn_event_pump(SpawnPumpRequest {
        event_rx,
        stop_reason_rx,
        sink: Arc::clone(&event_sink),
        session_id: ctx.session_id.clone(),
        effective_context_window,
        langfuse_tracer: langfuse_tracer.clone(),
        trace_input: trace_input.to_string(),
    });

    // 把会 move/借用 的资源直接传入 build_and_execute_agent。
    // 由于 prompt builder 需要的所有资源都已提供，调用方后续不再访问这些已 move 字段
    // （session_id 在 collect_result 借用，此时 build_and_execute_agent 已完成）。
    let exec_outcome = build_and_execute_agent(
        &ctx,
        &turn,
        agent_input,
        history,
        event_sink,
        &ctx.session_id,
        &event_tx,
        cached_llm.as_ref(),
        &v2_message_queue,
        async_router.clone(),
        langfuse_tracer,
        bg_registry_for_cmd,
    )
    .await;

    // Send stop_reason to the event pump before it pushes done
    let _ = stop_reason_tx.send(exec_outcome.stop_reason);

    let result = collect_result(CollectRequest {
        event_tx: &event_tx,
        pump_handle,
        session_id: &ctx.session_id,
        exec_outcome,
    })
    .await;

    result
}

/// Agent 执行后的最终输出（state + 停止原因）。
struct ExecOutcome {
    ok: bool,
    stop_reason: PromptStopReason,
    /// A Full Compact committed during this turn and replaced prior visible history.
    history_replaced_by_compaction: bool,
    agent_state: AgentState,
}

/// 构建 + 执行 agent。包含：
/// - system prompt 解析（frozen 或 legacy 重建）
/// - SubAgentMiddleware register/deregister 闭包
/// - `build_agent` 调用 + AgentPool 缓存回写
/// - bg event pump + todo 转发 pump 启动
/// - `build_and_execute_agent_v2` 调用 + 错误事件转发
/// - cancel cascade 子 agent
#[allow(clippy::too_many_arguments)] // 收拢 AAC-only 字段，后续可进一步分组
async fn build_and_execute_agent(
    ctx: &SessionContext,
    turn: &TurnConfig<'_>,
    agent_input: peri_agent::agent::react::AgentInput,
    history: Vec<BaseMessage>,
    event_sink: Arc<dyn EventSink>,
    session_id: &str,
    event_tx: &Arc<parking_lot::Mutex<Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>>>,
    cached_llm: Option<&CachedLlmInstances>,
    v2_message_queue: &peri_agent::session::MessageQueue,
    async_router: Option<AsyncRouter>,
    langfuse_tracer: Option<Arc<parking_lot::Mutex<LangfuseTracer>>>,
    bg_registry: Arc<peri_middlewares::subagent::BackgroundTaskRegistry>,
) -> ExecOutcome {
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
        // 调用方未提供 frozen 数据时，在此一次性构建。
        // -p print mode 已在 cli_print.rs 迁移为提前构建 FrozenSessionData，
        // 此分支仅作防御性编程保留。
        let frozen_data = FrozenSessionData::build(
            turn.cwd,
            turn.language.as_deref(),
            &ctx.plugin_skill_roots,
            &ctx.plugin_agent_dirs,
            &Local::now().format("%Y-%m-%d").to_string(),
            peri_middlewares::prelude::PermissionMode::AutoMode,
        );
        (
            frozen_data.system_prompt().to_string(),
            frozen_data.claude_md().map(|s| s.to_string()),
            frozen_data.claude_local_md().map(|s| s.to_string()),
            frozen_data.skill_summary().map(|s| s.to_string()),
            Some(frozen_data.date().to_string()),
        )
    };

    // Build register/deregister closures for SubAgentMiddleware
    let register_runtime = ctx.session_manager.clone().map(|sm| {
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
    let deregister_runtime = ctx.session_manager.clone().map(|sm| {
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
    if let Some(wf_mw) = ctx.workflow_middleware.as_ref() {
        // 将 session 级 bg_registry 注入 WorkflowMiddleware（延迟注入，支持内部可变性）
        wf_mw.set_bg_registry(bg_registry.clone());

        // init_notification_buffer() 是 set-once gate：首次返回 true，后续返回 false。
        // WorkflowMiddleware 是 session 级实例（session/new 创建），
        // 因此每个 session 的消费者只 spawn 一次，无跨 session 污染。
        if wf_mw.init_notification_buffer() {
            let wf_mw_for_notify = Arc::clone(wf_mw);
            let notify_sink = Arc::clone(&event_sink);
            let notify_sid = session_id.to_string();
            let notify_cw = turn.effective_context_window;
            // AsyncRouter（v2 路径：push_defer + wake Notify）
            // 或回退 v2 queue clone（无 inbox 时直接 push，无 wake）
            let wf_router = async_router.clone();
            let fallback_queue = v2_message_queue.clone();
            // bg_registry 用于在 Defer 入队后递减 active_count，消除竞态窗口
            let notify_bg = bg_registry.clone();
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
                                    &task_result.phase_summaries,
                                );
                            } else {
                                // 回退：直接 push（无 wake，兼容无 inbox 场景）
                                let mut phase_lines = String::new();
                                for s in &task_result.phase_summaries {
                                    let token_info = if s.token_count > 0 {
                                        format!(", {} tokens", s.token_count)
                                    } else {
                                        String::new()
                                    };
                                    let dur_info = if let Some(d) = s.duration_ms {
                                        format!(", {}ms", d)
                                    } else {
                                        String::new()
                                    };
                                    phase_lines.push_str(&format!(
                                        "- {}: {} agents{}{}\n",
                                        s.name, s.agent_count, token_info, dur_info
                                    ));
                                }
                                // 不包裹 <system-reminder>：append_messages_to_transcript 统一包裹所有 Defer/Info
                                let notif_text = format!(
                                    "Workflow '{}' completed. ({}ms, {} agents, {} tool calls)\n\
                                    {}Results saved to .claude/workflow-runs/{}/state.json",
                                    task_result.workflow_name,
                                    task_result.duration_ms,
                                    task_result.agent_count,
                                    task_result.tool_calls_count,
                                    phase_lines,
                                    task_result.run_id,
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
                                    task_result.workflow_name,
                                    task_result.status,
                                    task_result.duration_ms,
                                    task_result.agent_count,
                                    task_result.tool_calls_count,
                                    task_result.run_id
                                ),
                                tool_calls_count: task_result.tool_calls_count,
                                duration_ms: task_result.duration_ms,
                                child_thread_id: None,
                            };
                            // 在 Defer 入队后递减 active_count，消除 tool.rs 通知 task 中的竞态窗口：
                            // 原实现在 registry.complete() broadcast 后立即调用 bg.complete_workflow()，
                            // 若 broadcast consumer 尚未被调度，agent 的 idle_should_wait probe
                            // (active_count > 0) 提前归零 → agent 退出 ReAct loop → Defer 堆积在队列中。
                            // [修复] 将 complete_workflow 移至 consumer 内 Defer push 之后执行。
                            notify_bg.complete(&task_result.run_id, bg.clone());
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
    let goal_controller: Option<Arc<dyn peri_agent::goal::GoalController>> = ctx
        .session_manager
        .as_ref()
        .and_then(|sm| sm.goal_state_for(session_id))
        .map(|gs| Arc::new(gs) as Arc<dyn peri_agent::goal::GoalController>);

    let thread_persistence = builder::ThreadPersistence {
        store: ctx.thread_store.clone(),
        parent_thread_id: ctx.thread_id.clone(),
        register_runtime,
        deregister_runtime,
    };

    let background_registry = ctx
        .session_manager
        .as_ref()
        .and_then(|sm| sm.get_session(session_id))
        .map(|s| s.background_registry.clone());

    let on_bg_complete = async_router.as_ref().map(|router| {
        let router = router.clone();
        Arc::new(move |result: &BackgroundTaskResult| {
            router.route_bg_result(result);
        }) as Arc<dyn Fn(&BackgroundTaskResult) + Send + Sync>
    });

    // v2 单一路径。
    return build_and_execute_agent_v2(
        ctx,
        cached_llm,
        agent_input,
        history,
        event_tx,
        &event_sink,
        langfuse_tracer,
        // ── AAC-only ──
        system_prompt,
        builder::FrozenData {
            claude_md: frozen_claude_md,
            claude_local_md: frozen_claude_local_md,
            skill_summary: frozen_skill_summary,
            date: frozen_date,
        },
        event_handler,
        None,       // agent_overrides
        Vec::new(), // preload_skills
        None,       // child_handler_factory
        turn.auxiliary_model.clone(),
        thread_persistence,
        goal_controller,
        background_registry,
        on_bg_complete,
        turn.effective_context_window,
    )
    .await;
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
/// （`AgentModelBridge::new` + `ReactLLM::generate_reasoning` 一次调用），
/// 避免违反 CLAUDE.md [TRAP]：
///
/// > Agent 构建和执行统一通过 `peri_acp::session::executor::run_session_loop()`。
/// > 禁止在 TUI 层直接构建 Agent。
///
/// 构建一个 1 轮、无工具、无中间件的最小 LLM 调用，注入 `history`（应已过滤 System
/// 消息并限制条数），30 秒超时后返回结构化动作列表或 [`PredictionError`]。
/// 模型输出经 [`parse_prediction_actions`] 解析为 `<peri:xxx>` 标记动作；
/// 无标记时回落为单个 `Placeholder` 动作（现有 placeholder 行为）。
///
/// `current_title` 为会话当前标题（`None` 表示无标题），注入指令后模型才能
/// 判断标题是否需要更新。
///
/// 调用方负责发送 `peri/prediction_ready` 通知（保留在 TUI 层以便复用 transport）。
pub async fn execute_prediction(
    provider: crate::provider::LlmProvider,
    history: Vec<BaseMessage>,
    cwd: &str,
    current_title: Option<&str>,
) -> Result<Vec<PredictionAction>, PredictionError> {
    debug!(
        msg_count = history.len(),
        cwd, "Prediction facade: starting"
    );

    // 直接复用已构建的 LlmProvider（绕过 from_config）
    let llm =
        peri_agent::agent::model_bridge::AgentModelBridge::new(Arc::from(provider.into_model()));

    // execute_prediction 是 1-turn 无工具无中间件的最小 LLM 调用，
    // 不需要构造完整 v2 stages。直接构造 messages 调
    // ReactLLM::generate_reasoning 一次。
    let directive = peri_middlewares::subagent::build_prediction_directive(current_title);
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
                Ok(Vec::new())
            } else {
                debug!(%text, "Prediction facade: ready");
                Ok(parse_prediction_actions(&text))
            }
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

/// 动作内容最大长度（字符数）
const MAX_ACTION_LEN: usize = 200;

/// 解析模型输出为结构化动作列表。
///
/// - 匹配 `<peri:(\w+)>(.*?)</peri:\1>` 标记（非贪婪，取第一个闭合）
/// - 未知标签忽略，其内容并入占位文本流
/// - 标记之间的纯文本片段（trim 后非空）收集为单个 Placeholder
/// - 同名动作后者覆盖前者
/// - 每个动作内容：剥离控制字符（含换行）、trim、截断 200 字符；空内容跳过
/// - 无任何标记/解析失败：整段回落为 Placeholder（现有行为）
pub fn parse_prediction_actions(text: &str) -> Vec<PredictionAction> {
    let mut actions: Vec<PredictionAction> = Vec::new();
    let mut plain_parts: Vec<String> = Vec::new();
    let mut cursor = 0usize;

    while let Some(rel_open) = text[cursor..].find("<peri:") {
        let open = cursor + rel_open;
        let Some(rel_gt) = text[open..].find('>') else {
            break;
        };
        let tag_end = open + rel_gt;
        let tag = &text[open + "<peri:".len()..tag_end];
        if !tag_is_valid(tag) {
            cursor = open + 1;
            continue;
        }
        let closing = format!("</peri:{tag}>");
        let content_start = tag_end + 1;
        let Some(rel_close) = text[content_start..].find(&closing) else {
            break; // 未闭合：剩余全部按纯文本
        };
        let content_end = content_start + rel_close;
        let whole_tag_end = content_end + closing.len();

        if open > cursor {
            plain_parts.push(text[cursor..open].to_string());
        }
        // 未知标签：标记剥除，仅内容并入占位文本
        if !matches!(tag, "title" | "tag" | "summary") {
            plain_parts.push(text[content_start..content_end].to_string());
            cursor = whole_tag_end;
            continue;
        }
        let action = match tag {
            "title" => sanitize_action_content(&text[content_start..content_end])
                .map(|content| PredictionAction::SetTitle { title: content }),
            "tag" => sanitize_action_content(&text[content_start..content_end])
                .map(|content| PredictionAction::AddTag { tag: content }),
            "summary" => sanitize_action_content(&text[content_start..content_end])
                .map(|content| PredictionAction::Summary { text: content }),
            _ => unreachable!("已知标签已在上面过滤"),
        };
        if let Some(action) = action {
            push_replace_action(&mut actions, action);
        }
        cursor = whole_tag_end;
    }
    if cursor < text.len() {
        plain_parts.push(text[cursor..].to_string());
    }

    let placeholder = plain_parts
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if !placeholder.is_empty() {
        actions.insert(0, PredictionAction::Placeholder { text: placeholder });
    }
    actions
}

/// 标签名仅允许 ASCII 字母数字，防止任意闭合注入
fn tag_is_valid(tag: &str) -> bool {
    !tag.is_empty() && tag.chars().all(|c| c.is_ascii_alphanumeric())
}

/// 剥离控制字符（含换行）、trim、截断；空内容返回 None（跳过动作）
fn sanitize_action_content(s: &str) -> Option<String> {
    let cleaned: String = s.chars().filter(|c| !c.is_control()).collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.chars().take(MAX_ACTION_LEN).collect())
    }
}

/// 同名（同变体）动作后者覆盖前者
fn push_replace_action(actions: &mut Vec<PredictionAction>, action: PredictionAction) {
    let disc = std::mem::discriminant(&action);
    if let Some(pos) = actions
        .iter()
        .position(|a| std::mem::discriminant(a) == disc)
    {
        actions[pos] = action;
    } else {
        actions.push(action);
    }
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
