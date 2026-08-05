//! v2 stages 桥接 helper：把 SubAgent 装配产物（LLM + middlewares + tools + system_prompt）
//! 构造为可直接 `run_react_loop` 的 v2 `StageContext`。
//!
//! 设计：避免每个 SubAgent 调用点（define / execute_bg / execute_fork / spawner）
//! 重复写 60+ 行 v2 装配代码。

use std::sync::Arc;

use parking_lot::RwLock;
use peri_agent::{
    agent::{
        events_v2::{EventBus, EventBusConfig, EventHandles},
        react::ReactLLM,
        stages::{SharedToolMap, StageContext},
        CompactConfig, ContextBudget,
    },
    error_suggest::{ErrorSuggestRegistry, ToolRegistrySnapshot},
    group::pipeline::AgentId,
    messages::BaseMessage,
    middleware::chain::MiddlewareChain,
    session::{FrozenContext, MessageQueue, Session as V2Session},
    tools::BaseTool,
};
use tokio_util::sync::CancellationToken;

/// SubAgent v2 上下文产物
pub struct V2SubagentContext {
    /// v2 StageContext（传给 run_react_loop）
    pub context: StageContext,
    /// v2 Session（调用方持有以读取 transcript）
    pub session: Arc<V2Session>,
    /// EventBus 消费端（调用方 spawn forwarder 用）
    pub event_handles: EventHandles,
    /// 统一后的 subagent 身份键（= child_thread_id 的 AgentId 形式，
    /// 与 `context.session.agent_id` 同一值；事件侧 `ObserveEvent::*.agent_id` 使用它）
    pub agent_id: AgentId,
    /// EventBus 生产端（补发 SubagentStart/Stop 等 ObserveEvent 用；
    /// 与 `context.runtime.event_bus` 是同一 Arc）
    pub event_bus: Arc<EventBus>,
}

/// 从 `child_thread_id`（UUID v7 字符串）解析统一身份键 `AgentId`（C1）。
///
/// 身份契约：`child_thread_id`、subagent session `AgentId`、`instance_id`、
/// forwarder `source_agent_id`、事件 `agent_id` 收敛为同一 UUID。
/// 所有生成点（spawner / execute_fork / build_agent）均由 `uuid::Uuid::now_v7().to_string()`
/// 产生，parse 不可能失败，故直接 expect。
pub fn agent_id_from_child_thread(child_thread_id: &str) -> AgentId {
    AgentId::from_uuid(
        uuid::Uuid::parse_str(child_thread_id)
            .expect("child_thread_id 由 Uuid::now_v7() 生成，必为合法 UUID"),
    )
}

/// 经 child EventBus 补发 v2 `SubagentStart`（C2）。
///
/// `parent_agent_id` 为 None（未注入/测试路径）时不 emit，仅 tracing warn——
/// 防脏数据：缺父身份的事件会让 tracer 无法归属，宁可走 incomplete 分支。
/// 事件与 child 内容事件同通道同 forwarder，FIFO 保证 Start 先于内容事件到达。
pub(crate) fn emit_subagent_start_v2(
    event_bus: &Arc<EventBus>,
    turn_id: peri_agent::session::turn::TurnId,
    parent_agent_id: Option<AgentId>,
    child_agent_id: AgentId,
    agent_name: &str,
    is_background: bool,
) {
    let Some(parent_agent_id) = parent_agent_id else {
        tracing::warn!(
            target: "langfuse::subagent",
            child_agent_id = %child_agent_id,
            agent_name,
            "parent_agent_id 未注入，跳过 v2 SubagentStart emit（防脏数据）"
        );
        return;
    };
    event_bus.emit_observe(peri_agent::agent::events_v2::ObserveEvent::SubagentStart {
        turn_id,
        agent_id: parent_agent_id,
        child_agent_id,
        agent_name: agent_name.to_string(),
        is_background,
    });
}

/// 经 child EventBus 补发 v2 `SubagentStop`（C3）。
///
/// 与 [`emit_subagent_start_v2`] 同一通道；parent_agent_id 为 None 时同样跳过。
pub(crate) fn emit_subagent_stop_v2(
    event_bus: &Arc<EventBus>,
    turn_id: peri_agent::session::turn::TurnId,
    parent_agent_id: Option<AgentId>,
    child_agent_id: AgentId,
    agent_name: &str,
    result: &str,
    is_error: bool,
) {
    let Some(parent_agent_id) = parent_agent_id else {
        tracing::warn!(
            target: "langfuse::subagent",
            child_agent_id = %child_agent_id,
            agent_name,
            "parent_agent_id 未注入，跳过 v2 SubagentStop emit（防脏数据）"
        );
        return;
    };
    event_bus.emit_observe(peri_agent::agent::events_v2::ObserveEvent::SubagentStop {
        turn_id,
        agent_id: parent_agent_id,
        child_agent_id,
        agent_name: agent_name.to_string(),
        result: result.to_string(),
        is_error,
    });
}

/// 构造 SubAgent v2 上下文（不经过 AcpAgentConfig / build_agent）
///
/// 参数：
/// - `llm`：SubAgent LLM（ReactLLM 实现或裸 LLM）
/// - `chain`：已组装的中间件链
/// - `tools`：工具列表（Arc<Vec<Arc<dyn BaseTool>>>，可来自 parent_tools）
/// - `cwd`：工作目录
/// - `cancel_token`：CancellationToken（Cascade = parent.child_token()，Independent = new）
/// - `parent_messages`：fork 路径注入的父消息（非 fork 路径传空 Vec）
/// - `system_prompt`：SubAgent system prompt
/// - `shared_tools`：deferred tools 注册表（可选）
/// - `compact_config`：auto-compact 阈值配置（None = 不启用）
/// - `context_budget`：上下文预算（None = 不追踪 token 使用率）
/// - `compact_llm`：Full Compact 专用 LLM（None 时 Full Compact 跳过）
/// - `error_suggest_registry`：错误感知建议（可选）
/// - `tool_registry_snapshot`：工具注册表快照（None 用 default）
/// - `agent_id`：统一后的 subagent 身份键（= child_thread_id 解析的 AgentId）。
///   None 时内部 `AgentId::new()`（测试 / workflow 等无 child_thread_id 场景兜底）。
#[allow(clippy::too_many_arguments)]
pub fn build_v2_subagent_context(
    llm: Box<dyn ReactLLM + Send + Sync>,
    chain: MiddlewareChain,
    tools: Vec<Arc<dyn BaseTool>>,
    cwd: &str,
    cancel_token: CancellationToken,
    parent_messages: Vec<BaseMessage>,
    system_prompt: Option<String>,
    shared_tools: Option<SharedToolMap>,
    compact_config: Option<CompactConfig>,
    context_budget: Option<ContextBudget>,
    compact_llm: Option<Arc<dyn peri_model::Model>>,
    error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
    tool_registry_snapshot: Option<ToolRegistrySnapshot>,
    agent_id: Option<AgentId>,
) -> V2SubagentContext {
    let cwd_arc: Arc<str> = Arc::from(cwd);
    let frozen = FrozenContext::builder().build();
    let cancel_arc = Arc::new(cancel_token);
    // SubAgent 独立 MessageQueue（不与 main agent 共享）
    let queue = MessageQueue::new();
    let session = V2Session::new_with_cancel_and_queue(cwd_arc, frozen, None, cancel_arc, queue);

    let turn = session.start_turn();
    let transcript = session.transcript();
    let queue_clone = session.queue().clone();

    // fork 路径：把 parent_messages 注入 transcript（让子 agent 看到父会话上下文）
    if !parent_messages.is_empty() {
        let mut tx = transcript.write();
        for msg in parent_messages {
            tx.append(msg);
        }
    }

    // SubAgent system_prompt（身份构建）注入到 transcript 开头位置：
    // - fork 路径：在 parent_messages 之后（让身份提示词位于对话上下文之后、
    //   prompt 之前——SubAgent 的 prompt 由调用方 push 到 queue，Receive 阶段追加）
    // - 非 fork 路径：parent_messages 为空，直接 append 到 transcript 开头
    //
    // 注意：这是 session 起始身份构建（在 run_react_loop 调用前注入），不是中途纠正，
    // 用 BaseMessage::System 合法（CLAUDE.md TRAP 仅禁止中途纠正用 System）。
    // 模型桥接时 Provider 会把 System hoist 到请求顶层，与请求的 system 字段合并。
    if let Some(sp) = system_prompt {
        let mut tx = transcript.write();
        tx.append(BaseMessage::system(sp));
    }

    // tools → SharedToolMap（即使外部传 shared_tools，本地 tools 也合并进去）
    let mut tools_map: std::collections::BTreeMap<String, Arc<dyn BaseTool>> =
        std::collections::BTreeMap::new();
    for tool in tools {
        tools_map.insert(tool.name().to_string(), tool);
    }
    let combined_shared_tools: SharedToolMap = if let Some(shared) = shared_tools {
        // 合并：外部 deferred tools 写入到合并 map
        let external = shared.read();
        for (k, v) in external.iter() {
            tools_map.entry(k.clone()).or_insert_with(|| Arc::clone(v));
        }
        drop(external);
        Arc::new(RwLock::new(tools_map))
    } else {
        Arc::new(RwLock::new(tools_map))
    };

    let (event_bus, event_handles) = EventBus::new(EventBusConfig::default());
    let event_bus_arc: Arc<EventBus> = Arc::new(event_bus);

    // 身份键统一（C1）：child_thread_id → AgentId；None（测试/workflow 路径）内部生成。
    let resolved_agent_id = agent_id.unwrap_or_default();

    let session_context = Arc::new(RwLock::new(std::collections::HashMap::new()));
    let v2_llm: Arc<dyn ReactLLM + Send + Sync> = Arc::from(llm);

    let snapshot = tool_registry_snapshot.unwrap_or_default();

    let mut builder = StageContext::builder(turn, transcript, queue_clone)
        .with_agent_id(resolved_agent_id)
        .with_llm(v2_llm)
        .with_tools(combined_shared_tools)
        .with_tool_invocation_resolver(Arc::new(
            crate::tool_search::ExecuteExtraToolResolver::default(),
        ))
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
    // system_prompt 已作为 BaseMessage::System 注入 transcript（见上方 fork 路径后块）。
    // 不再写入 StageContext.system_prompt 死字段——peri-agent/src/agent/stages/ 内零代码读取该字段。
    // StageContextBuilder::with_system_prompt 已随字段移除，无残余调用方。

    let context = builder.build();

    V2SubagentContext {
        context,
        session,
        event_handles,
        agent_id: resolved_agent_id,
        event_bus: event_bus_arc,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, sync::Mutex};

    use async_trait::async_trait;
    use peri_agent::{
        agent::{
            react::{Reasoning, ToolCall},
            stages::tool_dispatch::dispatch_tools,
        },
        tools::ToolContext,
    };
    use serde_json::json;

    struct SnapshotTool {
        name: &'static str,
        output: &'static str,
        limit: Option<usize>,
        invoked: Arc<Mutex<usize>>,
    }

    #[async_trait]
    impl BaseTool for SnapshotTool {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            "snapshot test tool"
        }

        fn parameters(&self) -> serde_json::Value {
            json!({})
        }

        fn output_char_limit(&self) -> Option<usize> {
            self.limit
        }

        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            *self.invoked.lock().unwrap() += 1;
            Ok(self.output.to_string())
        }
    }

    #[tokio::test]
    async fn combined_stage_context_snapshot_owns_wrapper_target_resolution() {
        let local_invoked = Arc::new(Mutex::new(0));
        let external_invoked = Arc::new(Mutex::new(0));
        let local: Arc<dyn BaseTool> = Arc::new(SnapshotTool {
            name: "DeferredTarget",
            output: "local-output",
            limit: Some(5),
            invoked: Arc::clone(&local_invoked),
        });
        let external: Arc<dyn BaseTool> = Arc::new(SnapshotTool {
            name: "DeferredTarget",
            output: "external-output",
            limit: None,
            invoked: Arc::clone(&external_invoked),
        });
        let external_tools = Arc::new(RwLock::new(BTreeMap::from([(
            "DeferredTarget".to_string(),
            external,
        )])));
        let wrapper: Arc<dyn BaseTool> = Arc::new(crate::tool_search::ExecuteExtraTool::new(
            Arc::clone(&external_tools),
        ));
        let built = build_v2_subagent_context(
            Box::new(peri_agent::agent::stages::NullReactLLM),
            MiddlewareChain::new(),
            vec![local, wrapper],
            "/tmp",
            CancellationToken::new(),
            Vec::new(),
            None,
            Some(external_tools),
            None,
            None,
            None,
            None,
            None,
            None, // 测试兜底：内部 AgentId::new()
        );
        let reasoning = Reasoning::with_tools(
            "",
            vec![ToolCall::new(
                "call-1",
                crate::tool_search::EXECUTE_EXTRA_TOOL_NAME,
                json!({"tool_name": "DeferredTarget", "params": {}}),
            )],
        );

        let outcome = dispatch_tools(&built.context, &reasoning, &CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(*local_invoked.lock().unwrap(), 1);
        assert_eq!(*external_invoked.lock().unwrap(), 0);
        assert_eq!(
            outcome.results[0].1.output,
            "local\n\n[Output truncated at 5 chars]"
        );
    }

    // ─── C1 身份键统一契约测试 ──────────────────────────────────────────────

    fn build_ctx_with(agent_id: Option<AgentId>) -> V2SubagentContext {
        build_v2_subagent_context(
            Box::new(peri_agent::agent::stages::NullReactLLM),
            MiddlewareChain::new(),
            Vec::new(),
            "/tmp",
            CancellationToken::new(),
            Vec::new(),
            None,
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
        let fixed = AgentId::from_uuid(
            uuid::Uuid::parse_str("00000000-0000-7000-8000-000000000001").unwrap(),
        );
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
        let agent_id = super::agent_id_from_child_thread(&child_thread_id);
        assert_eq!(
            agent_id.to_string(),
            child_thread_id,
            "AgentId 字符串形式必须与 child_thread_id 完全一致"
        );
        // 与 events_v2_mapper 语义对齐：child_agent_id.to_string() 即 instance_id
        assert_eq!(agent_id.as_uuid().to_string(), child_thread_id);
    }
}
