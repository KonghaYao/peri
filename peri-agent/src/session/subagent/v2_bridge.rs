use std::sync::Arc;

use parking_lot::RwLock;
use peri_acp_types::identity::AgentId;
use tokio_util::sync::CancellationToken;

use crate::agent::events::AgentEventHandler;
use crate::agent::events_v2::{
    observe_event_to_executor, EventBus, EventBusConfig, EventHandles, ObserveEvent,
};
use crate::agent::react::ReactLLM;
use crate::agent::stages::{SharedToolMap, StageContext};
use crate::agent::{CompactConfig, ContextBudget};
use crate::error_suggest::{ErrorSuggestRegistry, ToolRegistrySnapshot};
use crate::middleware::chain::MiddlewareChain;
use crate::session::turn::TurnId;
use crate::session::{FrozenContext, MessageQueue, Session};
use crate::tools::{BaseTool, DirectToolInvocationResolver, ToolInvocationResolver};

// ─── v2 桥接（自 peri-middlewares/src/subagent/v2_bridge.rs 迁移） ──────────

/// SubAgent v2 上下文产物
pub struct V2SubagentContext {
    /// v2 StageContext（传给 run_react_loop）
    pub context: StageContext,
    /// v2 Session（调用方持有以读取 transcript）
    pub session: Arc<Session>,
    /// EventBus 消费端（调用方 spawn forwarder 用）
    pub event_handles: EventHandles,
    /// 统一后的 subagent 身份键（= child_thread_id 的 AgentId 形式）
    pub agent_id: AgentId,
    /// EventBus 生产端（补发 SubagentStart/Stop 等 ObserveEvent 用）
    pub event_bus: Arc<EventBus>,
}

/// 从 `child_thread_id`（UUID v7 字符串）解析统一身份键 `AgentId`（C1）。
///
/// 身份契约：`child_thread_id`、subagent session `AgentId`、`instance_id`、
/// forwarder `source_agent_id`、事件 `agent_id` 收敛为同一 UUID。
pub fn agent_id_from_child_thread(child_thread_id: &str) -> AgentId {
    AgentId::from_uuid(
        uuid::Uuid::parse_str(child_thread_id)
            .expect("child_thread_id 由 Uuid::now_v7() 生成，必为合法 UUID"),
    )
}

/// 构造 v2 `SubagentStart` 事件（发射语义单一事实源）。
///
/// `agent_id` 为父视角归属身份：`parent_agent_id` 未注入（/bg、测试路径）时以
/// `child_agent_id` 占位——v1 协议化映射（`observe_event_to_executor`）不消费
/// 该字段，仅 v2 emit（Langfuse tracer 归属）需要真实父身份。
pub(crate) fn build_subagent_start_v2(
    turn_id: TurnId,
    parent_agent_id: Option<AgentId>,
    child_agent_id: AgentId,
    agent_name: &str,
    is_background: bool,
) -> ObserveEvent {
    ObserveEvent::SubagentStart {
        turn_id,
        agent_id: parent_agent_id.unwrap_or(child_agent_id),
        child_agent_id,
        agent_name: agent_name.to_string(),
        is_background,
    }
}

/// 经 child EventBus 发射 v2 `SubagentStart`（C2）。
///
/// `parent_agent_id` 为 None（未注入/测试路径）时不 emit，仅 tracing warn——
/// 防脏数据：缺父身份的事件会让 tracer 无法归属，宁可走 incomplete 分支。
/// （v1 协议化直发不依赖本函数：`forward_subagent_start_v1` 独立于父身份。）
pub(crate) fn emit_subagent_start_v2(
    event_bus: &Arc<EventBus>,
    turn_id: TurnId,
    parent_agent_id: Option<AgentId>,
    child_agent_id: AgentId,
    agent_name: &str,
    is_background: bool,
) {
    if parent_agent_id.is_none() {
        tracing::warn!(
            target: "langfuse::subagent",
            child_agent_id = %child_agent_id,
            agent_name,
            "parent_agent_id 未注入，跳过 v2 SubagentStart emit（防脏数据）"
        );
        return;
    }
    event_bus.emit_observe(build_subagent_start_v2(
        turn_id,
        parent_agent_id,
        child_agent_id,
        agent_name,
        is_background,
    ));
}

/// 构造 v2 `SubagentStop` 事件（发射语义单一事实源）。占位规则同
/// [`build_subagent_start_v2`]。
pub(crate) fn build_subagent_stop_v2(
    turn_id: TurnId,
    parent_agent_id: Option<AgentId>,
    child_agent_id: AgentId,
    agent_name: &str,
    result: &str,
    is_error: bool,
) -> ObserveEvent {
    ObserveEvent::SubagentStop {
        turn_id,
        agent_id: parent_agent_id.unwrap_or(child_agent_id),
        child_agent_id,
        agent_name: agent_name.to_string(),
        result: result.to_string(),
        is_error,
    }
}

/// 经 child EventBus 发射 v2 `SubagentStop`（C3）。
///
/// 与 [`emit_subagent_start_v2`] 同一通道；parent_agent_id 为 None 时同样跳过。
pub(crate) fn emit_subagent_stop_v2(
    event_bus: &Arc<EventBus>,
    turn_id: TurnId,
    parent_agent_id: Option<AgentId>,
    child_agent_id: AgentId,
    agent_name: &str,
    result: &str,
    is_error: bool,
) {
    if parent_agent_id.is_none() {
        tracing::warn!(
            target: "langfuse::subagent",
            child_agent_id = %child_agent_id,
            agent_name,
            "parent_agent_id 未注入，跳过 v2 SubagentStop emit（防脏数据）"
        );
        return;
    }
    event_bus.emit_observe(build_subagent_stop_v2(
        turn_id,
        parent_agent_id,
        child_agent_id,
        agent_name,
        result,
        is_error,
    ));
}

// ─── v1 协议化载体直发（发射侧同步映射） ───────────────────────────────────
//
// v1 `ExecutorEvent` 中间态已退役（`2026-07-18-executor-event-retirement.md`）：
// SubagentStart/Stop 的发射语义单一事实源为 v2 事件构造，v1 仅作 ACP 协议化
// 载体——经 `peri-acp-types::event_v2::observe_event_to_executor`（协议序列化面
// 保留的最小映射）同步映射后直发父 handler / bg 泵。同步直发（非 forwarder
// 异步转发）保证 Started/Stopped 与 BackgroundTaskCompleted 的顺序契约；
// 转发器（`subagent_event_forwarder`）对 v2 SubagentStart/Stop 保持过滤（防双发）。

/// v1 协议化直发 `SubagentStarted`（从 v2 事件同步映射）。
///
/// `handler` 为 None（无父 handler / 无 bg 通道）时静默跳过。
pub(super) fn forward_subagent_start_v1(
    handler: Option<&Arc<dyn AgentEventHandler>>,
    ev: ObserveEvent,
) {
    let Some(h) = handler else { return };
    // SubagentStarted 无 source_agent_id 字段（TUI 按 instance_id 配对），
    // 无需 set_source_agent_id；instance_id 由 child_agent_id 身份透传（C1）。
    if let Some(exec_ev) = observe_event_to_executor(ev) {
        h.on_event(exec_ev);
    }
}

/// v1 协议化直发 `SubagentStopped`（从 v2 事件同步映射）。语义同
/// [`forward_subagent_start_v1`]。
pub(super) fn forward_subagent_stop_v1(
    handler: Option<&Arc<dyn AgentEventHandler>>,
    ev: ObserveEvent,
) {
    let Some(h) = handler else { return };
    if let Some(exec_ev) = observe_event_to_executor(ev) {
        h.on_event(exec_ev);
    }
}

/// 构造 SubAgent v2 上下文（自 `build_v2_subagent_context` 迁移；
/// `tool_invocation_resolver` 参数化避免 Agent 层反向依赖 middlewares）。
///
/// `session` 为调用方预创建的子 session（transcript 已绑定持久化、已注入
/// parent_messages / system_prompt）；None 时内部自建（测试/工具直调路径兜底，
/// 无持久化）。
#[allow(clippy::too_many_arguments)]
pub fn build_v2_subagent_context(
    session: Option<Arc<Session>>,
    llm: Box<dyn ReactLLM + Send + Sync>,
    chain: MiddlewareChain,
    tools: Vec<Arc<dyn BaseTool>>,
    cwd: &str,
    cancel_token: CancellationToken,
    tool_invocation_resolver: Option<Arc<dyn ToolInvocationResolver>>,
    error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
    tool_registry_snapshot: Option<ToolRegistrySnapshot>,
    compact_config: Option<CompactConfig>,
    context_budget: Option<ContextBudget>,
    compact_llm: Option<Arc<dyn peri_model::Model>>,
    agent_id: Option<AgentId>,
) -> V2SubagentContext {
    let session = match session {
        Some(s) => s,
        None => {
            let cwd_arc: Arc<str> = Arc::from(cwd);
            let frozen = FrozenContext::builder().build();
            let cancel_arc = Arc::new(cancel_token);
            // 自建兜底：独立 MessageQueue，无持久化
            let queue = MessageQueue::new();
            Session::new_with_cancel_and_queue(cwd_arc, frozen, None, cancel_arc, queue)
        }
    };

    let turn = session.start_turn();
    let transcript = session.transcript();
    let queue_clone = session.queue().clone();

    // tools → SharedToolMap（本地 tools 全部进 map）
    let mut tools_map: std::collections::BTreeMap<String, Arc<dyn BaseTool>> =
        std::collections::BTreeMap::new();
    for tool in tools {
        tools_map.insert(tool.name().to_string(), tool);
    }
    let combined_shared_tools: SharedToolMap = Arc::new(RwLock::new(tools_map));

    let (event_bus, event_handles) = EventBus::new(EventBusConfig::default());
    let event_bus_arc: Arc<EventBus> = Arc::new(event_bus);

    // 身份键统一（C1）：child_thread_id → AgentId；None（测试路径）内部生成。
    let resolved_agent_id = agent_id.unwrap_or_default();

    let session_context = Arc::new(RwLock::new(std::collections::HashMap::new()));
    let v2_llm: Arc<dyn ReactLLM + Send + Sync> = Arc::from(llm);

    let snapshot = tool_registry_snapshot.unwrap_or_default();

    let mut builder = StageContext::builder(turn, transcript, queue_clone)
        .with_agent_id(resolved_agent_id)
        .with_llm(v2_llm)
        .with_tools(combined_shared_tools)
        .with_tool_invocation_resolver(tool_invocation_resolver.unwrap_or_else(|| {
            Arc::new(DirectToolInvocationResolver) as Arc<dyn ToolInvocationResolver>
        }))
        .with_middleware_chain(Arc::new(chain))
        .with_event_bus(Arc::clone(&event_bus_arc))
        .with_session_context(session_context)
        .with_tool_registry_snapshot(snapshot);

    if let Some(reg) = error_suggest_registry {
        builder = builder.with_error_suggest_registry(reg);
    }
    if let Some(budget) = context_budget {
        builder = builder.with_context_budget(budget);
    }
    if let Some(cc) = compact_config {
        builder = builder.with_compact_config(cc);
    }
    if let Some(llm) = compact_llm {
        builder = builder.with_compact_llm(llm);
    }
    // system_prompt 由 spawn_subagent 以 BaseMessage::System 注入 transcript
    //（StageContext.system_prompt 为死字段，不写入）。

    let context = builder.build();

    V2SubagentContext {
        context,
        session,
        event_handles,
        agent_id: resolved_agent_id,
        event_bus: event_bus_arc,
    }
}

/// SubAgent v2 上下文构建器（3.0 批 2 注入面）。
///
/// `build_v2_subagent_context` 的 trait 封装：ACP workflow agent 等协议面
/// 经装配注入本 trait 调用（不直接引用本层实现），默认实现即委托
/// [`build_v2_subagent_context`]（[`DefaultSubagentV2ContextBuilder`]）。
#[allow(clippy::too_many_arguments)]
pub trait SubagentV2ContextBuilder: Send + Sync {
    /// 构造 SubAgent v2 上下文（参数与 [`build_v2_subagent_context`] 一致）。
    fn build(
        &self,
        session: Option<Arc<Session>>,
        llm: Box<dyn ReactLLM + Send + Sync>,
        chain: MiddlewareChain,
        tools: Vec<Arc<dyn BaseTool>>,
        cwd: &str,
        cancel_token: CancellationToken,
        tool_invocation_resolver: Option<Arc<dyn ToolInvocationResolver>>,
        error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
        tool_registry_snapshot: Option<ToolRegistrySnapshot>,
        compact_config: Option<CompactConfig>,
        context_budget: Option<ContextBudget>,
        compact_llm: Option<Arc<dyn peri_model::Model>>,
        agent_id: Option<AgentId>,
    ) -> V2SubagentContext;
}

/// [`SubagentV2ContextBuilder`] 的默认实现：委托 [`build_v2_subagent_context`]。
pub struct DefaultSubagentV2ContextBuilder;

#[allow(clippy::too_many_arguments)]
impl SubagentV2ContextBuilder for DefaultSubagentV2ContextBuilder {
    fn build(
        &self,
        session: Option<Arc<Session>>,
        llm: Box<dyn ReactLLM + Send + Sync>,
        chain: MiddlewareChain,
        tools: Vec<Arc<dyn BaseTool>>,
        cwd: &str,
        cancel_token: CancellationToken,
        tool_invocation_resolver: Option<Arc<dyn ToolInvocationResolver>>,
        error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
        tool_registry_snapshot: Option<ToolRegistrySnapshot>,
        compact_config: Option<CompactConfig>,
        context_budget: Option<ContextBudget>,
        compact_llm: Option<Arc<dyn peri_model::Model>>,
        agent_id: Option<AgentId>,
    ) -> V2SubagentContext {
        build_v2_subagent_context(
            session,
            llm,
            chain,
            tools,
            cwd,
            cancel_token,
            tool_invocation_resolver,
            error_suggest_registry,
            tool_registry_snapshot,
            compact_config,
            context_budget,
            compact_llm,
            agent_id,
        )
    }
}
