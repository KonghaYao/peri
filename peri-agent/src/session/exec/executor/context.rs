use std::sync::Arc;

use peri_acp_types::{
    interaction::{ChannelState, UserInteractionBroker},
    messages::{BaseMessage, MessageContent},
    session::SessionAccessPort,
};
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use crate::agent::langfuse_bridge::LangfuseBridgeLike;
use crate::agent::react::ReactLLM;
use crate::session::exec::stage_builder::CachedLlmInstances;
use crate::tools::ToolInvocationResolver;

use super::{CommandLookupFn, ContinuationRequest, ForwarderLauncherFn, StageBuildFn};

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
}

impl FrozenSessionData {
    /// L5：从 ACP 宿主渲染产物构造（渲染面 `FrozenSessionData::build` 的
    /// prompt 模板 / CLAUDE.md 解析 / skills 摘要扫描留在 ACP——§0 渲染是
    /// ACP 协议面职责；本构造器是类型迁入后的装配入口，供
    /// `SessionManager::build_frozen_data` 与 print mode 调用）。
    pub fn from_frozen_parts(
        v2_frozen: crate::session::FrozenContext,
        claude_local_md: Option<Arc<str>>,
    ) -> Self {
        Self {
            v2_frozen,
            claude_local_md,
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

    /// 会话级 MetaHarness 冻结状态（段落覆盖 + middleware 关闭集合）。
    ///
    /// 单一事实源为 `v2_frozen.meta_harness`——本 accessor 委托存储，不维护
    /// 第二份副本（设计 §2.3；防止双事实源漂移）。
    pub fn meta_harness(&self) -> &peri_acp_types::meta_harness::MetaHarnessState {
        &self.v2_frozen.meta_harness
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
    /// `turn.frozen=None` 时构造最小 snapshot 的语言回退。
    /// production stage/render/subagent 必须从 `FrozenSessionData` 派生语言。
    pub language: Option<String>,
    /// Compact 配置（`load_compact_config` 语义：unwrap_or_default + env
    /// overrides 每轮在宿主构造点应用——[TRAP] env 每轮重读，非 frozen）。
    pub compact_config: peri_acp_types::compact::CompactConfig,
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
    pub dynamic_mcp: Option<Arc<dyn peri_acp_types::ports::DynamicMcpDeploymentPort>>,
    pub session_mcp_capability: Option<Arc<dyn peri_acp_types::ports::SessionMcpCapabilityPort>>,
    pub dynamic_mcp_projection:
        Arc<parking_lot::Mutex<Option<Arc<dyn peri_acp_types::ports::SessionMcpProjectionLease>>>>,
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
    /// 命令注册表查找（ACP 协议面会话级注册表；返回 `ResolvedCommand`）。
    pub command_lookup: CommandLookupFn,
    /// compact 配置装载（`load_compact_config` 语义，含 env overrides，留 ACP）。
    pub compact_config_loader:
        Arc<dyn Fn() -> peri_acp_types::compact::CompactConfig + Send + Sync>,
    /// deferred 工具解析器（主 Agent 与后台 Agent 工具共享）。
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
