//! Shared prompt execution logic（L5：自 `peri-acp/src/host/exec/executor.rs`
//! 物理迁入，ACP 侧 `crate::session::executor` 薄壳 re-export 保兼容）。
//!
//! Provides [`run_session_loop`] which encapsulates the common agent execution
//! pipeline used by both TUI (via [`TransportEventSink`]) and stdio (via
//! [`StdioEventSink`]) paths.
//!
//! Compact 由 v2 `stages/compact.rs`（`run_react_loop` 在每轮开头调
//! `compact_v2::run_compact`）统一处理，不再需要外层 loop + resubmit，
//! 也不再经过 CompactMiddleware。
//!
//! # 文件结构（EXECUTOR-SPLIT 选项 B + L5 迁出）
//!
//! 本文件是 orchestrator，仅保留：
//! - 共享类型：`PromptStopReason` / `PromptResult` / `FrozenSessionData`
//!   / `SessionContext` / `TurnInput` / `TurnConfig` / `LangfuseHooks`
//! - 入口：`run_session_loop`（编排）+ `build_and_execute_agent`（cfg 组装与 v2 dispatch）
//! - Prediction facade：`execute_prediction` / `extract_prediction_text`
//!
//! 子流程已随 L5 迁入本 crate `session::exec::executor_helpers`：
//! - [`intercept_immediate_command`]：slash 命令拦截
//! - [`spawn_event_pump`]：后台事件泵 + Langfuse tracer（注入闭包）
//! - [`build_and_execute_agent_v2`]：v2 stages 装配与 ReAct 循环驱动（9 个 phase）
//! - [`collect_result`] / [`close_channel`] / [`wait_for_pump`]：结果收集
//!
//! 本模块经下方 use 块把 helper 提升到本模块命名空间，使 `executor_test.rs`
//! 的 `super::{intercept_immediate_command, InterceptRequest}` 路径继续可解析。
//!
//! # 依赖反转（§0）
//!
//! 本模块只依赖 peri-acp-types / peri-model / crate 内部：
//! - `provider` / `peri_config` / `AgentPool` / `SessionManager` / `Controller`
//!   五个 ACP 特有字段端口化为投影值 + 注入闭包 + [`SessionAccessPort`] /
//!   [`EventPublisher`] / [`EventSubscriber`] 端口（ACP 宿主装配面构造）；
//! - 事件发射/订阅经契约端口（[`EventPublisher`] / [`EventSubscriber`] 适配层
//!   在 ACP 宿主侧），命令拦截 / stage 装配 / Langfuse / cancel cascade
//!   全部经注入面接入；
//! - Langfuse 遥测经 [`LangfuseHooks`]（on_turn_start / on_turn_end /
//!   bridge_factory 闭包，ACP 宿主从 `LangfuseSession` 构造）；
//! - stage 装配桥（[`StageBuildFn`]）与 EventBus forwarder 启动器
//!   （[`ForwarderLauncherFn`]）由 ACP 宿主构造后经 [`TurnInput`] 注入。
//!
//! ## Cancel 语义保持
//!
//! - `intercept_immediate_command` 内的 `tokio::select!` 分支顺序原样保留
//!   （`cmd.execute` 与 `cancel.cancelled()` 仍按原 biased 顺序，二者均触发 push_done）
//! - `build_and_execute_agent_v2` 末尾的 cancel cascade 仍在循环失败后触发，
//!   `LoopResult::Error` 分支先发 `AgentExecutionFailed` 事件再判断 stop_reason
//! - `collect_result` 严格 "close → wait_for_pump(10s timeout) → drain recall"

use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};

use tokio::sync::oneshot as exec_oneshot;

use chrono::Local;
use peri_acp_types::{
    event::{AgentEventHandler, BackgroundTaskResult, ExecutorEvent},
    frozen::{FrozenData, ThreadPersistence},
    interaction::{ChannelState, UserInteractionBroker},
    messages::{BaseMessage, ContentBlock, MessageContent},
    session::{MessageQueue, QueuedMessage, SessionAccessPort},
};
use tokio_util::sync::CancellationToken as AgentCancellationToken;
use tracing::debug;

use peri_acp_types::event_data::PredictionAction;
use peri_acp_types::permission::PermissionMode;
use peri_acp_types::tasks::{BgRegistryEvent, BgTaskKind, TaskManager};

use crate::agent::langfuse_bridge::LangfuseBridgeLike;
use crate::agent::react::{AgentInput, ReactLLM};
use crate::session::{
    async_router::AsyncRouter, exec::stage_builder::CachedLlmInstances,
    subagent::SubagentChainAssembler,
};
use crate::tools::ToolInvocationResolver;

// 子流程 helper 同 crate 迁移（`session::exec::executor_helpers`）：
// intercept_immediate_command / InterceptRequest / spawn_event_pump /
// SpawnPumpRequest / PumpHandle / collect_result / CollectRequest /
// close_channel / wait_for_pump / build_and_execute_agent_v2 /
// V2ExecuteRequest / StageBuildFn / ExecOutcome / ModeNoticeBooking /
// mark_permission_mode_notified / DefaultBgForkSpawner —— executor_test.rs
// 通过 `super::` 访问的符号路径保持不变。
pub use crate::session::exec::executor_helpers::{
    build_and_execute_agent_v2, close_channel, collect_result, intercept_immediate_command,
    mark_permission_mode_notified, spawn_event_pump, wait_for_pump, CollectRequest,
    CommandLookupFn, DefaultBgForkSpawner, ExecOutcome, ForwarderLauncherFn, InterceptRequest,
    ModeNoticeBooking, ParentToolsFactory, PumpHandle, SpawnPumpRequest, StageBuildFn,
    V2ExecuteRequest,
};

/// High-level reason why prompt execution stopped, used to derive ACP `StopReason`.
///
/// L5 契约化：事实源 `peri-acp-types::command::PromptStopReason`。
pub use peri_acp_types::command::PromptStopReason;

/// bg 完成 → ACP server continuation scheduler 的通知请求。
///
/// 由 executor 的 `on_bg_complete` 闭包在 [`AsyncRouter::route_bg_result`] 之后发送：
/// 先确保 deferred callback 已写入 SessionInbox，再通知 scheduler。
/// scheduler 按 session 原子 take `session/cancel` 置位的标记后运行一次内部
/// AsyncContinuation（见 peri-tui/src/acp_server/continuation.rs）。
#[derive(Debug, Clone)]
pub struct ContinuationRequest {
    pub session_id: String,
    pub kind: BgTaskKind,
}

/// Result of prompt execution.
///
/// L5 契约化：事实源 `peri-acp-types::session::PromptResult`。
pub use peri_acp_types::session::PromptResult;

/// keepgoing 判定：内容按 block 判空（`MessageContent::is_empty`）。
///
/// 这是 TUI keepgoing 按钮 ↔ ACP ↔ agent stages 的**跨层共享判定**：
/// - 此处：空 prompt → 跳过 recall 注入（`run_session_loop`）
/// - `peri-agent` stages：空 Prompt → 不写入 transcript（`append_messages_to_transcript`）
///
/// 必须与 stages 层保持同一语义。用 `is_empty()`（按 content block 判空）而非
/// `text_content().trim()`——后者会把 `Blocks([Image])` 这类纯附件消息误判为
/// keepgoing（图片接入后即触发），且畸形请求经 `extract_prompt_params` 默认值
/// （空文本）落入 keepgoing 路径时行为一致。
///
/// 协议约定（见 docs/standards/architecture-contracts.md ARC-KEEPGOING-001）：
/// 空白 user prompt = "继续跑 loop"，唯一生产者为 TUI keepgoing 按钮。
pub fn is_keepgoing(content: &peri_acp_types::messages::MessageContent) -> bool {
    content.is_empty()
}

/// Session-scoped frozen data that locks system prompt stability.
///
/// Populated at session creation time by `session/new`, passed through to
/// every turn's agent build to guarantee the system prompt never changes
/// within a session.
///
/// # v2 迁移
///
/// FrozenSessionData 现在委托给 `crate::session::FrozenContext`
/// 作为不可变数据存储，同时保留 v1 兼容的 accessor 方法。
/// 构造时同时产出 `crate::session::FrozenContext` 供 Session::new() 使用。
#[derive(Clone)]
pub struct FrozenSessionData {
    /// v2 冻结上下文（委托给本 crate session 层）
    v2_frozen: crate::session::FrozenContext,
    /// Frozen content of CLAUDE.local.md, None if no file.
    /// v2 FrozenContext 未包含 local_md，保留此处。
    claude_local_md: Option<Arc<str>>,
    /// 子 agent / fork / workflow agent 复用的冻结 system prompt
    /// （不含 16_workflow section，见 [`FrozenSessionData::subagent_system_prompt`]）。
    ///
    /// 仅当 `workflow_enabled`（主链可用）时额外渲染并存 Some；workflow 关闭时
    /// 两版字节相同，存 None 回退到 `system_prompt()`，避免重复占用。
    subagent_system_prompt: Option<Arc<str>>,
}

impl FrozenSessionData {
    /// L5：从 ACP 宿主渲染产物构造（渲染面 `FrozenSessionData::build` 的
    /// prompt 模板 / CLAUDE.md 解析 / skills 摘要扫描留在 ACP——§0 渲染是
    /// ACP 协议面职责；本构造器是类型迁入后的装配入口，供
    /// `SessionManager::build_frozen_data` 与 print mode 调用）。
    pub fn from_frozen_parts(
        v2_frozen: crate::session::FrozenContext,
        claude_local_md: Option<Arc<str>>,
        subagent_system_prompt: Option<Arc<str>>,
    ) -> Self {
        Self {
            v2_frozen,
            claude_local_md,
            subagent_system_prompt,
        }
    }

    /// v2 冻结上下文引用（供 Session::new() 使用）
    pub fn v2_frozen(&self) -> &crate::session::FrozenContext {
        &self.v2_frozen
    }

    /// 会话内冻结的完整 system prompt 字符串。
    pub fn system_prompt(&self) -> &str {
        &self.v2_frozen.system_prompt
    }

    /// 子 agent / fork / workflow agent 复用的冻结 system prompt（无 16_workflow）。
    ///
    /// 与 `system_prompt()` 同源、同冻结时机（session 创建），仅能力声明不同：
    /// 这些链不注册 WorkflowTool，不得宣称 workflow 可用（P2-2026-08-02）。
    /// workflow_enabled=false（print mode）时与 `system_prompt()` 字节相同。
    pub fn subagent_system_prompt(&self) -> &str {
        self.subagent_system_prompt
            .as_deref()
            .unwrap_or(&self.v2_frozen.system_prompt)
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

/// Langfuse 遥测注入面（L5：ACP 宿主从 `LangfuseSession` 构造；None = 禁用）。
///
/// 依赖反转（§0）：执行体不再引用 Controller 层 `LangfuseTracer`，
/// 改为消费三个注入闭包——turn 开始/结束的 trace 钩子与观测旁路 bridge
/// 工厂。bridge 工厂签名 `(provider_display, main_agent_id) -> bridge`，
/// ACP 宿主内部构造 `LangfuseBridge::new(Arc<Mutex<LangfuseTracer>>, …)`
/// （Controller 侧装配，观测旁路）。
/// Langfuse 观测旁路 bridge 工厂（SubAgent 转发器 / EventBus forwarder 用）。
pub type LangfuseBridgeFactory =
    Arc<dyn Fn(String, Option<String>) -> Option<Arc<dyn LangfuseBridgeLike>> + Send + Sync>;
/// auto-classifier LLM 构造闭包（stage 装配注入面）。
pub type AutoClassifierFactory =
    Arc<dyn Fn() -> Arc<tokio::sync::Mutex<Box<dyn peri_model::Model>>> + Send + Sync>;
/// 子 agent LLM 工厂（支持 SubAgent LLM 缓存复用；stage 装配注入面）。
pub type SubagentLlmFactory =
    Arc<dyn Fn(Option<&str>) -> Box<dyn ReactLLM + Send + Sync> + Send + Sync>;
/// 防御性 frozen 构建器（ACP 宿主渲染面构造；turn.frozen=None 时回落）。
pub type FrozenFallbackBuilder = Arc<dyn Fn(&str, Option<&str>) -> FrozenSessionData + Send + Sync>;
/// turn 结束 Langfuse 钩子（返回 flush JoinHandle，drop = fire-and-forget）。
pub type LangfuseTurnEndHook =
    Arc<dyn Fn(Option<String>) -> Option<tokio::task::JoinHandle<()>> + Send + Sync>;

pub struct LangfuseHooks {
    /// turn 开始钩子（参数 = 本轮输入文本；泵任务开头调用，语义同
    /// `LangfuseTracer::on_turn_start`）。
    pub on_turn_start: Arc<dyn Fn(&str) + Send + Sync>,
    /// turn 结束钩子（参数 = 错误文本；pump_done 之后调用，返回 flush
    /// JoinHandle，由调用方 drop——fire-and-forget，不得阻塞管线）。
    pub on_turn_end: LangfuseTurnEndHook,
    /// 观测旁路 bridge 工厂（SubAgent 转发器 / EventBus forwarder 用）。
    pub bridge_factory: LangfuseBridgeFactory,
}

/// Session-scoped context shared across all executor pipeline functions.
///
/// Replaces [`PromptExecutionContext`].
/// Fields grouped by subsystem for clarity.
///
/// L5：`Clone` 派生供 stage 装配注入闭包捕获；ACP 特有构造
/// （provider / peri_config / AgentPool / SessionManager / Controller）
/// 端口化为投影值 + 注入闭包 + [`SessionAccessPort`] / 事件端口，
/// ACP 宿主装配面（`host/prompt.rs` / `host/stdio/session/prompt_exec.rs`）
/// 在构造本结构时完成投影。
#[derive(Clone)]
#[allow(dead_code)]
pub struct SessionContext {
    // ── config: provider & global configuration（ACP 侧投影）───────────────
    pub cwd: String,
    /// provider 显示名（Langfuse bridge / 观测旁路用；原 `provider.display_name()`）。
    pub provider_name: String,
    /// provider 模型名（compact hooks 用；原 `provider.model_name()`）。
    pub provider_model_name: String,
    /// provider fingerprint（CachedLlmInstances 缓存键；原
    /// `session::agent_pool::fingerprint(&provider)`）。
    pub provider_fp: String,
    /// 生效上下文窗口（原 `provider.context_window()` / `context_1m()` 计算）。
    pub effective_context_window: u32,
    /// CLAUDE.md excludes（原 `peri_config.config.claude_md_excludes`）。
    pub claude_md_excludes: Option<Vec<String>>,
    /// 会话语言偏好（原 `peri_config.config.language`）。
    pub language: Option<String>,
    /// Compact 配置（`load_compact_config` 语义：unwrap_or_default + env
    /// overrides 每轮在宿主构造点应用——[TRAP] env 每轮重读，非 frozen）。
    pub compact_config: peri_acp_types::compact::CompactConfig,
    /// LLM 构造闭包（/bg fork 惰性构造；ACP 宿主从 `LlmProvider::from_config`）。
    pub bg_llm_factory:
        Arc<dyn Fn() -> Result<Box<dyn ReactLLM + Send + Sync>, String> + Send + Sync>,
    /// 主 LLM 缓存读取（AgentPool has_valid_cache + get_cached_llm 语义）。
    pub get_cached_llm: Option<Arc<dyn Fn() -> Option<CachedLlmInstances> + Send + Sync>>,
    /// fresh auxiliary model 构造（缓存缺失时；retry observer 烘焙在 ACP）。
    pub fresh_auxiliary_model: Option<Arc<dyn Fn() -> Arc<dyn peri_model::Model> + Send + Sync>>,
    /// LLM 缓存回写（AgentPool store_llm 语义）。
    pub store_llm: Option<Arc<dyn Fn(CachedLlmInstances) + Send + Sync>>,
    /// 会话级 retry 事件转发器（原 `pool.lock().retry_events`）。
    pub retry_events: Option<Arc<crate::session::retry_events::RetryEventForwarder>>,
    /// 主 LLM 构造（AgentPool 缓存 + RetryObserver 烘焙；stage 装配注入面）。
    pub primary_llm_factory: Option<Arc<dyn Fn() -> Arc<dyn peri_model::Model> + Send + Sync>>,
    /// auto-classifier 构造（cached 缺失时；stage 装配注入面）。
    pub auto_classifier_factory: Option<AutoClassifierFactory>,
    /// 子 agent LLM 工厂（支持 SubAgent LLM 缓存复用；stage 装配注入面）。
    pub subagent_llm_factory: Option<SubagentLlmFactory>,

    // ── session: session identity & transport ──────────────────────────────
    pub session_id: String,
    pub cancel: AgentCancellationToken,
    pub broker: Arc<dyn UserInteractionBroker>,
    pub permission_mode: Arc<peri_acp_types::permission::SharedPermissionMode>,

    // ── infra: session-level infrastructure（原 session_manager/pool 端口化）─
    /// 会话定位端口（ACP `SessionManager` 实现；None = print mode / 无 session）。
    pub session_access: Option<Arc<dyn SessionAccessPort>>,
    pub thread_store: Option<Arc<dyn peri_acp_types::store::ThreadStore>>,
    pub thread_id: Option<String>,

    // ── middleware: middleware chain resources ─────────────────────────────
    pub plugin_skill_roots: Vec<peri_acp_types::skills::SkillRoot>,
    pub plugin_agent_dirs: Vec<std::path::PathBuf>,
    pub plugin_loaded: Vec<peri_acp_types::plugin::LoadedPlugin>,
    pub hook_groups: Vec<Vec<peri_acp_types::hooks::RegisteredHook>>,
    pub cron_scheduler: Option<Arc<dyn peri_acp_types::cron::CronSchedulerPort>>,
    pub mcp_pool: Option<Arc<dyn peri_acp_types::ports::McpPoolPort>>,
    pub channel_state: Option<Arc<ChannelState>>,
    pub tool_search_index: Arc<dyn peri_acp_types::ports::ToolSearchPort>,
    /// Skills 扫描端口（prompt 渲染 available_agents / frozen 构造经此访问）。
    pub skills: Arc<dyn peri_acp_types::ports::SkillsPort>,
    pub shared_tools: Arc<
        parking_lot::RwLock<std::collections::BTreeMap<String, Arc<dyn crate::tools::BaseTool>>>,
    >,
    pub lsp_servers: Vec<peri_acp_types::lsp::LspServerConfig>,
    /// 会话级 LSP 服务器池端口（复用，None = 构造临时实例）。
    pub lsp_pool: Option<Arc<dyn peri_acp_types::ports::LspPoolPort>>,

    // ── workflow: workflow agents ──────────────────────────────────────────
    pub workflow_executor: Option<Arc<dyn peri_acp_types::workflow::AgentExecutor>>,
    pub workflow_middleware: Option<Arc<dyn peri_acp_types::ports::WorkflowMiddlewarePort>>,

    // ── 事件端口（原 controller；ACP 宿主适配 Controller）──────────────────
    /// 事件发射端口（`Controller::publish_event` 适配；补打 session_id/session_seq）。
    pub event_publisher: Arc<dyn peri_acp_types::event::EventPublisher>,
    /// 事件订阅工厂（`Controller::subscribe` 适配；每轮 pump spawn 时调用）。
    pub subscribe: Arc<dyn Fn() -> Box<dyn peri_acp_types::event::EventSubscriber> + Send + Sync>,

    // ── 命令拦截注入面（ACP 协议面）────────────────────────────────────────
    /// 命令注册表查找（ACP 协议面注册表 `default_prompt_command_registry`）。
    pub command_lookup: CommandLookupFn,
    /// compact 配置装载（`load_compact_config` 语义，含 env overrides，留 ACP）。
    pub compact_config_loader:
        Arc<dyn Fn() -> peri_acp_types::compact::CompactConfig + Send + Sync>,
    /// /bg fork 父工具集构造（middlewares 实现注入）。
    pub parent_tools_factory: ParentToolsFactory,
    /// /bg fork 链装配器（middlewares 实现注入）。
    pub chain_assembler: Arc<dyn SubagentChainAssembler>,
    /// tool resolver（middlewares 实现注入）。
    pub tool_invocation_resolver: Arc<dyn ToolInvocationResolver>,

    // ── turn: per-turn metadata ────────────────────────────────────────────
    pub session_start_source: Option<String>,

    /// 本轮 prompt RPC 的 requestId（TUI 提交时生成、随 `session/prompt`
    /// params 传入）。turn 结束（push_done → `peri/agent_event_done`）时透传
    /// 回带，供 TUI 侧 stale `TurnInterrupted` 的 request_id 配对判定
    /// （Issue 2026-08-05）。缺失路径（continuation / Immediate 命令 /
    /// stdio / print 模式）为 None——TUI 侧相应跳过 id 判定、回退代际兜底。
    pub request_id: Option<String>,

    // ── transport: transport-aware flags ───────────────────────────────────
    pub allow_await_wake: bool,

    /// 内部 continuation 通知通道（ACP server session-scoped scheduler 注入）。
    ///
    /// `on_bg_complete` 闭包在 `router.route_bg_result` 之后发送
    /// [`ContinuationRequest`]；server 的 scheduler 原子 take 被取消 prompt
    /// 的标记后，通过同一 session execution path 执行一次 AsyncContinuation，
    /// 让父 agent 消费已 route 到 SessionInbox 的 deferred callback。
    /// None = 无 continuation 消费方（stdio / print mode）。
    pub continuation_notify: Option<tokio::sync::mpsc::UnboundedSender<ContinuationRequest>>,

    /// 防御性 frozen 构建器（turn.frozen=None 时的回落；ACP 宿主渲染面
    /// 构造——生产不可达，print mode 已走 session/new 构建，None 时回落
    /// 最小 FrozenSessionData）。
    pub frozen_fallback_builder: Option<FrozenFallbackBuilder>,
}

/// Per-turn computed configuration derived from [`SessionContext`].
///
/// Built once at the top of [`run_session_loop`], passed by reference to
/// [`build_and_execute_agent`] to avoid recomputing and to keep the agent
/// builder function signature manageable.
#[allow(dead_code)]
struct TurnConfig<'a> {
    cwd: &'a str,
    frozen: Option<&'a FrozenSessionData>,
    language: Option<String>,
    cancel: &'a AgentCancellationToken,
    permission_mode: &'a Arc<peri_acp_types::permission::SharedPermissionMode>,
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
    pub event_sink: Arc<dyn peri_acp_types::event::EventSink>,
    /// 用户本轮输入。
    pub content: MessageContent,
    /// 内部异步续跑（bg 完成唤醒被取消的 turn）。
    ///
    /// 与 keepgoing（空白 user prompt = TUI 按钮"继续跑 loop"）语义隔离：
    /// - 不把空 user prompt 当 keepgoing（不触发空历史 keepgoing short-circuit）
    /// - 不写入空 human prompt（Phase 6 跳过 Prompt push，仅消费已 route 的
    ///   Defer/Info 消息）
    ///
    /// 唯一生产者为 ACP server 的 continuation scheduler（内部触发），
    /// 绝不来自 TUI kit bridge / SubmitRequest。
    pub continuation: bool,
    /// 会话级 frozen 数据（system prompt 稳定性锚点）。
    pub frozen: Option<FrozenSessionData>,
    /// 现有历史消息（执行前）。
    pub history: Vec<BaseMessage>,
    /// 上一轮 recall 注入项。
    pub incoming_recalls: Vec<String>,
    /// 后台任务结果（注入合成的 AgentResult tool_use/tool_result）。
    pub bg_results: Vec<peri_acp_types::event::BackgroundTaskResult>,
    /// Langfuse 遥测注入面（None 表示禁用遥测）。
    pub langfuse: Option<LangfuseHooks>,
    /// stage 装配桥（ACP 宿主构造：捕获 SessionContext 投影 + Langfuse
    /// bridge factory，调用 ACP `stage_builder::build_stage_context`）。
    pub stage_build: StageBuildFn,
    /// EventBus forwarder 启动器（ACP 宿主持有 Langfuse bridge 构造；
    /// 参数 = event_handles / 主 agent_id / 事件消费闭包）。
    pub forwarder_launcher: ForwarderLauncherFn,
}

/// 各 PermissionMode 的模型可见语义说明（与 10_hitl.md 的机制描述一致）。
fn permission_mode_semantics(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Default => {
            "Sensitive tool calls (Bash, Write, Edit, WebFetch, WebSearch, mcp__*, cron_register, ...) require explicit user approval."
        }
        PermissionMode::AcceptEdit => {
            "Write, Edit and folder_operations are auto-approved; other sensitive tools still require explicit approval."
        }
        PermissionMode::AutoMode => {
            "An LLM classifier decides each sensitive tool call; approval falls back to the user when the classifier is unsure."
        }
        PermissionMode::Bypass => "All tool calls are allowed without approval.",
    }
}

/// 权限模式名（含 Default，与 `display_name` 的空字符串区分）。
fn permission_mode_name(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Default => "Default",
        PermissionMode::AcceptEdit => "Accept Edit",
        PermissionMode::AutoMode => "Auto Mode",
        PermissionMode::Bypass => "Bypass",
    }
}

/// 未通知过的哨兵值：session 创建后首个模型可见 turn 需向模型公开初始
/// PermissionMode（10_hitl 不含 mode snapshot、Bypass 时 10_hitl 不渲染）。
/// 真实 mode 值为 0..=4（见 `PermissionMode` repr），不会碰撞。
pub const PERMISSION_MODE_NEVER_NOTIFIED: u8 = u8::MAX;

/// 检测 PermissionMode 是否变化；变化时返回受控通知文本（**纯检测，不记账**）。
///
/// D2：mode 会话内切换后，于下一可消费 turn 以受控 runtime event 通知模型，
/// 不重建 frozen system prompt，也不改变正在执行 batch 的 mode snapshot。
/// `last_notified` 记录"上次已随消息入队通知的 mode"：初始化为
/// [`PERMISSION_MODE_NEVER_NOTIFIED`] 哨兵，首轮返回"当前模式"初始说明；
/// 之后返回"模式切换"说明。返回值不含保留 tag（由调用方包裹
/// `<system-reminder>`），语义文本与 10_hitl.md 的机制说明一致。
///
/// 记账由 [`mark_permission_mode_notified`] 在通知文本**随消息推入模型可见
/// v2 MessageQueue 后**执行（executor_helpers Phase 6 入队点）：本 turn 在
/// 入队前失败/取消时不记账，下一 turn 重新检测仍会生成通知（可重试，不丢失）。
pub(crate) fn permission_mode_notice_if_changed(
    current: PermissionMode,
    last_notified: &AtomicU8,
) -> Option<String> {
    let cur = current as u8;
    let last = last_notified.load(Ordering::Relaxed);
    if last == cur {
        return None;
    }
    if last == PERMISSION_MODE_NEVER_NOTIFIED {
        Some(format!(
            "Current permission mode: {}: {}",
            permission_mode_name(current),
            permission_mode_semantics(current)
        ))
    } else {
        Some(format!(
            "Permission mode changed to {}: {}",
            permission_mode_name(current),
            permission_mode_semantics(current)
        ))
    }
}

/// /bg 命令事件的 envelope 身份标记（agent_id 槽位）。
///
/// bg 命令事件泵（Immediate 命令路径）发射时打此标记，消费端按标记过滤，
/// 避免与主 turn 事件流交叉（本泵每轮 spawn 并订阅全局广播，不过滤会重复
/// 消费主 pump 的事件，破坏 turn 终态唯一）。envelope 仅 ACP 内部使用，
/// TUI 协议化映射不消费身份字段。
const BG_CMD_EVENT_AGENT: &str = "__bg_cmd__";

/// BgRegistryEvent → unstable 事件（bg-task-started/completed/cancelled）映射。
///
/// TUI bg 面板协议面（`AcpEventData::BgTask*` 解码）依赖的事件名与 payload
/// 字段保持不变——事件三层化仅改发射/消费路径（发射经 Controller 补打身份、
/// 消费经 Controller 订阅），不改协议面。
fn registry_unstable_event(event: &BgRegistryEvent) -> (String, serde_json::Value) {
    match event {
        BgRegistryEvent::Started {
            task_id,
            kind,
            summary,
            started_at,
        } => (
            "bg-task-started".to_string(),
            serde_json::json!({
                "task_id": task_id,
                "kind": kind,
                "summary": summary,
                "started_at": started_at,
            }),
        ),
        BgRegistryEvent::Completed {
            task_id,
            kind,
            success,
            output_preview,
            duration_ms,
            // route_bg_result 现在在 spawner 中同步执行（在 task_manager.complete()
            // 之前），不再需要 registry 事件泵异步注入。
            result: _result,
        } => (
            "bg-task-completed".to_string(),
            serde_json::json!({
                "task_id": task_id,
                "kind": kind,
                "success": success,
                "output_preview": output_preview,
                "duration_ms": duration_ms,
            }),
        ),
        BgRegistryEvent::Cancelled { task_id, reason } => (
            "bg-task-cancelled".to_string(),
            serde_json::json!({
                "task_id": task_id,
                "reason": reason,
            }),
        ),
    }
}

/// Shared agent execution pipeline with auto-compact support.
///
/// # 调用方职责（L5 依赖反转）
///
/// - Session management (storing/retrieving cwd, history, cancel_token)
/// - Choosing the broker (HITL/AskUser handler)
/// - Providing the correct `EventSink` implementation
/// - 投影 ACP 特有构造（provider / peri_config / AgentPool / SessionManager /
///   Controller）为 [`SessionContext`] 的端口字段与注入闭包
/// - 经 [`TurnInput::stage_build`] / [`TurnInput::forwarder_launcher`] 注入
///   stage 装配桥与 EventBus forwarder 启动器（ACP 宿主侧）
pub async fn run_session_loop(ctx: SessionContext, turn: TurnInput) -> PromptResult {
    let TurnInput {
        event_sink,
        content,
        continuation,
        frozen,
        history,
        incoming_recalls,
        bg_results,
        langfuse,
        stage_build,
        forwarder_launcher,
    } = turn;

    // keepgoing：空白 user prompt 是 TUI keepgoing 按钮发起的"继续跑 loop"指令。
    // 语义：不插入 user prompt（stages/append_messages_to_transcript 跳过空 Prompt），
    // 仅让 Receive 消费计数 >0 从而驱动 ReAct loop 继续。此时不注入 recall——
    // 否则 recall 会拼进 user 消息使其非空，破坏"不插入"语义。
    // 判定与 stages 层共用同一语义：按 content block 判空（见 is_keepgoing 注释）。
    //
    // [AsyncContinuation] 内部续跑（continuation=true）不是 keepgoing：
    // 空 user prompt 不落入 keepgoing 语义，也不走空历史 keepgoing short-circuit。
    // 与 keepgoing 相同的是：**不注入 recall**——上一轮留给用户 prompt 的 recall
    // 由 run_prompt 保留在 SessionState（clone 而非 take），续跑只消费已 route 的
    // Defer/Info 消息；recall 留给后续用户 prompt 注入。
    let is_keepgoing = !continuation && is_keepgoing(&content);
    let incoming_recalls = if is_keepgoing || continuation {
        tracing::debug!(
            skip = if continuation {
                "continuation"
            } else {
                "keepgoing"
            },
            "empty user prompt, skipping recall injection"
        );
        Vec::new()
    } else {
        incoming_recalls
    };

    // 空历史 + 空 prompt：无内容可继续——直接短路返回，避免跑一轮无意义 LLM 调用。
    // （TUI 侧 handle_keepgoing_submit 已有 has_session 防御；此处防御 stdio 等
    // 其他 transport 对全新 session 发空 prompt 的场景。）
    if is_keepgoing && history.is_empty() {
        tracing::debug!("keepgoing: empty history, short-circuiting (nothing to continue)");
        // [TRAP] 短路路径绕过 agent event pump（spawn_event_pump 的 push_done
        // 不会执行），必须手动发送终止通知（ARC-EVENT-001），否则 TUI 依赖
        // AgentDone→TurnDone 退出 loading 的机制失效，界面永久卡在 loading。
        // stop_reason 与正常路径保持一致（executor_helpers push_done "end_turn"）。
        event_sink
            .push_done(&ctx.session_id, "end_turn", ctx.request_id.as_deref())
            .await;
        return PromptResult {
            messages: history,
            ok: true,
            stop_reason: PromptStopReason::EndTurn,
            history_replaced_by_compaction: false,
            recall_items: Vec::new(),
        };
    }

    // Compact config — computed early for command interception and agent building.
    // （L5：env overrides 在宿主构造点应用，语义与 load_compact_config 一致）
    let disable_compact = std::env::var("DISABLE_COMPACT").is_ok()
        || std::env::var("DISABLE_AUTO_COMPACT").is_ok()
        || !ctx.compact_config.auto_compact_enabled;

    // 解析会话级共享的 v2 MessageQueue（经 SessionAccessPort）。
    // 缺失时（无 session_access / session 不存在）退化为独立 MessageQueue，
    // 保持行为可运行——但跨 turn 消息将不可见（仅降级场景）。
    //
    // 在 run_session_loop 开头解析而非 build_and_execute_agent 内部，
    // 是为了让 bg_results / workflow Path B 等会话级注入能在此处统一 push。
    let v2_message_queue = ctx
        .session_access
        .as_ref()
        .and_then(|sa| sa.v2_message_queue(&ctx.session_id))
        .unwrap_or_default();

    // 解析 session-level SessionInbox（await-wake wrapper）。
    // 用于：(1) executor idle 期间 await_wake 阻塞等待异步事件，
    // (2) AsyncRouter 推送 bg_results/workflow 事件时触发 wake。
    // None 表示不支持 async wake（如 print mode），保持向后兼容。
    let session_inbox = ctx
        .session_access
        .as_ref()
        .and_then(|sa| sa.session_inbox(&ctx.session_id));

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
                router.route_bg_result(result, BgTaskKind::Agent);
            }
        } else {
            // 回退路径：直接 push（无 wake，兼容 print mode / 无 SessionAccess）
            use peri_acp_types::session::{MessageKind as V2Kind, MessageSource as V2Src};
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
    // （L5：缓存读取 / fresh 构造经注入闭包，AgentPool 留在 ACP）
    let cached_llm = ctx.get_cached_llm.as_ref().and_then(|f| f());
    let auxiliary_model: Option<Arc<dyn peri_model::Model>> = if disable_compact {
        None
    } else {
        cached_llm
            .as_ref()
            .map(|c| c.auxiliary_model.clone())
            .or_else(|| {
                // 转发器从 session 级 AgentPool 取：fresh 模型烘焙的 observer 会随
                // CachedLlmInstances 跨 turn 复用，必须指向 session 级转发器。
                ctx.fresh_auxiliary_model.as_ref().map(|f| f())
            })
    };

    // Context window（宿主构造点已按 context_1m 计算 effective 值）
    let effective_context_window = ctx.effective_context_window;

    // 前置创建 bg 事件通道（BgCommand 等 Immediate 命令依赖）
    let (bg_event_tx_for_cmd, mut bg_event_rx_for_cmd) =
        tokio::sync::mpsc::unbounded_channel::<ExecutorEvent>();
    // session 级 TaskManager（跨 prompt 存活，由 executor 从 session 获取）
    let task_manager_for_cmd = ctx
        .session_access
        .as_ref()
        .and_then(|sa| sa.task_manager(&ctx.session_id))
        .unwrap_or_else(|| Arc::new(peri_acp_types::tasks::NoopTaskManager));

    // BgCommand 事件的 bg event pump（必须在命令拦截之前启动，Immediate 命令才能发事件）。
    // 事件三层化：发射端把 /bg 子 agent 事件经 `EventPublisher` 发射
    // （Controller 补打 session_id/session_seq），消费端从 `subscribe()` 工厂
    // 订阅并按 [`BG_CMD_EVENT_AGENT`] 身份标记过滤（只消费本泵发射的事件）。
    // 过滤必要性：本泵每轮 spawn 且订阅全局广播——若不过滤会重复消费主 turn
    // 事件（主 pump 也订阅并推送，双推破坏 turn 终态唯一断言）。
    {
        let mut subscription = (ctx.subscribe)();
        let bg_cmd_sink = Arc::clone(&event_sink);
        let bg_cmd_sid = ctx.session_id.clone();
        let bg_cmd_cw = effective_context_window;
        let publisher = Arc::clone(&ctx.event_publisher);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    msg = subscription.recv() => {
                        match msg {
                            Ok(m) if m.envelope.session_id == bg_cmd_sid
                                && m.envelope.agent_id == BG_CMD_EVENT_AGENT => {
                                if let Some(bg_event) = m.event {
                                    bg_cmd_sink
                                        .push_event(&bg_cmd_sid, &bg_event, bg_cmd_cw)
                                        .await;
                                    // bg agent 完成后必须 push_done，否则 TUI 因
                                    // SubagentStopped 设置 is_loading=true 后永久卡住
                                    // （与 Immediate 命令路径同模式，需手动发
                                    // peri/agent_event_done 触发 acp_notifier 的
                                    // AgentDone→TurnDone）。
                                    if matches!(bg_event, ExecutorEvent::BackgroundTaskCompleted(_)) {
                                        // bg 完成事件与当前 turn 无关，不携带 request_id（None）。
                                        bg_cmd_sink.push_done(&bg_cmd_sid, "end_turn", None).await;
                                    }
                                }
                            }
                            Ok(_) => {}
                            Err(peri_acp_types::event::SubscriptionError::Lagged(n)) => {
                                tracing::warn!(n, "bg command event subscription lagged, events dropped");
                            }
                            Err(peri_acp_types::event::SubscriptionError::Closed) => break,
                        }
                    }
                    ev = bg_event_rx_for_cmd.recv() => {
                        match ev {
                            Some(bg_event) => {
                                // 发射端：v1 事件无 turn/agent 身份，agent_id 打
                                // [`BG_CMD_EVENT_AGENT`] 标记供消费端过滤（envelope
                                // 仅 ACP 内部使用，TUI 协议化映射不消费身份字段）。
                                let source = peri_acp_types::runtime::UnstampedEvent::new(
                                    String::new(),
                                    BG_CMD_EVENT_AGENT.to_string(),
                                    None,
                                    peri_acp_types::identity::EventDeliveryClass::Critical,
                                );
                                publisher.publish_event(&bg_cmd_sid, &source, bg_event);
                            }
                            None => {
                                // 发射点集合结束（bg_event_tx 全 drop）：drain 广播
                                // 在途事件后退出（与主 pump 同语义）。
                                loop {
                                    match subscription.try_recv() {
                                        Ok(Some(m)) if m.envelope.session_id == bg_cmd_sid
                                            && m.envelope.agent_id == BG_CMD_EVENT_AGENT => {
                                            if let Some(bg_event) = m.event {
                                                bg_cmd_sink
                                                    .push_event(&bg_cmd_sid, &bg_event, bg_cmd_cw)
                                                    .await;
                                                if matches!(bg_event, ExecutorEvent::BackgroundTaskCompleted(_)) {
                                                    bg_cmd_sink.push_done(&bg_cmd_sid, "end_turn", None).await;
                                                }
                                            }
                                        }
                                        Ok(Some(_)) => {}
                                        Ok(None) => break,
                                        Err(peri_acp_types::event::SubscriptionError::Lagged(n)) => {
                                            tracing::warn!(n, "bg command event subscription lagged, events dropped");
                                            break;
                                        }
                                        Err(peri_acp_types::event::SubscriptionError::Closed) => break,
                                    }
                                }
                                break;
                            }
                        }
                    }
                }
            }
        });
    }

    // Registry → 事件链泵（事件三层化收尾）：发射经 EventPublisher
    // （BgRegistryEvent 包装为 ExecutorEvent::BgRegistryEvent 载体；身份降级为
    // 空串——registry 事件无 turn 归属），消费端从 subscribe() 工厂订阅
    // 本 session 事件并映射回 bg-task-* unstable 事件（TUI bg 面板协议面不变）。
    {
        let (registry_event_tx, mut registry_event_rx) =
            tokio::sync::mpsc::unbounded_channel::<BgRegistryEvent>();
        task_manager_for_cmd.set_event_sender(registry_event_tx, ctx.session_id.clone());
        let mut subscription = (ctx.subscribe)();
        let registry_sink = Arc::clone(&event_sink);
        let registry_sid = ctx.session_id.clone();
        let publisher = Arc::clone(&ctx.event_publisher);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    msg = subscription.recv() => {
                        match msg {
                            Ok(m) if m.envelope.session_id == registry_sid => {
                                if let Some(ExecutorEvent::BgRegistryEvent(event)) = m.event {
                                    let (event_name, payload) = registry_unstable_event(&event);
                                    registry_sink
                                        .push_unstable_event(&registry_sid, event_name, payload)
                                        .await;
                                }
                            }
                            Ok(_) => {}
                            Err(peri_acp_types::event::SubscriptionError::Lagged(n)) => {
                                tracing::warn!(n, "registry event subscription lagged, events dropped");
                            }
                            Err(peri_acp_types::event::SubscriptionError::Closed) => break,
                        }
                    }
                    ev = registry_event_rx.recv() => {
                        match ev {
                            Some(event) => {
                                // 发射端：registry 事件无 turn/agent 身份（身份降级为
                                // 空串；envelope 仅 ACP 内部使用）。
                                let source = peri_acp_types::runtime::UnstampedEvent::new(
                                    String::new(),
                                    String::new(),
                                    None,
                                    peri_acp_types::identity::EventDeliveryClass::Critical,
                                );
                                publisher.publish_event(
                                    &registry_sid,
                                    &source,
                                    ExecutorEvent::BgRegistryEvent(event),
                                );
                            }
                            None => {
                                // 发射点集合结束（registry_event_tx 全 drop）：drain 广播
                                // 在途事件后退出（与主 pump 同语义）。
                                loop {
                                    match subscription.try_recv() {
                                        Ok(Some(m)) if m.envelope.session_id == registry_sid => {
                                            if let Some(ExecutorEvent::BgRegistryEvent(event)) = m.event {
                                                let (event_name, payload) = registry_unstable_event(&event);
                                                registry_sink
                                                    .push_unstable_event(&registry_sid, event_name, payload)
                                                    .await;
                                            }
                                        }
                                        Ok(Some(_)) => {}
                                        Ok(None) => break,
                                        Err(peri_acp_types::event::SubscriptionError::Lagged(n)) => {
                                            tracing::warn!(n, "registry event subscription lagged, events dropped");
                                            break;
                                        }
                                        Err(peri_acp_types::event::SubscriptionError::Closed) => break,
                                    }
                                }
                                break;
                            }
                        }
                    }
                }
            }
        });
    }

    // ── L5 命令拦截注入面（注册表 / compact 配置 / bg fork spawner）──
    // 命令注册表查找：ACP 协议面注册表（default_prompt_command_registry 注册
    // compact/bg 命令，实现已在 Agent 层）；每次拦截按原语义构造新注册表。
    let command_lookup = Arc::clone(&ctx.command_lookup);
    // compact 配置装载：load_compact_config 语义（含 env overrides）留在 ACP。
    let compact_config_loader = Arc::clone(&ctx.compact_config_loader);
    // /bg fork spawner（默认实现迁入本 crate；LLM 构造 / 父工具集 /
    // 链装配器 / resolver 经注入面接入——LLM 与工具集惰性构造，仅 /bg 触发）。
    let bg_llm_factory = Arc::clone(&ctx.bg_llm_factory);
    let parent_tools_factory = Arc::clone(&ctx.parent_tools_factory);
    let chain_assembler = Arc::clone(&ctx.chain_assembler);
    let tool_invocation_resolver = Arc::clone(&ctx.tool_invocation_resolver);
    let bg_spawner_arc: Arc<dyn peri_acp_types::command::BgForkSpawner> =
        Arc::new(DefaultBgForkSpawner::new(
            Arc::clone(&task_manager_for_cmd),
            bg_llm_factory,
            parent_tools_factory,
            chain_assembler,
            tool_invocation_resolver,
        ));

    // Command interception — check if content is a slash command before building agent.
    if let Some(immediate) = intercept_immediate_command(InterceptRequest {
        content: &content,
        history: &history,
        cwd: &ctx.cwd,
        session_id: &ctx.session_id,
        cancel: &ctx.cancel,
        thread_store: ctx.thread_store.clone(),
        thread_id: ctx.thread_id.clone(),
        // L5：冻结数据由调用点投影为字符串字段（原 FrozenSessionData 引用）
        frozen_claude_md: frozen
            .as_ref()
            .and_then(|f| f.claude_md().map(String::from)),
        frozen_claude_local_md: frozen
            .as_ref()
            .and_then(|f| f.claude_local_md().map(String::from)),
        frozen_skill_summary: frozen
            .as_ref()
            .and_then(|f| f.skill_summary().map(String::from)),
        // fork/bg-fork 复用的冻结 prompt 用"无 16_workflow"版本（P2-2026-08-02）
        frozen_system_prompt: frozen
            .as_ref()
            .map(|f| f.subagent_system_prompt().to_string()),
        event_sink: &event_sink,
        auxiliary_model: &auxiliary_model,
        bg_event_tx: &bg_event_tx_for_cmd,
        task_manager: &task_manager_for_cmd,
        command_lookup,
        compact_config_loader,
        bg_spawner: Some(bg_spawner_arc),
    })
    .await
    {
        return immediate;
    }

    let trace_input = content.text_content();
    // D2：PermissionMode 会话内切换后，于下一可消费 turn 以受控 runtime event
    // 通知模型（不重建 frozen system prompt）。last-notified 值存于 session 级
    // AcpSession（跨 turn 持久）；print mode 等无 SessionAccess 场景不注入。
    // 与 incoming_recalls 共用 `<system-reminder>` 受控容器，但不属于 recall
    // 语义（keepgoing 清空 recall 时通知仍注入）。
    //
    // [P3] 此处只做纯检测生成文本，**不记账**：`ModeNoticeBooking` 随 agent_input
    // 下传，executor_helpers 在 Phase 6 把消息推入模型可见 v2 MessageQueue 时
    // 才调用 `mark_permission_mode_notified`。本 turn 在入队前失败/取消不会
    // 丢失通知——下一 turn 重新检测仍会生成（可重复重试，恰好可见一次）。
    // 初始 mode（哨兵状态）同样经此路径在首个模型可见 turn 公开一次。
    let mode_notice_booking: Option<ModeNoticeBooking> = ctx
        .session_access
        .as_ref()
        .and_then(|sa| sa.last_notified_permission_mode(&ctx.session_id))
        .and_then(|last| {
            permission_mode_notice_if_changed(ctx.permission_mode.load(), &last).map(|text| {
                ModeNoticeBooking {
                    text,
                    last_notified: last,
                    mode: ctx.permission_mode.load(),
                }
            })
        });
    let mode_notice = mode_notice_booking.as_ref().map(|b| b.text.clone());
    let agent_input = if incoming_recalls.is_empty() && mode_notice.is_none() {
        AgentInput::blocks(content)
    } else {
        let mut reminder_parts: Vec<String> = Vec::new();
        if !incoming_recalls.is_empty() {
            reminder_parts.push(incoming_recalls.join("\n"));
        }
        if let Some(notice) = mode_notice {
            reminder_parts.push(notice);
        }
        let reminder_text = format!(
            "<system-reminder>\n{}\n</system-reminder>",
            reminder_parts.join("\n\n")
        );
        let mut blocks = content.content_blocks();
        blocks.push(ContentBlock::text(reminder_text));
        AgentInput::blocks(MessageContent::blocks(blocks))
    };

    // [v2] Context budget 由 AgentComponents 传给 StageContext，此处不再需要本地变量。

    // Event channel (lives for entire run_session_loop lifetime)
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<ExecutorEvent>();
    let event_tx = Arc::new(parking_lot::Mutex::new(Some(event_tx)));

    // 将会 move 的 middleware resources（无法借用，必须 move）。
    // turn 仍以引用形式借用 cwd/cancel/permission_mode/broker。
    let turn = TurnConfig {
        cwd: &ctx.cwd,
        frozen: frozen.as_ref(),
        language: frozen
            .as_ref()
            .and_then(|f| f.language().map(|s| s.to_string()))
            .or_else(|| ctx.language.clone()),
        cancel: &ctx.cancel,
        permission_mode: &ctx.permission_mode,
        broker: &ctx.broker,
        session_start_source: ctx.session_start_source.clone(),
        auxiliary_model: auxiliary_model.clone(),
        effective_context_window,
    };

    // Langfuse 遥测经注入闭包（宿主构造的 LangfuseHooks）接入。
    let langfuse_on_turn_start: Option<Arc<dyn Fn() + Send + Sync>> = langfuse.as_ref().map(|h| {
        let on_start = Arc::clone(&h.on_turn_start);
        let trace_input = trace_input.to_string();
        Arc::new(move || {
            on_start(&trace_input);
        }) as Arc<dyn Fn() + Send + Sync>
    });
    let langfuse_on_turn_end: Option<LangfuseTurnEndHook> =
        langfuse.as_ref().map(|h| Arc::clone(&h.on_turn_end));

    // Main event pump（事件三层化：发射点 → EventPublisher →
    // 本泵订阅消费；event_rx 仅作发射点集合的关闭信号）
    let (stop_reason_tx, stop_reason_rx) = exec_oneshot::channel::<PromptStopReason>();
    let pump_handle = spawn_event_pump(SpawnPumpRequest {
        // L5：订阅经端口适配（契约层 SubscriptionError 镜像，Controller 零改动）
        subscription: (ctx.subscribe)(),
        event_rx,
        stop_reason_rx,
        sink: Arc::clone(&event_sink),
        session_id: ctx.session_id.clone(),
        effective_context_window,
        // L5：Langfuse tracer 留在 ACP——泵经闭包在任务开头触发
        // on_turn_start、pump_done 之后触发 on_turn_end（JoinHandle drop =
        // fire-and-forget，不得阻塞管线）。
        langfuse_on_turn_start,
        langfuse_on_turn_end,
        request_id: ctx.request_id.clone(),
    });

    // 把会 move/借用 的资源直接传入 build_and_execute_agent。
    // 由于 prompt builder 需要的所有资源都已提供，调用方后续不再访问这些已 move 字段
    // （session_id 在 collect_result 借用，此时 build_and_execute_agent 已完成）。
    let exec_outcome = build_and_execute_agent(
        &ctx,
        &turn,
        agent_input,
        history,
        &ctx.session_id,
        cached_llm.as_ref(),
        &v2_message_queue,
        async_router.clone(),
        task_manager_for_cmd,
        mode_notice_booking,
        continuation,
        stage_build,
        forwarder_launcher,
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

    // turn 收尾：转发器是 session 级（挂 AgentPool），turn 间不清理、不重建，
    // 靠下一 turn `build_agent` 覆盖式 `set` 当前 handler。残留 handler（v1
    // 直发）经 `EventPublisher` 发射，事件泵随本轮 `event_tx` 关闭
    // 退出后到达的事件自然丢弃（与迁移前 close 后检查 None 丢弃语义一致）。

    result
}

/// Agent 执行后的最终输出（state + 停止原因）。
///
/// L5：定义迁入本模块（`session::exec::executor_helpers::ExecOutcome`），
/// 本处经上方 use 块 re-export。
/// 构建 + 执行 agent。包含：
/// - system prompt 解析（frozen 或 legacy 重建）
/// - SubAgentMiddleware register/deregister 闭包
/// - `build_agent` 调用 + AgentPool 缓存回写
/// - bg event pump + todo 转发 pump 启动
/// - `build_and_execute_agent_v2` 调用 + 错误事件转发
/// - cancel cascade 子 agent
#[allow(clippy::too_many_arguments)]
async fn build_and_execute_agent(
    ctx: &SessionContext,
    turn: &TurnConfig<'_>,
    agent_input: AgentInput,
    history: Vec<BaseMessage>,
    session_id: &str,
    cached_llm: Option<&CachedLlmInstances>,
    v2_message_queue: &MessageQueue,
    async_router: Option<AsyncRouter>,
    task_manager: Arc<dyn TaskManager>,
    mode_notice_booking: Option<ModeNoticeBooking>,
    continuation: bool,
    stage_build: StageBuildFn,
    forwarder_launcher: ForwarderLauncherFn,
) -> ExecOutcome {
    let (
        system_prompt,
        subagent_system_prompt,
        frozen_claude_md,
        frozen_claude_local_md,
        frozen_skill_summary,
        frozen_date,
    ) = if let Some(f) = turn.frozen {
        // 使用 session 创建时冻结的数据，跳过重建
        (
            f.system_prompt().to_string(),
            Some(f.subagent_system_prompt().to_string()),
            f.claude_md().map(|s| s.to_string()),
            f.claude_local_md().map(|s| s.to_string()),
            f.skill_summary().map(|s| s.to_string()),
            Some(f.date().to_string()),
        )
    } else {
        // 调用方未提供 frozen 数据时，经注入的防御性构建器在此一次性构建
        // （渲染面在 ACP 宿主；生产不可达——print mode 已迁移为提前构建
        // FrozenSessionData，此分支仅作防御性编程保留，None 时回落最小数据）。
        match ctx.frozen_fallback_builder.as_ref() {
            Some(builder) => {
                let frozen_data = builder(turn.cwd, turn.language.as_deref());
                (
                    frozen_data.system_prompt().to_string(),
                    Some(frozen_data.subagent_system_prompt().to_string()),
                    frozen_data.claude_md().map(|s| s.to_string()),
                    frozen_data.claude_local_md().map(|s| s.to_string()),
                    frozen_data.skill_summary().map(|s| s.to_string()),
                    Some(frozen_data.date().to_string()),
                )
            }
            None => {
                // 最小回落：无 skills / 无 CLAUDE.md 的空冻结数据
                let empty = FrozenSessionData::from_frozen_parts(
                    crate::session::FrozenContext {
                        system_prompt: Arc::from(""),
                        claude_md: Arc::from(""),
                        skill_summary: Arc::from(""),
                        date: Arc::from(Local::now().format("%Y-%m-%d").to_string()),
                        language: turn.language.clone().map(Arc::from),
                    },
                    None,
                    None,
                );
                (
                    empty.system_prompt().to_string(),
                    Some(empty.subagent_system_prompt().to_string()),
                    empty.claude_md().map(|s| s.to_string()),
                    empty.claude_local_md().map(|s| s.to_string()),
                    empty.skill_summary().map(|s| s.to_string()),
                    Some(empty.date().to_string()),
                )
            }
        }
    };

    // Build register/deregister closures for SubAgentMiddleware（经 SessionAccessPort
    // 端口构造——原逻辑：定位 AcpSession 并维护 active_agents 注册表）
    let register_runtime = ctx
        .session_access
        .as_ref()
        .and_then(|sa| sa.register_runtime(session_id));
    let deregister_runtime = ctx
        .session_access
        .as_ref()
        .and_then(|sa| sa.deregister_runtime(session_id));

    let event_handler: Arc<dyn AgentEventHandler> =
        Arc::new(peri_acp_types::event::FnEventHandler({
            // v1 协议化载体直发（subagent 发射侧同步映射 / retry observer 等）
            // 统一经 EventPublisher：无 turn_id/agent_id 身份的事件身份降级为空串
            // （envelope 仅 ACP 内部使用，TUI 协议化映射不消费空身份字段）。
            // v1 ExecutorEvent 中间态已退役（批 2「v1-retire」）：本 handler 是
            // ACP 协议序列化面的接收端，不承载 Agent 层业务发射。
            let publisher = Arc::clone(&ctx.event_publisher);
            let sid = session_id.to_string();
            move |event: ExecutorEvent| {
                let source = peri_acp_types::runtime::UnstampedEvent::new(
                    String::new(),
                    String::new(),
                    None,
                    peri_acp_types::identity::EventDeliveryClass::Critical,
                );
                publisher.publish_event(&sid, &source, event);
            }
        }));

    // Session 级 workflow 完成通知消费者（单次 spawn）。
    // 单路径：
    //   Path B (Agent): 通过 AsyncRouter → InboxHandle → push_defer（Defer kind）→ End 阶段唤醒新 turn。
    // TUI 侧通知由 registry 通道提供（registry.complete → BgRegistryEvent::Completed →
    // bg-task-completed unstable event），不再经 EventSink 直推 BackgroundTaskCompleted——
    // 该映射在 event_sink 是死路径（S5.1，issue 2026-08-05-background-task-completed-event-dead-path）。
    if let Some(wf_mw) = ctx.workflow_middleware.as_ref() {
        // 将 session 级 TaskManager 注入 WorkflowMiddleware（延迟注入，支持内部可变性）
        wf_mw.set_bg_registry(task_manager.clone());

        // init_notification_buffer() 是 set-once gate：首次返回 true，后续返回 false。
        // WorkflowMiddleware 是 session 级实例（session/new 创建），
        // 因此每个 session 的消费者只 spawn 一次，无跨 session 污染。
        if wf_mw.init_notification_buffer() {
            let wf_mw_for_notify = Arc::clone(wf_mw);
            // AsyncRouter（v2 路径：push_defer + wake Notify）
            // 或回退 v2 queue clone（无 inbox 时直接 push，无 wake）
            let wf_router = async_router.clone();
            let fallback_queue = v2_message_queue.clone();
            // task_manager 用于在 Defer 入队后递减 active_count，消除竞态窗口
            let notify_bg = task_manager.clone();
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
                                    task_result.status.as_str(),
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
                                // 幽灵完成事件防护（issue 2026-08-05）：killed/failed 不得显示为 "completed"
                                let status_word = match task_result.status.as_str() {
                                    "completed" => "completed",
                                    "killed" => "killed",
                                    _ => "failed",
                                };
                                // 不包裹 <system-reminder>：append_messages_to_transcript 统一包裹所有 Defer/Info
                                let notif_text = format!(
                                    "Workflow '{}' {status_word}. ({}ms, {} agents, {} tool calls)\n\
                                    {}Results saved to .claude/workflow-runs/{}/state.json",
                                    task_result.workflow_name,
                                    task_result.duration_ms,
                                    task_result.agent_count,
                                    task_result.tool_calls_count,
                                    phase_lines,
                                    task_result.run_id,
                                );
                                fallback_queue.push(QueuedMessage::new(
                                    peri_acp_types::session::MessageKind::Defer,
                                    peri_acp_types::session::MessageSource::WorkflowComplete,
                                    BaseMessage::human(MessageContent::text(notif_text)),
                                ));
                            }

                            // 构造 BackgroundTaskResult 并写入 registry：触发 BgRegistryEvent::Completed
                            // （→ bg-task-completed unstable event → TUI 通知条），这是通知的真实路径。
                            // 不再经 EventSink 直推 BackgroundTaskCompleted（S5.1：event_sink 无映射，死路径）。
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
                                timed_out: false,
                            };
                            // 在 Defer 入队后递减 active_count，消除 tool.rs 通知 task 中的竞态窗口：
                            // 原实现在 registry.complete() broadcast 后立即调用 bg.complete_workflow()，
                            // 若 broadcast consumer 尚未被调度，agent 的 idle_should_wait probe
                            // (active_count > 0) 提前归零 → agent 退出 ReAct loop → Defer 堆积在队列中。
                            // [修复] 将 complete_workflow 移至 consumer 内 Defer push 之后执行。
                            notify_bg.complete(&task_result.run_id, bg);
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

    // 从 session_access 获取 goal_state（实现 GoalController trait）
    let goal_controller: Option<Arc<dyn peri_acp_types::goal::GoalController>> = ctx
        .session_access
        .as_ref()
        .and_then(|sa| sa.goal_controller(session_id));

    let thread_persistence = ThreadPersistence {
        store: ctx.thread_store.clone(),
        parent_thread_id: ctx.thread_id.clone(),
        register_runtime,
        deregister_runtime,
    };

    let task_manager_opt = ctx
        .session_access
        .as_ref()
        .and_then(|sa| sa.task_manager(session_id));

    // on_bg_complete：bg 完成时**先**把结果同步 route 到 SessionInbox
    // （Defer + wake），**再**通知 ACP server 的 per-session continuation
    // scheduler。回调可能在主 prompt 结束后才发生（bg 独立运行），此时
    // callback queue 已先写入；scheduler 原子 take session/cancel 标记后
    // 通过同一 session execution path 发起内部 AsyncContinuation。
    let on_bg_complete = async_router.as_ref().map(|router| {
        let router = router.clone();
        let notify = ctx.continuation_notify.clone();
        let sid = ctx.session_id.clone();
        Arc::new(move |result: &BackgroundTaskResult, kind: BgTaskKind| {
            router.route_bg_result(result, kind);
            if let Some(ref tx) = notify {
                let _ = tx.send(ContinuationRequest {
                    session_id: sid.clone(),
                    kind,
                });
            }
        }) as Arc<dyn Fn(&BackgroundTaskResult, BgTaskKind) + Send + Sync>
    });

    // ── L5 执行体注入面（stage 构建 / 事件发射 / LLM 缓存 / cancel cascade / forwarder）──
    // 事件发射端口（Controller 适配；Phase 2/3/4/7/9 统一发射点）
    let publisher = Arc::clone(&ctx.event_publisher);

    // LLM 缓存回写（AgentPool；ACP 宿主注入）
    let store_llm: Arc<dyn Fn(CachedLlmInstances) + Send + Sync> = match &ctx.store_llm {
        Some(f) => Arc::clone(f),
        None => Arc::new(|_| {}),
    };

    // cancel cascade 子 agent（SessionAccessPort）
    let sa_for_cascade = ctx.session_access.clone();
    let cancel_cascade: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |sid: &str| {
        if let Some(ref sa) = sa_for_cascade {
            sa.cancel_cascade_children(sid);
        }
    });

    // EventBus forwarder 启动器（ACP 宿主注入；Langfuse bridge 构造在 ACP——
    // 观测旁路；biased select 顺序不变量单点保持在 ACP spawn_eventbus_forwarder）

    // v2 单一路径。
    build_and_execute_agent_v2(V2ExecuteRequest {
        session_id: ctx.session_id.clone(),
        cwd: ctx.cwd.clone(),
        cancel: ctx.cancel.clone(),
        thread_store: ctx.thread_store.clone(),
        thread_id: ctx.thread_id.clone(),
        agent_input,
        history,
        cached_llm: cached_llm.cloned(),
        task_manager: task_manager_opt,
        mode_notice_booking,
        continuation,
        // ── stage 装配输入（透传 StageBuildRequest）──
        system_prompt,
        subagent_system_prompt,
        frozen: FrozenData {
            claude_md: frozen_claude_md,
            claude_local_md: frozen_claude_local_md,
            skill_summary: frozen_skill_summary,
            date: frozen_date,
        },
        event_handler,
        agent_overrides: None,       // agent_overrides
        preload_skills: Vec::new(),  // preload_skills
        child_handler_factory: None, // child_handler_factory
        auxiliary_model: turn.auxiliary_model.clone(),
        thread_persistence,
        goal_controller,
        on_bg_complete,
        // ── 注入面 ──
        publisher,
        stage_build,
        store_llm,
        cancel_cascade,
        forwarder_launcher,
    })
    .await
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
/// > Agent 构建和执行统一通过会话编排入口 `run_session_loop()`（ACP 侧经
/// > 协议化薄壳 re-export）。禁止在 TUI 层直接构建 Agent。
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
///
/// L5：LLM 构造（`AgentModelBridge`）由调用方（ACP 宿主）完成——执行体
/// 不引用 ACP provider 类型；指令模板同 crate（`session::subagent`）。
pub async fn execute_prediction(
    llm: Box<dyn ReactLLM + Send + Sync>,
    history: Vec<BaseMessage>,
    cwd: &str,
    current_title: Option<&str>,
) -> Result<Vec<PredictionAction>, PredictionError> {
    debug!(
        msg_count = history.len(),
        cwd, "Prediction facade: starting"
    );

    // execute_prediction 是 1-turn 无工具无中间件的最小 LLM 调用，
    // 不需要构造完整 v2 stages。直接构造 messages 调
    // ReactLLM::generate_reasoning 一次。
    let directive = crate::session::subagent::build_prediction_directive(current_title);
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
