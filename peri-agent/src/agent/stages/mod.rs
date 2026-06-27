//! ReAct v2 — 五阶段循环
//!
//! 每阶段有明确的类型契约（StageInput → StageOutput），可脱离完整 Agent 单独测试。
//! 阶段间依赖通过输入结构体声明，不读全局状态。
//!
//! 控制流：`Compact → Receive → Reason → Act → (有 tool_calls 回 Compact，无则) → End`
//! End 检查队列：有 Prompt/Defer 则下个 turn，无则退出。

pub mod act;
pub mod compact;
pub mod end;
pub mod middleware_runner;
pub mod reason;
pub mod receive;
pub mod tool_dispatch;

use std::collections::HashMap;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::agent::compact::config::CompactConfig;
use crate::agent::events_v2::EventBus;
use crate::agent::react::ReactLLM;
use crate::agent::token::ContextBudget;
use crate::error_suggest::{ErrorSuggestRegistry, ToolRegistrySnapshot};
use crate::group::pipeline::AgentId;
use crate::llm::BaseModel;
use crate::messages::BaseMessage;
use crate::middleware::chain::MiddlewareChain;
use crate::session::turn::TurnContext;
use crate::session::{MessageQueue, MessageTranscript, QueuedMessage};
use crate::tools::BaseTool;

/// 共享工具注册表类型别名（避免 clippy::type_complexity）
pub type SharedToolMap = Arc<RwLock<HashMap<String, Arc<dyn BaseTool>>>>;

// ─── 循环控制 ───────────────────────────────────────────────────────────────

/// 循环继续方向
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopAction {
    /// 回到 Compact 开始新 step（有 tool_calls）
    NextStep,
    /// 进入 End 阶段检查是否需要新 turn
    CheckEnd,
}

/// 循环最终结果
#[derive(Debug)]
pub enum LoopResult {
    /// 正常结束（无更多消息）
    Completed,
    /// 被中断
    Interrupted,
    /// 错误
    Error(crate::error::AgentError),
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
    // ── 会话级实体（v2 原生）──
    /// Turn 上下文（turn_id / step / cancel）
    pub turn: Arc<TurnContext>,
    /// 对话笔录（RwLock 保护，标记代替删除）
    pub transcript: Arc<RwLock<MessageTranscript>>,
    /// 收件箱
    pub queue: MessageQueue,
    /// 当前 Agent 标识（事件总线路由用）
    pub agent_id: AgentId,

    // ── 运行时依赖（P2 扩展）──
    /// LLM 适配器（Reason 阶段调用）
    pub llm: Arc<dyn ReactLLM + Send + Sync>,
    /// 工具注册表（LLM 可见 + 可执行）
    pub tools: Arc<RwLock<HashMap<String, Arc<dyn BaseTool>>>>,
    /// 中间件链（驱动 before_model / after_tool 等钩子）
    pub middleware_chain: Arc<MiddlewareChain>,
    /// 事件总线（三层事件流）
    pub event_bus: Arc<EventBus>,
    /// 上下文预算（token 监控 + auto compact 触发）
    pub context_budget: Option<ContextBudget>,
    /// Compact 配置（Compact 阶段使用）
    pub compact_config: Option<CompactConfig>,
    /// Compact 专用 LLM（Full Compact 摘要请求；None 时 Full Compact 跳过）
    pub compact_llm: Option<Arc<dyn BaseModel>>,
    /// 共享工具注册表（供 ExecuteExtraTool 代理执行 deferred tools）
    pub shared_tools: Option<SharedToolMap>,
    /// 错误感知建议注册表（None = 不启用）
    pub error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
    /// 工具注册表快照（工具名 + subagent 类型，供 suggester 查询）
    pub tool_registry_snapshot: Arc<ToolRegistrySnapshot>,
    /// Frozen system prompt（构造时一次性确定）
    pub system_prompt: Option<String>,
    /// 连续失败计数（工具失败检测，跨 step 共享）
    pub consecutive_failures: Arc<AtomicU32>,
    /// 会话上下文键值（session_id / run_id 等，metrics/tracing 用）
    pub session_context: Arc<RwLock<HashMap<String, String>>>,
    /// Recall 累加器（跨 middleware hook 共享）。
    ///
    /// 每次 middleware hook 都会构造临时 AgentState
    /// （见 [`super::middleware_runner::snapshot_to_agent_state`]），调用结束
    /// 后由 `restore_from_agent_state` 整体写回 transcript——但 recall 字段
    /// 不属于 transcript，会被丢弃。本字段作为跨 hook 的等价缓冲区：每次
    /// middleware hook 调用结束后，[`restore_from_agent_state`] 把 state 中
    /// 新增的 recall drain 到本缓冲区，循环结束后由 executor 统一取出。
    pub recall_buffer: Arc<RwLock<Vec<String>>>,

    // ── Compact hook 回调（插件 PreCompact/PostCompact 触发）──
    /// Pre-compact 插件 hook 回调（可选）。由 ACP 层注入。
    pub compact_pre_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Post-compact 插件 hook 回调（可选）。由 ACP 层注入。
    /// 参数: (compacted, affected_count)
    pub compact_post_hook: Option<Arc<dyn Fn(bool, usize) + Send + Sync>>,
}

impl StageContext {
    /// 兼容旧测试：仅传会话实体时构造 minimal context（运行时字段需要单独填充）
    ///
    /// **注意**：此构造函数仅用于单元测试。生产代码请用 `StageContextBuilder`。
    pub fn new(
        turn: TurnContext,
        transcript: Arc<RwLock<MessageTranscript>>,
        queue: MessageQueue,
    ) -> Self {
        Self {
            turn: Arc::new(turn),
            transcript,
            queue,
            agent_id: AgentId::new(),
            llm: Arc::new(NullReactLLM),
            tools: Arc::new(RwLock::new(HashMap::new())),
            middleware_chain: Arc::new(MiddlewareChain::new()),
            event_bus: Arc::new(EventBus::new(Default::default()).0),
            context_budget: None,
            compact_config: None,
            compact_llm: None,
            shared_tools: None,
            error_suggest_registry: None,
            tool_registry_snapshot: Arc::new(ToolRegistrySnapshot::default()),
            system_prompt: None,
            consecutive_failures: Arc::new(AtomicU32::new(0)),
            session_context: Arc::new(RwLock::new(HashMap::new())),
            recall_buffer: Arc::new(RwLock::new(Vec::new())),
            compact_pre_hook: None,
            compact_post_hook: None,
        }
    }

    /// 创建 builder（生产代码推荐路径）
    pub fn builder(
        turn: TurnContext,
        transcript: Arc<RwLock<MessageTranscript>>,
        queue: MessageQueue,
    ) -> StageContextBuilder {
        StageContextBuilder {
            turn: Arc::new(turn),
            transcript,
            queue,
            agent_id: None,
            inner: Default::default(),
        }
    }

    /// 便捷访问：当前 turn_id
    pub fn turn_id(&self) -> crate::session::turn::TurnId {
        self.turn.turn_id
    }

    /// 便捷访问：当前 cwd
    pub fn cwd(&self) -> &str {
        &self.turn.cwd
    }

    /// 取出可见消息快照（已过滤 excluded 标记）
    pub fn visible_messages(&self) -> Vec<BaseMessage> {
        self.transcript
            .read()
            .visible_messages()
            .into_iter()
            .cloned()
            .collect()
    }
}

/// 空 ReactLLM——用于未配置 LLM 的测试场景
///
/// 调用时返回 Interrupted 错误，避免 stub 默认行为掩盖生产配置缺失。
#[derive(Debug, Default, Clone, Copy)]
pub struct NullReactLLM;

#[async_trait::async_trait]
impl ReactLLM for NullReactLLM {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<crate::llm::types::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        Err(crate::error::AgentError::Interrupted)
    }

    fn model_name(&self) -> String {
        "null".to_string()
    }
}

// ─── StageContextBuilder ────────────────────────────────────────────────────

/// StageContext 构建器
///
/// 必填：turn / transcript / queue / llm（生产场景）
/// 可选：tools / middleware_chain / event_bus / budget / compact_config 等
pub struct StageContextBuilder {
    turn: Arc<TurnContext>,
    transcript: Arc<RwLock<MessageTranscript>>,
    queue: MessageQueue,
    agent_id: Option<AgentId>,
    inner: StageContextInner,
}

#[derive(Default)]
struct StageContextInner {
    llm: Option<Arc<dyn ReactLLM + Send + Sync>>,
    tools: Option<SharedToolMap>,
    middleware_chain: Option<Arc<MiddlewareChain>>,
    event_bus: Option<Arc<EventBus>>,
    context_budget: Option<ContextBudget>,
    compact_config: Option<CompactConfig>,
    compact_llm: Option<Arc<dyn BaseModel>>,
    shared_tools: Option<SharedToolMap>,
    error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
    tool_registry_snapshot: Option<Arc<ToolRegistrySnapshot>>,
    system_prompt: Option<String>,
    session_context: Option<Arc<RwLock<HashMap<String, String>>>>,
    compact_pre_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    compact_post_hook: Option<Arc<dyn Fn(bool, usize) + Send + Sync>>,
}

impl StageContextBuilder {
    pub fn with_llm(mut self, llm: Arc<dyn ReactLLM + Send + Sync>) -> Self {
        self.inner.llm = Some(llm);
        self
    }

    pub fn with_tools(mut self, tools: SharedToolMap) -> Self {
        self.inner.tools = Some(tools);
        self
    }

    pub fn with_middleware_chain(mut self, chain: Arc<MiddlewareChain>) -> Self {
        self.inner.middleware_chain = Some(chain);
        self
    }

    pub fn with_event_bus(mut self, bus: Arc<EventBus>) -> Self {
        self.inner.event_bus = Some(bus);
        self
    }

    pub fn with_context_budget(mut self, budget: ContextBudget) -> Self {
        self.inner.context_budget = Some(budget);
        self
    }

    pub fn with_compact_config(mut self, config: CompactConfig) -> Self {
        self.inner.compact_config = Some(config);
        self
    }

    pub fn with_compact_llm(mut self, llm: Arc<dyn BaseModel>) -> Self {
        self.inner.compact_llm = Some(llm);
        self
    }

    pub fn with_shared_tools(mut self, shared: SharedToolMap) -> Self {
        self.inner.shared_tools = Some(shared);
        self
    }

    pub fn with_error_suggest_registry(mut self, registry: Arc<ErrorSuggestRegistry>) -> Self {
        self.inner.error_suggest_registry = Some(registry);
        self
    }

    pub fn with_tool_registry_snapshot(mut self, snapshot: ToolRegistrySnapshot) -> Self {
        self.inner.tool_registry_snapshot = Some(Arc::new(snapshot));
        self
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.inner.system_prompt = Some(prompt.into());
        self
    }

    pub fn with_agent_id(mut self, agent_id: AgentId) -> Self {
        self.agent_id = Some(agent_id);
        self
    }

    pub fn with_session_context(mut self, ctx: Arc<RwLock<HashMap<String, String>>>) -> Self {
        self.inner.session_context = Some(ctx);
        self
    }

    pub fn with_compact_pre_hook(mut self, hook: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.inner.compact_pre_hook = Some(hook);
        self
    }

    pub fn with_compact_post_hook(mut self, hook: Arc<dyn Fn(bool, usize) + Send + Sync>) -> Self {
        self.inner.compact_post_hook = Some(hook);
        self
    }

    pub fn build(self) -> StageContext {
        StageContext {
            turn: self.turn,
            transcript: self.transcript,
            queue: self.queue,
            agent_id: self.agent_id.unwrap_or_default(),
            llm: self.inner.llm.unwrap_or_else(|| Arc::new(NullReactLLM)),
            tools: self
                .inner
                .tools
                .unwrap_or_else(|| Arc::new(RwLock::new(HashMap::new()))),
            middleware_chain: self
                .inner
                .middleware_chain
                .unwrap_or_else(|| Arc::new(MiddlewareChain::new())),
            event_bus: self
                .inner
                .event_bus
                .unwrap_or_else(|| Arc::new(EventBus::new(Default::default()).0)),
            context_budget: self.inner.context_budget,
            compact_config: self.inner.compact_config,
            compact_llm: self.inner.compact_llm,
            shared_tools: self.inner.shared_tools,
            error_suggest_registry: self.inner.error_suggest_registry,
            tool_registry_snapshot: self
                .inner
                .tool_registry_snapshot
                .unwrap_or_else(|| Arc::new(ToolRegistrySnapshot::default())),
            system_prompt: self.inner.system_prompt,
            consecutive_failures: Arc::new(AtomicU32::new(0)),
            session_context: self
                .inner
                .session_context
                .unwrap_or_else(|| Arc::new(RwLock::new(HashMap::new()))),
            recall_buffer: Arc::new(RwLock::new(Vec::new())),
            compact_pre_hook: self.inner.compact_pre_hook,
            compact_post_hook: self.inner.compact_post_hook,
        }
    }
}

// ─── Compact 阶段类型 ────────────────────────────────────────────────────────

/// Compact 阶段输入
pub struct CompactInput {
    pub context: StageContext,
    /// 上一步 Act 是否产出了 tool_calls（首次进入 turn 时为 false）
    pub has_tool_calls: bool,
}

/// Compact 阶段输出
pub struct CompactOutput {
    /// 是否执行了 compact（用于事件追踪）
    pub compacted: bool,
}

// ─── Receive 阶段类型 ────────────────────────────────────────────────────────

/// Receive 阶段输入
pub struct ReceiveInput {
    pub context: StageContext,
}

/// Receive 阶段输出
pub struct ReceiveOutput {
    /// 本轮消费的消息数量
    pub consumed_count: usize,
}

// ─── Reason 阶段类型 ─────────────────────────────────────────────────────────

/// Reason 阶段输入
pub struct ReasonInput {
    pub context: StageContext,
    /// 上一步 Act 是否产出了 tool_calls（用于构建 LLM 请求上下文）
    pub has_tool_calls: bool,
}

/// Reason 阶段输出
#[derive(Debug)]
pub struct ReasonOutput {
    /// LLM 推理结果（含 tool_calls 或 final_answer）
    pub reasoning: crate::agent::react::Reasoning,
    /// LLM 请求使用的消息快照（用于调试/追踪）
    pub messages_snapshot: Vec<BaseMessage>,
}

// ─── Act 阶段类型 ────────────────────────────────────────────────────────────

/// Act 阶段输入
pub struct ActInput {
    pub context: StageContext,
    /// Reason 阶段的推理结果
    pub reasoning: crate::agent::react::Reasoning,
}

/// Act 阶段输出
pub struct ActOutput {
    /// 是否有工具调用
    pub has_tool_calls: bool,
    /// 最终回答文本（无 tool_calls 时）
    pub final_answer: Option<String>,
}

// ─── End 阶段类型 ───────────────────────────────────────────────────────────

/// End 阶段输入
pub struct EndInput {
    pub context: StageContext,
}

/// End 阶段输出
pub struct EndOutput {
    /// 是否有新消息需要继续（Prompt / Defer）
    pub should_continue: bool,
    /// End 阶段排空的消息（唤醒新 turn 的）
    pub awakened_messages: Vec<QueuedMessage>,
}

// ─── 控制流编排 ──────────────────────────────────────────────────────────────

/// 运行 ReAct v2 五阶段循环
///
/// 返回循环最终结果（Completed / Interrupted / Error）。
pub async fn run_react_loop(context: StageContext, max_iterations: usize) -> LoopResult {
    let mut has_tool_calls = false;

    for _ in 0..max_iterations {
        // 检查 cancel
        if context.turn.is_cancelled() {
            return LoopResult::Interrupted;
        }

        // 推进 step
        context.turn.advance_step();

        // ── Compact ──
        let _compact_out = match compact::run_compact(CompactInput {
            context: context.clone(),
            has_tool_calls,
        })
        .await
        {
            Ok(out) => out,
            Err(e) => return LoopResult::Error(e),
        };

        // ── Receive ──
        let _receive_out = match receive::run_receive(ReceiveInput {
            context: context.clone(),
        })
        .await
        {
            Ok(out) => out,
            Err(e) => return LoopResult::Error(e),
        };

        // ── Reason ──
        let reason_out = match reason::run_reason(ReasonInput {
            context: context.clone(),
            has_tool_calls,
        })
        .await
        {
            Ok(out) => out,
            Err(e) => return LoopResult::Error(e),
        };

        // ── Act ──
        let act_out = match act::run_act(ActInput {
            context: context.clone(),
            reasoning: reason_out.reasoning,
        })
        .await
        {
            Ok(out) => out,
            Err(e) => return LoopResult::Error(e),
        };

        has_tool_calls = act_out.has_tool_calls;

        // 有 tool_calls → 回 Compact（跳过 End）
        if has_tool_calls {
            tracing::debug!(
                step = context.turn.current_step(),
                "tool_calls 存在，回到 Compact"
            );
            continue;
        }

        // ── End ──
        let end_out = end::run_end(EndInput {
            context: context.clone(),
        });

        if end_out.should_continue {
            // 有新消息唤醒新 turn → 回 Compact
            has_tool_calls = false;
            tracing::debug!(
                awakened = end_out.awakened_messages.len(),
                "End 阶段有新消息，开始新 turn"
            );
            continue;
        }

        // 队列空 → 退出
        return LoopResult::Completed;
    }

    // 达到最大迭代次数
    tracing::warn!(max_iterations, "ReAct v2 循环达到最大迭代次数");
    LoopResult::Error(crate::error::AgentError::MaxIterationsExceeded(
        max_iterations,
    ))
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::MessageContent;
    use crate::session::queue::MessageSource;
    use crate::session::store::FrozenContext;
    use crate::session::Session;

    /// 构造测试用 StageContext
    fn make_stage_context() -> StageContext {
        let cwd: Arc<str> = Arc::from("/tmp/test");
        let frozen = FrozenContext::builder()
            .system_prompt("You are a test agent.")
            .build();
        let session = Session::new(cwd, frozen, None);
        let turn = session.start_turn();
        StageContext::new(turn, session.transcript(), session.queue().clone())
    }

    // ── 类型契约测试 ──

    #[test]
    fn test_compact_input_output_contract() {
        let ctx = make_stage_context();
        let input = CompactInput {
            context: ctx,
            has_tool_calls: false,
        };
        assert!(!input.has_tool_calls);

        let output = CompactOutput { compacted: false };
        assert!(!output.compacted);
    }

    #[test]
    fn test_receive_input_output_contract() {
        let ctx = make_stage_context();
        let _input = ReceiveInput { context: ctx };
        let output = ReceiveOutput { consumed_count: 0 };
        assert_eq!(output.consumed_count, 0);
    }

    #[test]
    fn test_reason_input_output_contract() {
        let ctx = make_stage_context();
        let _input = ReasonInput {
            context: ctx,
            has_tool_calls: false,
        };
        let reasoning = crate::agent::react::Reasoning::with_answer("thinking", "answer");
        let output = ReasonOutput {
            reasoning,
            messages_snapshot: vec![],
        };
        assert!(!output.reasoning.needs_tool_call());
        assert!(output.messages_snapshot.is_empty());
    }

    #[test]
    fn test_act_input_output_contract() {
        let ctx = make_stage_context();
        let reasoning = crate::agent::react::Reasoning::with_answer("thinking", "done");
        let _input = ActInput {
            context: ctx,
            reasoning,
        };

        let output_with_tools = ActOutput {
            has_tool_calls: true,
            final_answer: None,
        };
        assert!(output_with_tools.has_tool_calls);
        assert!(output_with_tools.final_answer.is_none());

        let output_no_tools = ActOutput {
            has_tool_calls: false,
            final_answer: Some("done".to_string()),
        };
        assert!(!output_no_tools.has_tool_calls);
        assert_eq!(output_no_tools.final_answer.as_deref(), Some("done"));
    }

    #[test]
    fn test_end_input_output_contract() {
        let ctx = make_stage_context();
        let _input = EndInput { context: ctx };

        let output_continue = EndOutput {
            should_continue: true,
            awakened_messages: vec![],
        };
        assert!(output_continue.should_continue);

        let output_stop = EndOutput {
            should_continue: false,
            awakened_messages: vec![],
        };
        assert!(!output_stop.should_continue);
    }

    #[test]
    fn test_loop_action_and_result_types() {
        // LoopAction
        assert_eq!(LoopAction::NextStep, LoopAction::NextStep);
        assert_ne!(LoopAction::NextStep, LoopAction::CheckEnd);

        // LoopResult
        let _ = LoopResult::Completed;
        let _ = LoopResult::Interrupted;
        let _ = LoopResult::Error(crate::error::AgentError::MaxIterationsExceeded(0));
    }

    #[test]
    fn test_stage_context_construction() {
        let ctx = make_stage_context();
        assert_eq!(&*ctx.turn.cwd, "/tmp/test");
        assert_eq!(ctx.turn.current_step(), 0);
        assert!(ctx.queue.is_empty());
        assert!(ctx.transcript.read().is_empty());
    }

    #[test]
    fn test_stage_context_builder_default() {
        // builder 不传 llm 时，自动 fallback 到 NullReactLLM
        let cwd: Arc<str> = Arc::from("/tmp");
        let frozen = FrozenContext::builder().build();
        let session = Session::new(cwd, frozen, None);
        let turn = session.start_turn();
        let ctx =
            StageContext::builder(turn, session.transcript(), session.queue().clone()).build();
        assert_eq!(ctx.llm.model_name(), "null");
    }

    #[tokio::test]
    async fn test_end_stage_no_messages_stops() {
        // 队列为空 → should_continue = false
        let ctx = make_stage_context();
        let end_out = end::run_end(EndInput { context: ctx });
        assert!(!end_out.should_continue);
        assert!(end_out.awakened_messages.is_empty());
    }

    #[tokio::test]
    async fn test_end_stage_prompt_wakes() {
        // 队列有 Prompt → should_continue = true
        let ctx = make_stage_context();
        ctx.queue.push(QueuedMessage::prompt(
            MessageSource::UserInput,
            BaseMessage::human(MessageContent::text("new question")),
        ));
        let end_out = end::run_end(EndInput { context: ctx });
        assert!(end_out.should_continue);
        assert_eq!(end_out.awakened_messages.len(), 1);
    }

    #[tokio::test]
    async fn test_end_stage_defer_wakes() {
        // 队列有 Defer → should_continue = true
        let ctx = make_stage_context();
        ctx.queue.push(QueuedMessage::defer(
            MessageSource::SubAgentComplete,
            BaseMessage::human(MessageContent::text("deferred result")),
        ));
        let end_out = end::run_end(EndInput { context: ctx });
        assert!(end_out.should_continue);
    }

    #[tokio::test]
    async fn test_end_stage_info_does_not_wake() {
        // 队列仅有 Info → should_continue = false
        let ctx = make_stage_context();
        ctx.queue.push(QueuedMessage::info(
            MessageSource::SystemInjected,
            BaseMessage::human(MessageContent::text("info only")),
        ));
        let end_out = end::run_end(EndInput { context: ctx });
        assert!(!end_out.should_continue);
    }

    // ── e2e 集成测试（验证完整 v2 ReAct 循环）──

    /// MockLLM：首轮返回 final_answer，无 tool_calls
    struct FinalAnswerLLM {
        answer: &'static str,
    }
    #[async_trait::async_trait]
    impl ReactLLM for FinalAnswerLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn crate::tools::BaseTool],
            _streaming: Option<crate::llm::types::StreamingContext>,
        ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
            Ok(crate::agent::react::Reasoning::with_answer(
                "thinking",
                self.answer,
            ))
        }
        fn model_name(&self) -> String {
            "mock-final-answer".to_string()
        }
    }

    #[tokio::test]
    async fn test_e2e_final_answer_no_tools() {
        // e2e：推入 Prompt → run_react_loop → 直接 final_answer → Completed
        let cwd: Arc<str> = Arc::from("/tmp/e2e");
        let frozen = FrozenContext::builder().build();
        let session = Session::new(cwd, frozen, None);
        let turn = session.start_turn();
        let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
            .with_llm(Arc::new(FinalAnswerLLM {
                answer: "task completed",
            }))
            .build();

        // 推入用户输入
        ctx.queue.push(QueuedMessage::prompt(
            MessageSource::UserInput,
            BaseMessage::human(MessageContent::text("do the task")),
        ));

        let result = run_react_loop(ctx.clone(), 10).await;
        assert!(
            matches!(result, LoopResult::Completed),
            "expected Completed, got {:?}",
            result
        );

        // transcript 应包含：[user_prompt, ai_final_answer]
        let transcript = ctx.transcript.read();
        let visible: Vec<_> = transcript.visible_messages().into_iter().collect();
        assert_eq!(
            visible.len(),
            2,
            "expected 2 messages (user + ai), got {}",
            visible.len()
        );
        assert!(matches!(visible[0], BaseMessage::Human { .. }));
        assert!(matches!(visible[1], BaseMessage::Ai { .. }));
    }

    #[tokio::test]
    async fn test_e2e_cancel_before_loop() {
        // e2e：cancel_token 在 run_react_loop 之前触发 → Interrupted
        let cwd: Arc<str> = Arc::from("/tmp/e2e-cancel");
        let frozen = FrozenContext::builder().build();
        let session = Session::new(cwd, frozen, None);
        let turn = session.start_turn();
        let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
            .with_llm(Arc::new(FinalAnswerLLM {
                answer: "should not reach",
            }))
            .build();

        // 立即 cancel
        ctx.turn.cancel_token.cancel();

        let result = run_react_loop(ctx, 10).await;
        assert!(
            matches!(result, LoopResult::Interrupted),
            "expected Interrupted, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_e2e_empty_queue_completes_immediately() {
        // e2e：无 Prompt 推入 → End 阶段 should_continue=false → Completed
        // 注意：第一轮仍会调用 LLM（无 tool_calls 时进入 End）
        let cwd: Arc<str> = Arc::from("/tmp/e2e-empty");
        let frozen = FrozenContext::builder().build();
        let session = Session::new(cwd, frozen, None);
        let turn = session.start_turn();
        let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
            .with_llm(Arc::new(FinalAnswerLLM { answer: "answer" }))
            .build();

        // 不推入 Prompt，直接跑循环（首轮 Receive 阶段会消费空队列）
        let result = run_react_loop(ctx.clone(), 10).await;
        assert!(
            matches!(result, LoopResult::Completed),
            "expected Completed, got {:?}",
            result
        );

        // transcript 应只有 ai 回答（无 user prompt）
        let transcript = ctx.transcript.read();
        let visible: Vec<_> = transcript.visible_messages().into_iter().collect();
        assert!(
            visible.iter().any(|m| matches!(m, BaseMessage::Ai { .. })),
            "expected at least one AI message"
        );
    }
}
