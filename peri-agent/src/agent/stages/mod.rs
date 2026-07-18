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
use crate::llm::BaseModel;
use crate::messages::BaseMessage;
use crate::middleware::chain::MiddlewareChain;
use crate::session::turn::TurnContext;
use crate::session::{MessageQueue, MessageTranscript, QueuedMessage};
use crate::tools::BaseTool;

/// 共享工具注册表类型别名（避免 clippy::type_complexity）
pub type SharedToolMap = Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>;

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
    pub compact_llm: Option<Arc<dyn BaseModel>>,
    pub compact_pre_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    pub compact_post_hook: Option<Arc<dyn Fn(bool, usize) + Send + Sync>>,
    /// 会话级 Token 追踪器（Compact 写 reset/estimated_tokens，Act 读用于 StateSnapshot）
    pub token_tracker: Arc<RwLock<crate::agent::token::TokenTracker>>,
    /// 连续失败计数（tool_dispatch 递增/重置，Compact 读用于降级跳过，Act 读用于 StateSnapshot）
    pub consecutive_failures: Arc<AtomicU32>,
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
        let cfail = Arc::new(AtomicU32::new(0));
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
                consecutive_failures: cfail,
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

    pub fn with_compact_llm(mut self, llm: Arc<dyn BaseModel>) -> Self {
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

// ─── 工具函数 ────────────────────────────────────────────────────────────────

/// 把 drained 队列消息写入 transcript。
///
/// - `Prompt`：message 原样 append（用户输入）
/// - `Defer` / `Info`：content 用 `<system-reminder>` 包裹后 append（系统注入）
///
/// Defer 与 Info 在 transcript 中的渲染一致（都是 system-injected 数据），
/// 差异仅在队列行为（drain 时机）——见 `MessageQueue::drain_for_receive`
/// 与 `MessageQueue::drain_for_end`。
pub fn append_messages_to_transcript(
    transcript: &mut MessageTranscript,
    messages: Vec<QueuedMessage>,
) {
    use crate::messages::{BaseMessage, MessageContent};
    use crate::session::MessageKind;
    for msg in messages {
        let content = match msg.kind {
            MessageKind::Prompt => msg.message,
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
}

/// 运行 ReAct v2 五阶段循环
///
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

        // ── Receive ──
        let receive_start = std::time::Instant::now();
        context
            .runtime
            .event_bus
            .emit_observe(ObserveEvent::StageStarted {
                turn_id: context.turn_id(),
                agent_id: context.session.agent_id,
                stage: Stage::Receive,
            });
        let _receive_out = match receive::run_receive(ReceiveInput {
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

        // 有 tool_calls → 回 Compact（跳过 End）
        if loop_state.has_tool_calls {
            tracing::debug!(
                step = context.session.turn.current_step(),
                "tool_calls 存在，回到 Compact"
            );
            continue;
        }

        // ── End ──
        let end_start = std::time::Instant::now();
        context
            .runtime
            .event_bus
            .emit_observe(ObserveEvent::StageStarted {
                turn_id: context.turn_id(),
                agent_id: context.session.agent_id,
                stage: Stage::End,
            });
        let end_out = end::run_end(EndInput {
            context: context.clone(),
        });
        context
            .runtime
            .event_bus
            .emit_observe(ObserveEvent::StageEnded {
                turn_id: context.turn_id(),
                agent_id: context.session.agent_id,
                stage: Stage::End,
                status: StageStatus::Done,
                duration_ms: end_start.elapsed().as_millis() as u64,
            });

        tracing::debug!(
            step = context.session.turn.current_step(),
            should_continue = end_out.should_continue,
            awakened_count = end_out.awakened_messages.len(),
            queue_len_after = context.session.queue.len(),
            "End stage: should_continue decision"
        );

        if end_out.should_continue {
            // End 阶段 drain 出的 Prompt / Defer 必须写入 transcript——
            // drain_for_end 是 destructive，不写入会物理丢失。
            // Defer（bg_results / WorkflowComplete / cron）用 <system-reminder>
            // 包裹，符合 CLAUDE.md "中途纠正消息必须用 human + reminder" 约定。
            if !end_out.awakened_messages.is_empty() {
                let mut transcript = context.session.transcript.write();
                // 4. 发送合成 user message 事件——在 agent 消费 MQ Defer 消息时（而非
                //    在 executor registry event pump 中）发送，消除时序竞争窗口。
                //    此时前一轮 turn 的 TurnDone 已由 ACP 层归档到 committed，
                //    TUI bridge 收到事件后推入 committed 的顺序与 agent 内部状态严格一致。
                //    见 spec/issues/2026-07-08-mq-injected-user-message-not-in-tui.md
                for msg in &end_out.awakened_messages {
                    use crate::session::queue::MessageKind;
                    // 对所有 Defer-kind 消息（goal steering / cron / workflow / hook feedback）
                    // emit SyntheticUserMessage，让 TUI bridge 能刷新 committed 视图。
                    if msg.kind == MessageKind::Defer {
                        let raw_text = msg.message.content().to_string();
                        let text = format!("<system-reminder>\n{}\n</system-reminder>", raw_text);
                        context.runtime.event_bus.emit_state(
                            crate::agent::events_v2::StateEvent::SyntheticUserMessage {
                                turn_id: context.turn_id(),
                                agent_id: context.session.agent_id,
                                text,
                            },
                        );
                    }
                }
                append_messages_to_transcript(&mut transcript, end_out.awakened_messages);
            }
            loop_state.has_tool_calls = false;
            tracing::debug!("End: should_continue=true, loop continue new turn");
            continue;
        }

        // 队列空 → 如有 idle_inbox 且有未完成异步任务，等异步事件续跑。
        // 这条路径是 c9dbfb18 移除 run_session_loop 末尾 await_wake 后的替代方案：
        // 把 await_wake 下沉到 run_react_loop 内部，由 idle_inbox: Option 控制启用。
        // TUI 路径注入 Some → idle 等异步事件续跑（cron/bg/workflow）。
        // stdio/print 路径 None → 直接退出，保持 PromptResponse 响应性（避免 Zed 卡死）。
        //
        // 2026-07-11: 移除 woken_once 守卫——agent 在 idle_should_wait=true 时
        // 反复进入 await_wake，直到所有异步任务完成（idle_should_wait=false 时
        // 自然退出）。修复多 bg agent 同轮场景。
        let should_wait = context
            .async_ctx
            .idle_should_wait
            .as_ref()
            .map(|probe| probe())
            .unwrap_or(false);
        // 只有当 idle_should_wait closure 返回 true（主 agent 有未完成异步任务）
        // 才 await_wake。否则直接退出，避免正常对话 loading 卡死。
        if should_wait {
            if let Some(inbox) = &context.async_ctx.idle_inbox {
                tracing::debug!("End: queue empty, awaiting wake (idle_should_wait=true)");
                // 在 await_wake 阻塞之前 emit TurnSuspended：通知 TUI
                // flush current_turn + is_loading=false（停止 loading spinner）。
                // Agent 保持存活（await_wake 阻塞），bg callback 到达时
                // 新 turn 的 TextChunk/ToolStarted 自动恢复 loading。
                context.runtime.event_bus.emit_state(
                    crate::agent::events_v2::StateEvent::TurnSuspended {
                        turn_id: context.turn_id(),
                        agent_id: context.session.agent_id,
                    },
                );
                // select cancel：用户中断时立即退出，避免 await_wake 永久阻塞
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
                            "run_react_loop: idle inbox woken, continue new turn"
                        );
                        // 醒来后立即 drain_for_end 消费已 push 的 Defer/Prompt 写入 transcript，
                        // 让新一轮 Reason 阶段就能看到 bg/workflow 结果，避免 hallucination +
                        // 多余续跑（否则本轮 Receive 跳过 Defer，Reason 看不到，End 才写入触发又一轮）。
                        if let Some(msgs) = context.session.queue.drain_for_end() {
                            if !msgs.is_empty() {
                                // 4. 发送合成 user message 事件——与 End 阶段
                                //    should_continue 分支同模式：在 agent 消费
                                //    MQ Defer 消息时发送，消除时序竞争窗口。
                                for msg in &msgs {
                                    use crate::session::queue::MessageKind;
                                    // 对所有 Defer-kind 消息（goal steering / cron / workflow / hook feedback）
                                    // emit SyntheticUserMessage，让 TUI bridge 能刷新 committed 视图。
                                    if msg.kind == MessageKind::Defer {
                                        let raw_text = msg.message.content().to_string();
                                        let text = format!(
                                            "<system-reminder>\n{}\n</system-reminder>",
                                            raw_text
                                        );
                                        context.runtime.event_bus.emit_state(
                                            crate::agent::events_v2::StateEvent::SyntheticUserMessage {
                                                turn_id: context.turn_id(),
                                                agent_id: context.session.agent_id,
                                                text,
                                            },
                                        );
                                    }
                                }
                                let mut transcript = context.session.transcript.write();
                                append_messages_to_transcript(&mut transcript, msgs);
                                tracing::debug!(
                                    "post-wake drain_for_end wrote messages to transcript"
                                );
                            }
                        }
                        continue;
                    }
                    _ = &mut cancel_fut => return LoopResult::Interrupted,
                }
            }
        }
        tracing::debug!(
            idle_should_wait = should_wait,
            queue_len = context.session.queue.len(),
            "run_react_loop: exit (idle_should_wait=false or no idle_inbox)"
        );
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
#[path = "stages_test.rs"]
mod tests;
