//! RCRA 阶段共享依赖与构造，执行循环只消费此边界。

use super::*;

// ─── 阶段间共享上下文子结构 ─────────────────────────────────────────────────

/// 会话级实体（生命周期 = 整个 Agent Session）
#[derive(Clone)]
pub struct SessionHandle {
    pub turn: Arc<TurnContext>,
    pub transcript: Arc<RwLock<MessageTranscript>>,
    pub queue: MessageQueue,
    pub user_input_mailbox: Option<Arc<crate::session::user_input_mailbox::UserInputMailbox>>,
    /// 会话级后台结果处理责任 owner（与 SessionTerminalDelivery 共用同一实例）。
    pub async_result_mailbox: Arc<crate::session::async_result_mailbox::AsyncResultMailbox>,
    pub agent_id: AgentId,
    /// metrics/tracing 用键值对（不作为 middleware hook 的隐式共享协议）
    pub session_context: Arc<RwLock<HashMap<String, String>>>,
}

/// LLM 调用 + 工具执行运行时服务
#[derive(Clone)]
pub struct RuntimeServices {
    pub llm: Arc<dyn ReactLLM + Send + Sync>,
    /// Mutable middleware working view. Dispatch never resolves from this map.
    pub tools: SharedToolMap,
    /// Session-local immutable catalog publisher.
    pub tool_catalog: Arc<SessionToolCatalog>,
    /// 每个 dispatch 使用其工具表 snapshot 的 canonical invocation resolver。
    pub tool_invocation_resolver: Arc<dyn ToolInvocationResolver>,
    pub middleware_chain: Arc<MiddlewareChain>,
    pub event_bus: Arc<EventBus>,
    /// Deferred tools 外部注册表（ExecuteExtraTool 代理执行用）
    pub shared_tools: Option<SharedToolMap>,
}

/// Compact 系统上下文（含跨阶段计数器）
#[derive(Clone)]
pub struct CompactContext {
    pub context_budget: Option<ContextBudget>,
    pub compact_config: Option<CompactConfig>,
    pub compact_llm: Option<Arc<dyn peri_model::Model>>,
    pub compact_pre_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    pub compact_post_hook: Option<Arc<dyn Fn(bool, usize) + Send + Sync>>,
    /// 会话级 Token 追踪器（Compact 写 reset/estimated_tokens，Act 读用于 StateSnapshot）
    pub token_tracker: Arc<RwLock<crate::agent::token::TokenTracker>>,
    /// 连续工具失败计数（tool_dispatch 递增/重置，Act 读用于 StateSnapshot）
    pub consecutive_failures: Arc<AtomicU32>,
    /// Compact 连续失败计数（run_compact 内部递增/重置，仅用于 Compact 降级跳过决策）
    pub compact_consecutive_failures: Arc<AtomicU32>,
    pub(crate) exposure:
        Arc<parking_lot::Mutex<crate::agent::compact_v2::exposure::ExposureLedger>>,
    pub(crate) budget_recovery: Arc<parking_lot::Mutex<compact_progress::CompactBudgetRecovery>>,
}

// ─── 阶段间共享上下文 ───────────────────────────────────────────────────────

/// 阶段间共享的会话资源引用
///
/// 所有阶段通过此结构体访问 Session 实体，不直接持有 Session。
///
/// **P2 扩展**：加入 LLM / 工具 / 中间件链 / EventBus / Compact 等运行时依赖，
/// 让 stages 可以自驱完整 ReAct 循环，由 [`run_react_loop`] 入口统一驱动。
#[derive(Clone)]
pub struct StageContext {
    pub session: SessionHandle,
    pub runtime: RuntimeServices,
    pub compact: CompactContext,
    pub async_ctx: AsyncContext,
    /// 当前 session 的 Goal 只读/控制投影源。
    pub goal_controller: Option<Arc<dyn peri_acp_types::goal::GoalController>>,
    /// Recall 累加器（跨 middleware hook 共享）。
    ///
    /// 每次 middleware hook 都会构造临时 [`AgentContext`]，
    /// 调用结束后由 middleware_runner 把 AgentContext 内部
    /// recall_buffer drain 到本缓冲区，循环结束后由 executor 统一取出。
    pub recall_buffer: Arc<RwLock<Vec<String>>>,
    /// Reason 准备阶段的模型视图复用槽（生命周期由 `run_reason` 显式划定：
    /// 入口 reset、取到请求快照后 release）。
    pub(crate) model_view: Arc<ModelViewScope>,
}

impl StageContext {
    /// canonical session 身份投影（`session_context["session_id"]`）。
    ///
    /// **单一权威**：调用期事实（`InvocationContext`）与 middleware 状态
    /// （`AgentContext`）都从这里取 session 身份，不得各自复写投影规则。
    ///
    /// **缺失即 `None`**：`session_id` 是 ACP 会话身份，只有真实会话才能给出
    /// （主 agent 在 `StageContext` 构造期写入；子 agent 自建 session 没有
    /// thread 绑定时不写）。`agent_id` 是执行实例身份，不是会话身份，**不得**
    /// 用作回退——否则会铸造出「可信会话归属」的事实，让审批的 fail-closed
    /// 守卫（`interaction_owner::untrusted_owner_error`）变成死分支。
    pub(crate) fn project_session_id(&self) -> Option<String> {
        self.session
            .session_context
            .read()
            .get("session_id")
            .cloned()
    }

    /// 测试最小上下文：仅传会话实体，运行时依赖按测试行为填充。
    ///
    /// **注意**：此构造函数仅用于单元测试。生产代码请用 `StageContextBuilder`。
    #[cfg(test)]
    pub fn new(
        turn: TurnContext,
        transcript: Arc<RwLock<MessageTranscript>>,
        queue: MessageQueue,
    ) -> Self {
        let turn_arc = Arc::new(turn);
        let tools_map: SharedToolMap = Arc::new(RwLock::new(BTreeMap::new()));
        let mw_chain = Arc::new(MiddlewareChain::new());
        let ebus = Arc::new(EventBus::new(Default::default()).0);
        let ttracker = Arc::new(parking_lot::RwLock::new(
            crate::agent::token::TokenTracker::default(),
        ));
        let tool_fail = Arc::new(AtomicU32::new(0));
        let compact_fail = Arc::new(AtomicU32::new(0));
        let sctx = Arc::new(RwLock::new(std::collections::HashMap::new()));
        let rbuf = Arc::new(RwLock::new(Vec::new()));
        Self {
            session: SessionHandle {
                turn: turn_arc,
                transcript,
                queue,
                user_input_mailbox: None,
                async_result_mailbox: Arc::new(
                    crate::session::async_result_mailbox::AsyncResultMailbox::default(),
                ),
                agent_id: AgentId::new(),
                session_context: sctx,
            },
            runtime: RuntimeServices {
                llm: Arc::new(NullReactLLM),
                tools: Arc::clone(&tools_map),
                tool_catalog: Arc::new(SessionToolCatalog::new(BTreeMap::new(), None)),
                tool_invocation_resolver: Arc::new(DirectToolInvocationResolver),
                middleware_chain: mw_chain,
                event_bus: ebus,
                shared_tools: None,
            },
            compact: CompactContext {
                context_budget: None,
                compact_config: None,
                compact_llm: None,
                compact_pre_hook: None,
                compact_post_hook: None,
                token_tracker: ttracker,
                consecutive_failures: tool_fail,
                compact_consecutive_failures: compact_fail,
                exposure: Arc::default(),
                budget_recovery: Arc::new(parking_lot::Mutex::new(Default::default())),
            },
            async_ctx: AsyncContext {
                child_settlement: None,
                received_after_deadline: Arc::default(),
                unprocessed_tool_calls: Arc::default(),
                idle_wait_enabled: false,
                idle_should_wait: None,
                pending_handoff: None,
                handoff_deadline: None,
                idle_registry: None,
                idle_suspended_flag: None,
                hook_stop_requested: None,
            },
            goal_controller: None,
            recall_buffer: rbuf,
            model_view: Arc::new(ModelViewScope::default()),
        }
    }

    /// 创建 builder（生产代码推荐路径）
    pub fn builder(
        turn: TurnContext,
        transcript: Arc<RwLock<MessageTranscript>>,
        queue: MessageQueue,
    ) -> StageContextBuilder {
        StageContextBuilder {
            session: SessionHandle {
                turn: Arc::new(turn),
                transcript,
                queue,
                user_input_mailbox: None,
                async_result_mailbox: Arc::new(
                    crate::session::async_result_mailbox::AsyncResultMailbox::default(),
                ),
                agent_id: AgentId::new(),
                session_context: Arc::new(RwLock::new(std::collections::HashMap::new())),
            },
            runtime: RuntimeServices {
                llm: Arc::new(NullReactLLM),
                tools: Arc::new(RwLock::new(BTreeMap::new())),
                tool_catalog: Arc::new(SessionToolCatalog::new(BTreeMap::new(), None)),
                tool_invocation_resolver: Arc::new(DirectToolInvocationResolver),
                middleware_chain: Arc::new(MiddlewareChain::new()),
                event_bus: Arc::new(EventBus::new(Default::default()).0),
                shared_tools: None,
            },
            compact: CompactContext {
                context_budget: None,
                compact_config: None,
                compact_llm: None,
                compact_pre_hook: None,
                compact_post_hook: None,
                token_tracker: Arc::new(parking_lot::RwLock::new(
                    crate::agent::token::TokenTracker::default(),
                )),
                consecutive_failures: Arc::new(AtomicU32::new(0)),
                compact_consecutive_failures: Arc::new(AtomicU32::new(0)),
                exposure: Arc::default(),
                budget_recovery: Arc::new(parking_lot::Mutex::new(Default::default())),
            },
            async_ctx: AsyncContext {
                child_settlement: None,
                received_after_deadline: Arc::default(),
                unprocessed_tool_calls: Arc::default(),
                idle_wait_enabled: false,
                idle_should_wait: None,
                pending_handoff: None,
                handoff_deadline: None,
                idle_registry: None,
                idle_suspended_flag: None,
                hook_stop_requested: None,
            },
            goal_controller: None,
        }
    }

    /// 便捷访问：当前 turn_id
    pub fn turn_id(&self) -> crate::session::turn::TurnId {
        self.session.turn.turn_id()
    }

    /// 便捷访问：当前 cwd
    pub fn cwd(&self) -> &str {
        &self.session.turn.cwd
    }

    /// 取出可见消息快照（已过滤 excluded 标记）
    pub fn visible_messages(&self) -> Vec<BaseMessage> {
        self.session
            .transcript
            .read()
            .visible_messages()
            .into_iter()
            .cloned()
            .collect()
    }
}

// ─── StageContextBuilder ────────────────────────────────────────────────────

/// StageContext 构建器
///
/// 必填：turn / transcript / queue / llm（生产场景）
/// 可选：tools / middleware_chain / event_bus / budget / compact_config 等
pub struct StageContextBuilder {
    pub(super) session: SessionHandle,
    pub(super) runtime: RuntimeServices,
    pub(super) compact: CompactContext,
    pub(super) async_ctx: AsyncContext,
    pub(super) goal_controller: Option<Arc<dyn peri_acp_types::goal::GoalController>>,
}
