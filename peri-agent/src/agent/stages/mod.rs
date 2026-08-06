//! ReAct v2 — 四阶段循环（RCRA）
//!
//! 每阶段有明确的类型契约（StageInput → StageOutput），可脱离完整 Agent 单独测试。
//! 阶段间依赖通过输入结构体声明，不读全局状态。
//!
//! 控制流：`Receive → Compact → Reason → Act → (回 Receive)`
//! Receive 是循环入口，也是退出判断点：队列空 + 无 idle 等待时退出。

pub mod act;
pub mod compact;
pub mod middleware_runner;
pub mod reason;
pub mod receive;
pub mod speculation_guard;
pub mod tool_dispatch;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;
use peri_acp_types::identity::AgentId;

use crate::agent::compact_v2::config::CompactConfig;
use crate::agent::events::{Stage, StageStatus};
use crate::agent::events_v2::{EventBus, ObserveEvent};
use crate::agent::react::ReactLLM;
use crate::agent::token::ContextBudget;
use crate::error_suggest::{ErrorSuggestRegistry, ToolRegistrySnapshot};
use crate::messages::BaseMessage;
use crate::middleware::chain::MiddlewareChain;
use crate::session::turn::TurnContext;
use crate::session::{MessageQueue, MessageTranscript, QueuedMessage};
use crate::tools::{BaseTool, DirectToolInvocationResolver, ToolInvocationResolver};

/// 共享工具注册表类型别名（避免 clippy::type_complexity）
pub type SharedToolMap = Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>;

// ─── 循环控制 ───────────────────────────────────────────────────────────────

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

// ─── 阶段间共享上下文子结构 ─────────────────────────────────────────────────

/// 会话级实体（生命周期 = 整个 Agent Session）
#[derive(Clone)]
pub struct SessionHandle {
    pub turn: Arc<TurnContext>,
    pub transcript: Arc<RwLock<MessageTranscript>>,
    pub queue: MessageQueue,
    pub agent_id: AgentId,
    /// metrics/tracing 用键值对（AgentContext 在 from_stage 时克隆）
    pub session_context: Arc<RwLock<HashMap<String, String>>>,
}

/// LLM 调用 + 工具执行运行时服务
#[derive(Clone)]
pub struct RuntimeServices {
    pub llm: Arc<dyn ReactLLM + Send + Sync>,
    /// LLM 可见 + 可执行的工具（Reason 读列表传 LLM，tool_dispatch 按名执行）
    pub tools: SharedToolMap,
    /// 每个 dispatch 使用其工具表 snapshot 的 canonical invocation resolver。
    pub tool_invocation_resolver: Arc<dyn ToolInvocationResolver>,
    pub middleware_chain: Arc<MiddlewareChain>,
    pub event_bus: Arc<EventBus>,
    /// Deferred tools 外部注册表（ExecuteExtraTool 代理执行用）
    pub shared_tools: Option<SharedToolMap>,
    pub error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
    pub tool_registry_snapshot: Arc<ToolRegistrySnapshot>,
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
}

/// 异步传输控制（仅 run_react_loop idle 路径）
#[derive(Clone)]
pub struct AsyncContext {
    pub idle_inbox: Option<Arc<crate::agent::session::SessionInbox>>,
    pub idle_should_wait: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    /// 会话级 idle-suspended 标志（宿主 SessionAccessPort 注入的共享 Arc）。
    ///
    /// run_react_loop 在 await_wake 挂起期间置 true、醒来/取消时复位。
    /// 宿主 `dispatch_prompt_turn` 读取此标志把挂起期间到达的用户 prompt
    /// 注入 inbox（Prompt + wake），让挂起的 loop 立即醒来消费，而不是在
    /// per-session prompt lock 上阻塞至当前 turn 完成。
    pub idle_suspended_flag: Option<Arc<AtomicBool>>,
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
    /// Recall 累加器（跨 middleware hook 共享）。
    ///
    /// 每次 middleware hook 都会构造临时 [`AgentContext`]，
    /// 调用结束后由 middleware_runner 把 AgentContext 内部
    /// recall_buffer drain 到本缓冲区，循环结束后由 executor 统一取出。
    pub recall_buffer: Arc<RwLock<Vec<String>>>,
    /// 推测深挖哨兵开关（SpeculationGuard）。默认 true（主 agent 生效）；
    /// SubAgent 由 session_context 的 session_id 信号排除——
    /// 见 `speculation_guard.rs` 模块注释（构建点均不在本 issue 修改范围）。
    pub ask_discipline: bool,
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
        let tool_snapshot = Arc::new(ToolRegistrySnapshot::default());
        Self {
            session: SessionHandle {
                turn: turn_arc,
                transcript,
                queue,
                agent_id: AgentId::new(),
                session_context: sctx,
            },
            runtime: RuntimeServices {
                llm: Arc::new(NullReactLLM),
                tools: tools_map,
                tool_invocation_resolver: Arc::new(DirectToolInvocationResolver),
                middleware_chain: mw_chain,
                event_bus: ebus,
                shared_tools: None,
                error_suggest_registry: None,
                tool_registry_snapshot: tool_snapshot,
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
            },
            async_ctx: AsyncContext {
                idle_inbox: None,
                idle_should_wait: None,
                idle_suspended_flag: None,
            },
            recall_buffer: rbuf,
            ask_discipline: true,
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
                agent_id: AgentId::new(),
                session_context: Arc::new(RwLock::new(std::collections::HashMap::new())),
            },
            runtime: RuntimeServices {
                llm: Arc::new(NullReactLLM),
                tools: Arc::new(RwLock::new(BTreeMap::new())),
                tool_invocation_resolver: Arc::new(DirectToolInvocationResolver),
                middleware_chain: Arc::new(MiddlewareChain::new()),
                event_bus: Arc::new(EventBus::new(Default::default()).0),
                shared_tools: None,
                error_suggest_registry: None,
                tool_registry_snapshot: Arc::new(ToolRegistrySnapshot::default()),
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
            },
            async_ctx: AsyncContext {
                idle_inbox: None,
                idle_should_wait: None,
                idle_suspended_flag: None,
            },
            ask_discipline: true,
        }
    }

    /// 便捷访问：当前 turn_id
    pub fn turn_id(&self) -> crate::session::turn::TurnId {
        self.session.turn.turn_id
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
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        Err(crate::error::AgentError::Interrupted)
    }

    fn model_name(&self) -> String {
        "null".to_string()
    }

    fn provider_capabilities(&self) -> crate::agent::compact_v2::projection::ProviderCapabilities {
        crate::agent::compact_v2::projection::ProviderCapabilities::default()
    }
}

// ─── StageContextBuilder ────────────────────────────────────────────────────

/// StageContext 构建器
///
/// 必填：turn / transcript / queue / llm（生产场景）
/// 可选：tools / middleware_chain / event_bus / budget / compact_config 等
pub struct StageContextBuilder {
    session: SessionHandle,
    runtime: RuntimeServices,
    compact: CompactContext,
    async_ctx: AsyncContext,
    /// 推测深挖哨兵开关（默认 true；SubAgent 由 session_id 信号排除）
    ask_discipline: bool,
}

impl StageContextBuilder {
    pub fn with_llm(mut self, llm: Arc<dyn ReactLLM + Send + Sync>) -> Self {
        self.runtime.llm = llm;
        self
    }

    pub fn with_tools(mut self, tools: SharedToolMap) -> Self {
        self.runtime.tools = tools;
        self
    }

    pub fn with_tool_invocation_resolver(
        mut self,
        resolver: Arc<dyn ToolInvocationResolver>,
    ) -> Self {
        self.runtime.tool_invocation_resolver = resolver;
        self
    }

    pub fn with_middleware_chain(mut self, chain: Arc<MiddlewareChain>) -> Self {
        self.runtime.middleware_chain = chain;
        self
    }

    pub fn with_event_bus(mut self, bus: Arc<EventBus>) -> Self {
        self.runtime.event_bus = bus;
        self
    }

    pub fn with_context_budget(mut self, budget: ContextBudget) -> Self {
        self.compact.context_budget = Some(budget);
        self
    }

    pub fn with_compact_config(mut self, config: CompactConfig) -> Self {
        self.compact.compact_config = Some(config);
        self
    }

    pub fn with_compact_llm(mut self, llm: Arc<dyn peri_model::Model>) -> Self {
        self.compact.compact_llm = Some(llm);
        self
    }

    pub fn with_shared_tools(mut self, shared: SharedToolMap) -> Self {
        self.runtime.shared_tools = Some(shared);
        self
    }

    pub fn with_error_suggest_registry(mut self, registry: Arc<ErrorSuggestRegistry>) -> Self {
        self.runtime.error_suggest_registry = Some(registry);
        self
    }

    pub fn with_tool_registry_snapshot(mut self, snapshot: ToolRegistrySnapshot) -> Self {
        self.runtime.tool_registry_snapshot = Arc::new(snapshot);
        self
    }

    pub fn with_agent_id(mut self, agent_id: AgentId) -> Self {
        self.session.agent_id = agent_id;
        self
    }

    /// 设置推测深挖哨兵（SpeculationGuard）开关。
    ///
    /// 默认 true；仅对主 agent 生效（SubAgent 由 session_context 的
    /// session_id 信号排除，见 speculation_guard.rs 模块注释）。
    /// 需要显式关闭的场景（如批量/CI 模式）传入 false。
    pub fn with_ask_discipline(mut self, enabled: bool) -> Self {
        self.ask_discipline = enabled;
        self
    }

    pub fn with_session_context(mut self, ctx: Arc<RwLock<HashMap<String, String>>>) -> Self {
        self.session.session_context = ctx;
        self
    }

    pub fn with_compact_pre_hook(mut self, hook: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.compact.compact_pre_hook = Some(hook);
        self
    }

    pub fn with_compact_post_hook(mut self, hook: Arc<dyn Fn(bool, usize) + Send + Sync>) -> Self {
        self.compact.compact_post_hook = Some(hook);
        self
    }

    pub fn with_idle_inbox(mut self, inbox: Arc<crate::agent::session::SessionInbox>) -> Self {
        self.async_ctx.idle_inbox = Some(inbox);
        self
    }

    /// 设置 idle 时是否应该 await_wake 的判断 closure。
    /// 返回 true → 主 agent 有未完成异步任务，需要 await_wake 等结果续跑。
    /// 返回 false → 直接退出 loop，避免正常对话 loading 卡死。
    pub fn with_idle_should_wait(mut self, probe: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        self.async_ctx.idle_should_wait = Some(probe);
        self
    }

    /// 设置会话级 idle-suspended 标志（await_wake 挂起期间置 true；宿主
    /// `dispatch_prompt_turn` 据此把挂起期间到达的用户 prompt 注入 inbox）。
    pub fn with_idle_suspended_flag(mut self, flag: Arc<AtomicBool>) -> Self {
        self.async_ctx.idle_suspended_flag = Some(flag);
        self
    }

    pub fn build(self) -> StageContext {
        StageContext {
            session: self.session,
            runtime: self.runtime,
            compact: self.compact,
            async_ctx: self.async_ctx,
            recall_buffer: Arc::new(RwLock::new(Vec::new())),
            ask_discipline: self.ask_discipline,
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
    /// LLM 请求使用的消息快照（用于调试/追踪；Arc 共享，避免传递时再拷贝）
    pub messages_snapshot: std::sync::Arc<Vec<BaseMessage>>,
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

// ─── 工具函数 ────────────────────────────────────────────────────────────────

/// 把 drained 队列消息写入 transcript。
///
/// - `Prompt`：message 原样 append（用户输入）
/// - `Defer` / `Info`：content 用 `<system-reminder>` 包裹后 append（系统注入）
///
/// Defer 与 Info 在 transcript 中的渲染一致（都是 system-injected 数据），
/// 差异仅在队列行为（drain 时机）——见 `MessageQueue::drain_all`。
pub fn append_messages_to_transcript(
    transcript: &mut MessageTranscript,
    messages: Vec<QueuedMessage>,
) {
    use crate::messages::{BaseMessage, MessageContent};
    use crate::session::MessageKind;
    for msg in messages {
        let content = match msg.kind {
            MessageKind::Prompt => {
                // keepgoing：空 Prompt 仅驱动 ReAct loop 继续（Receive consumed_count>0），
                // 不写入 transcript——用户没有输入新内容，历史中不应出现空 user 消息。
                // [判空] 必须与 peri-acp `is_keepgoing` 同一语义（按 content block 判空）：
                // `Blocks([Image])` 等纯附件消息不应被误判为空。
                if msg.message.message_content().is_empty() {
                    continue;
                }
                msg.message
            }
            MessageKind::Info | MessageKind::Defer => {
                let text = msg.message.content().to_string();
                BaseMessage::human(MessageContent::text(format!(
                    "<system-reminder>\n{}\n</system-reminder>",
                    text
                )))
            }
        };
        transcript.append(content);
    }
}

// ─── 控制流编排 ──────────────────────────────────────────────────────────────

/// 循环运行时状态（P1-2: 显式封装 has_tool_calls，替代游离的局部变量）。
///
/// 后续扩展方向（P1-1）：与 StageContext 的 LoopState 职责统一，
/// 将更多迭代级别状态（consecutive_failures 等）迁入此结构。
///
/// SpeculationGuard 字段生命周期 = turn（与"无用户输入连续深挖"语义匹配）：
/// 用户 Prompt 到达时由 `speculation_guard::reset` 清零。
#[derive(Debug, Default)]
struct LoopState {
    /// 上一轮 Act 是否产出了 tool_calls
    has_tool_calls: bool,
    /// before_agent hooks 是否已执行（首次 Receive 后执行一次）
    before_agent_has_run: bool,
    /// SpeculationGuard：自首个用户 Prompt 以来的连续无输入工具轮数
    speculation_rounds: u32,
    /// SpeculationGuard：最近 K=2 轮 thought 是否命中推测词（环形窗口）
    recent_speculation: VecDeque<bool>,
    /// SpeculationGuard：最近 M=2 轮工具结果是否含错误（环形窗口）
    recent_errors: VecDeque<bool>,
    /// SpeculationGuard：已注入的最高提醒等级（0=无 / 1=L1 / 2=L2）
    warned_level: u8,
    /// SpeculationGuard：本 turn 是否已调用 AskUserQuestion
    asked_user: bool,
}

/// 执行单个 ReAct 阶段：emit StageStarted → 调用阶段函数 → emit StageEnded → Ok/Err 分发。
///
/// Receive/Compact/Reason/Act 四阶段共享同一「事件观测 + 错误传播」样板，
/// 阶段函数通过闭包传入（需捕获 `context.clone()` 或上游输出）。
/// 返回 `Err(LoopResult)` 时调用方直接 `return e` 即可退出循环。
async fn run_stage<F, Fut, T>(context: &StageContext, stage: Stage, run: F) -> Result<T, LoopResult>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = crate::error::AgentResult<T>>,
{
    let start = std::time::Instant::now();
    context
        .runtime
        .event_bus
        .emit_observe(ObserveEvent::StageStarted {
            turn_id: context.turn_id(),
            agent_id: context.session.agent_id,
            stage,
        });
    let out = match run().await {
        Ok(out) => {
            context
                .runtime
                .event_bus
                .emit_observe(ObserveEvent::StageEnded {
                    turn_id: context.turn_id(),
                    agent_id: context.session.agent_id,
                    stage,
                    status: StageStatus::Done,
                    duration_ms: start.elapsed().as_millis() as u64,
                });
            out
        }
        Err(e) => {
            // S1.4：Err 路径也必须 emit StageEnded（status=Error），否则
            // StageStarted 无条件 emit 而 StageEnded 只在 Ok 分支 emit，
            // LLM 失败/cancel/工具错误等退出路径留下悬挂 Langfuse span。
            context
                .runtime
                .event_bus
                .emit_observe(ObserveEvent::StageEnded {
                    turn_id: context.turn_id(),
                    agent_id: context.session.agent_id,
                    stage,
                    status: StageStatus::Error,
                    duration_ms: start.elapsed().as_millis() as u64,
                });
            return Err(LoopResult::Error(e));
        }
    };
    Ok(out)
}

/// 运行 ReAct v2 四阶段循环（RCRA）
///
/// 控制流：Receive → Compact → Reason → Act → (回 Receive)。
/// Receive 是循环入口，也是退出判断点。
/// 返回循环最终结果（Completed / Interrupted / Error）。
pub async fn run_react_loop(context: StageContext, max_iterations: usize) -> LoopResult {
    let mut loop_state = LoopState::default();
    // await_wake 在主 agent idle 时启用，反复等待异步事件续跑（cron/bg/workflow）。
    // idle_should_wait probe 检 active_count>0，保证无挂起任务时不会永久阻塞。

    for _ in 0..max_iterations {
        // 检查 cancel
        if context.session.turn.is_cancelled() {
            return LoopResult::Interrupted;
        }

        // 推进 step
        context.session.turn.advance_step();

        // SpeculationGuard：本轮队列中是否存在用户 Prompt。
        // Receive 之后据此决定是否重置推测深挖计数（Info/Defer 系统注入不重置——
        // 否则哨兵自己的提醒消息会把计数清零，L2 永远无法升级）。
        let has_pending_prompt = context.session.queue.has_pending_prompt();

        // ── Receive（循环入口，也是退出判断点）──
        let receive_out = match run_stage(&context, Stage::Receive, || async {
            receive::run_receive(ReceiveInput {
                context: context.clone(),
            })
            .await
        })
        .await
        {
            Ok(out) => out,
            Err(e) => return e,
        };

        // 退出判断：队列为空且上一轮无工具调用 → 检查是否该退出。
        // 工具调用结果写入 transcript 而非队列：has_tool_calls=true 时
        // consumed_count=0 是正常状态——继续循环让 LLM 处理工具结果。
        if receive_out.consumed_count == 0 && !loop_state.has_tool_calls {
            // 竞态保护：退出前再检查一次队列是否有新消息到达
            if context.session.queue.has_wake_up() {
                tracing::debug!("Receive: consumed=0 but queue has wake-up, continue");
                continue;
            }

            // idle_should_wait 逻辑：队列空 → 如有 idle_inbox 且有未完成异步任务，等异步事件续跑。
            let should_wait = context
                .async_ctx
                .idle_should_wait
                .as_ref()
                .map(|probe| probe())
                .unwrap_or(false);
            if should_wait {
                if let Some(inbox) = &context.async_ctx.idle_inbox {
                    tracing::debug!("Receive: queue empty, awaiting wake (idle_should_wait=true)");
                    // 置 idle-suspended 标志：宿主 dispatch_prompt_turn 据此把
                    // 挂起期间到达的用户 prompt 注入 inbox（而非在 prompt lock
                    // 上阻塞至当前 turn 完成——bg 任务活跃时可能长达数分钟）。
                    if let Some(flag) = &context.async_ctx.idle_suspended_flag {
                        flag.store(true, Ordering::Release);
                    }
                    context.runtime.event_bus.emit_state(
                        crate::agent::events_v2::StateEvent::TurnSuspended {
                            turn_id: context.turn_id(),
                            agent_id: context.session.agent_id,
                        },
                    );
                    let cancel_fut = context.session.turn.cancel_token.cancelled();
                    tokio::pin!(cancel_fut);
                    tokio::select! {
                        _ = inbox.await_wake() => {
                            // 醒来：无论由注入 prompt 还是 bg Defer 触发，先复位
                            // 标志——后续 Receive 会 drain 队列并继续本 turn。
                            if let Some(flag) = &context.async_ctx.idle_suspended_flag {
                                flag.store(false, Ordering::Release);
                            }
                            if context.session.turn.is_cancelled() {
                                return LoopResult::Interrupted;
                            }
                            tracing::debug!(
                                turn_id = %context.session.turn.turn_id,
                                queue_len_after_wake = context.session.queue.len(),
                                "run_react_loop: idle inbox woken, continue to Receive"
                            );
                            // 醒来直接 continue 回 Receive——下一轮 Receive 用 drain_all()
                            // 统一处理所有消息（Prompt + Info + Defer），不再需要 post-wake drain_for_end
                            continue;
                        }
                        _ = &mut cancel_fut => {
                            if let Some(flag) = &context.async_ctx.idle_suspended_flag {
                                flag.store(false, Ordering::Release);
                            }
                            return LoopResult::Interrupted;
                        }
                    }
                }
            }
            tracing::debug!(
                idle_should_wait = should_wait,
                queue_len = context.session.queue.len(),
                "run_react_loop: exit (queue empty, no idle wait)"
            );
            return LoopResult::Completed;
        }

        // ── before_agent hooks（首次 Receive 后执行一次）──
        // RCRA 下 Receive 是唯一队列消费点，消息已通过 drain_all() 写入 transcript，
        // 此时 before_agent 钩子（SkillPreloadMiddleware / AtMentionMiddleware 等）
        // 可通过 state.messages() 读取用户输入。
        // 替代原来在 run_react_loop 外部的 Phase 6.7 调用。
        if !loop_state.before_agent_has_run {
            loop_state.before_agent_has_run = true;
            if let Err(e) = middleware_runner::run_before_agent(&context).await {
                tracing::warn!(error = %e, "[v2] before_agent hook failed");
            }
        }

        // SpeculationGuard：用户新输入到达 → 重置推测深挖计数
        // （has_pending_prompt=true ⟹ consumed_count>0，不会走到上方退出判断）
        if has_pending_prompt {
            speculation_guard::reset(&mut loop_state);
        }

        // ── Compact ──
        // Compact 输出（compacted 标志）当前无调用方：compact 的副作用已直接
        // 写入 transcript/flags 与事件流，此处仅保留阶段观测与错误传播。
        if let Err(e) = run_stage(&context, Stage::Compact, || async {
            compact::run_compact(CompactInput {
                context: context.clone(),
                has_tool_calls: loop_state.has_tool_calls,
            })
            .await
        })
        .await
        {
            return e;
        }

        // ── Reason ──
        let reason_out = match run_stage(&context, Stage::Reason, || async {
            reason::run_reason(ReasonInput {
                context: context.clone(),
                has_tool_calls: loop_state.has_tool_calls,
            })
            .await
        })
        .await
        {
            Ok(out) => out,
            Err(e) => return e,
        };

        // SpeculationGuard：reasoning 随后 move 进 Act，需在此处提前读取：
        // - thought 副本（推测词检测用）
        // - 本轮是否调用了 AskUserQuestion（D 条件）
        // - consecutive_failures 基线（Act 内部执行工具，对比判断本轮是否有错误）
        let speculation_thought = reason_out.reasoning.thought.clone();
        let asked_user_this_round = reason_out
            .reasoning
            .tool_calls
            .iter()
            .any(|tc| tc.name == "AskUserQuestion");
        let failures_before_act = context
            .compact
            .consecutive_failures
            .load(std::sync::atomic::Ordering::Relaxed);

        // ── Act ──
        let act_out = match run_stage(&context, Stage::Act, || async {
            act::run_act(ActInput {
                context: context.clone(),
                reasoning: reason_out.reasoning,
            })
            .await
        })
        .await
        {
            Ok(out) => out,
            Err(e) => return e,
        };

        loop_state.has_tool_calls = act_out.has_tool_calls;
        if asked_user_this_round {
            loop_state.asked_user = true;
        }

        // ── SpeculationGuard 哨兵（P1）──
        // B 条件：当前轮无用户输入（consumed_count==0）且产出了工具调用。
        // 其余条件（A 连续轮数 / C 推测词或错误窗口 / D 未问过用户）在
        // observe_tool_round 内部判定；满足时注入分级提醒（queue push Info，
        // 下轮 Receive 消费）。
        if receive_out.consumed_count == 0 && act_out.has_tool_calls {
            let failures_after_act = context
                .compact
                .consecutive_failures
                .load(std::sync::atomic::Ordering::Relaxed);
            speculation_guard::observe_tool_round(
                &context,
                &mut loop_state,
                &speculation_thought,
                failures_after_act > failures_before_act,
            );
        }
        // RCRA：无论 has_tool_calls 是 true 或 false，统一回 Receive 开始新一轮迭代
        continue;
    }

    // 达到最大迭代次数
    tracing::warn!(max_iterations, "ReAct v2 循环达到最大迭代次数");
    LoopResult::Error(crate::error::AgentError::MaxIterationsExceeded(
        max_iterations,
    ))
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "stages_test.rs"]
mod tests;
