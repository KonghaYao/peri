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
pub mod tool_dispatch;

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::AtomicU32;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::agent::compact_v2::config::CompactConfig;
use crate::agent::events::{Stage, StageStatus};
use crate::agent::events_v2::{EventBus, ObserveEvent};
use crate::agent::react::ReactLLM;
use crate::agent::token::ContextBudget;
use crate::error_suggest::{ErrorSuggestRegistry, ToolRegistrySnapshot};
use crate::group::pipeline::AgentId;
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
            },
            recall_buffer: rbuf,
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
            },
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

    pub fn build(self) -> StageContext {
        StageContext {
            session: self.session,
            runtime: self.runtime,
            compact: self.compact,
            async_ctx: self.async_ctx,
            recall_buffer: Arc::new(RwLock::new(Vec::new())),
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
                if msg.message.content().trim().is_empty() {
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
#[derive(Debug, Default)]
struct LoopState {
    /// 上一轮 Act 是否产出了 tool_calls
    has_tool_calls: bool,
    /// before_agent hooks 是否已执行（首次 Receive 后执行一次）
    before_agent_has_run: bool,
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

        // ── Receive（循环入口，也是退出判断点）──
        let receive_start = std::time::Instant::now();
        context
            .runtime
            .event_bus
            .emit_observe(ObserveEvent::StageStarted {
                turn_id: context.turn_id(),
                agent_id: context.session.agent_id,
                stage: Stage::Receive,
            });
        let receive_out = match receive::run_receive(ReceiveInput {
            context: context.clone(),
        })
        .await
        {
            Ok(out) => {
                context
                    .runtime
                    .event_bus
                    .emit_observe(ObserveEvent::StageEnded {
                        turn_id: context.turn_id(),
                        agent_id: context.session.agent_id,
                        stage: Stage::Receive,
                        status: StageStatus::Done,
                        duration_ms: receive_start.elapsed().as_millis() as u64,
                    });
                out
            }
            Err(e) => return LoopResult::Error(e),
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
                        _ = &mut cancel_fut => return LoopResult::Interrupted,
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

        // ── Compact ──
        let compact_start = std::time::Instant::now();
        context
            .runtime
            .event_bus
            .emit_observe(ObserveEvent::StageStarted {
                turn_id: context.turn_id(),
                agent_id: context.session.agent_id,
                stage: Stage::Compact,
            });
        let _compact_out = match compact::run_compact(CompactInput {
            context: context.clone(),
            has_tool_calls: loop_state.has_tool_calls,
        })
        .await
        {
            Ok(out) => {
                context
                    .runtime
                    .event_bus
                    .emit_observe(ObserveEvent::StageEnded {
                        turn_id: context.turn_id(),
                        agent_id: context.session.agent_id,
                        stage: Stage::Compact,
                        status: StageStatus::Done,
                        duration_ms: compact_start.elapsed().as_millis() as u64,
                    });
                out
            }
            Err(e) => return LoopResult::Error(e),
        };

        // ── Reason ──
        let reason_start = std::time::Instant::now();
        context
            .runtime
            .event_bus
            .emit_observe(ObserveEvent::StageStarted {
                turn_id: context.turn_id(),
                agent_id: context.session.agent_id,
                stage: Stage::Reason,
            });
        let reason_out = match reason::run_reason(ReasonInput {
            context: context.clone(),
            has_tool_calls: loop_state.has_tool_calls,
        })
        .await
        {
            Ok(out) => {
                context
                    .runtime
                    .event_bus
                    .emit_observe(ObserveEvent::StageEnded {
                        turn_id: context.turn_id(),
                        agent_id: context.session.agent_id,
                        stage: Stage::Reason,
                        status: StageStatus::Done,
                        duration_ms: reason_start.elapsed().as_millis() as u64,
                    });
                out
            }
            Err(e) => return LoopResult::Error(e),
        };

        // ── Act ──
        let act_start = std::time::Instant::now();
        context
            .runtime
            .event_bus
            .emit_observe(ObserveEvent::StageStarted {
                turn_id: context.turn_id(),
                agent_id: context.session.agent_id,
                stage: Stage::Act,
            });
        let act_out = match act::run_act(ActInput {
            context: context.clone(),
            reasoning: reason_out.reasoning,
        })
        .await
        {
            Ok(out) => {
                context
                    .runtime
                    .event_bus
                    .emit_observe(ObserveEvent::StageEnded {
                        turn_id: context.turn_id(),
                        agent_id: context.session.agent_id,
                        stage: Stage::Act,
                        status: StageStatus::Done,
                        duration_ms: act_start.elapsed().as_millis() as u64,
                    });
                out
            }
            Err(e) => return LoopResult::Error(e),
        };

        loop_state.has_tool_calls = act_out.has_tool_calls;
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
