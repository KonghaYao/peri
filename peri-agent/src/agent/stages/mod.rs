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
use std::sync::Arc;
use std::sync::atomic::AtomicU32;

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
    /// 会话级 TokenTracker（每次 LLM 调用后累积，compact/act 读取）.
    /// P0 #2 修复：从 AgentContext 自有默认值迁移到 StageContext 共享实例。
    pub token_tracker: Arc<parking_lot::RwLock<crate::agent::token::TokenTracker>>,
    /// 连续失败计数（工具失败检测，跨 step 共享）
    pub consecutive_failures: Arc<AtomicU32>,
    /// 会话上下文键值（session_id / run_id 等，metrics/tracing 用）
    pub session_context: Arc<RwLock<HashMap<String, String>>>,
    /// Recall 累加器（跨 middleware hook 共享）。
    ///
    /// 每次 middleware hook 都会构造临时 [`AgentContext`]，
    /// 调用结束后由 middleware_runner 把 AgentContext 内部
    /// recall_buffer drain 到本缓冲区，循环结束后由 executor 统一取出。
    pub recall_buffer: Arc<RwLock<Vec<String>>>,

    // ── Compact hook 回调（插件 PreCompact/PostCompact 触发）──
    /// Pre-compact 插件 hook 回调（可选）。由 ACP 层注入。
    pub compact_pre_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Post-compact 插件 hook 回调（可选）。由 ACP 层注入。
    /// 参数: (compacted, affected_count)
    pub compact_post_hook: Option<Arc<dyn Fn(bool, usize) + Send + Sync>>,

    // ── Transport-aware async wake ───────────────────────────────────────────
    /// Idle 时等待异步事件的 inbox（可选；ACP 层注入）。
    /// TUI 路径传 Some，stdio/print 路径传 None（保持 c9dbfb18 的 stdio 不卡死保证）。
    /// run_react_loop 在 queue 空时调 await_wake 阻塞，等 AsyncRouter 推送的 Defer 触发 wake。
    pub idle_inbox: Option<Arc<crate::agent::session::SessionInbox>>,
    /// Idle 时是否应该 await_wake 的判断 closure（可选）。
    /// 返回 true → 主 agent 有未完成的异步任务（bg subagent），需要 await_wake 等结果。
    /// 返回 false 或 None → 直接退出 loop，避免正常对话 loading 卡死。
    /// peri-acp 注入：检查 background_registry.active_count() > 0。
    pub idle_should_wait: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
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
            token_tracker: Arc::new(parking_lot::RwLock::new(
                crate::agent::token::TokenTracker::default(),
            )),
            consecutive_failures: Arc::new(AtomicU32::new(0)),
            session_context: Arc::new(RwLock::new(HashMap::new())),
            recall_buffer: Arc::new(RwLock::new(Vec::new())),
            compact_pre_hook: None,
            compact_post_hook: None,
            idle_inbox: None,
            idle_should_wait: None,
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
    idle_inbox: Option<Arc<crate::agent::session::SessionInbox>>,
    idle_should_wait: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
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

    pub fn with_idle_inbox(mut self, inbox: Arc<crate::agent::session::SessionInbox>) -> Self {
        self.inner.idle_inbox = Some(inbox);
        self
    }

    /// 设置 idle 时是否应该 await_wake 的判断 closure。
    /// 返回 true → 主 agent 有未完成异步任务，需要 await_wake 等结果续跑。
    /// 返回 false → 直接退出 loop，避免正常对话 loading 卡死。
    pub fn with_idle_should_wait(mut self, probe: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        self.inner.idle_should_wait = Some(probe);
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
            token_tracker: Arc::new(parking_lot::RwLock::new(
                crate::agent::token::TokenTracker::default(),
            )),
            consecutive_failures: Arc::new(AtomicU32::new(0)),
            session_context: self
                .inner
                .session_context
                .unwrap_or_else(|| Arc::new(RwLock::new(HashMap::new()))),
            recall_buffer: Arc::new(RwLock::new(Vec::new())),
            compact_pre_hook: self.inner.compact_pre_hook,
            compact_post_hook: self.inner.compact_post_hook,
            idle_inbox: self.inner.idle_inbox,
            idle_should_wait: self.inner.idle_should_wait,
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

/// 运行 ReAct v2 五阶段循环
///
/// 返回循环最终结果（Completed / Interrupted / Error）。
pub async fn run_react_loop(context: StageContext, max_iterations: usize) -> LoopResult {
    let mut has_tool_calls = false;
    // await_wake 只在主 agent 首次 idle 时启用一次。
    // 被 wake 唤醒续跑一轮后，本轮 End queue 空时直接退出，避免 await_wake 永久阻塞
    // 导致 TUI loading 卡死。后续异步事件（cron/workflow）由 TUI 发新 prompt 重启
    // run_session_loop 处理（与 stdio 路径一致）。
    let mut woken_once = false;

    tracing::info!(
        turn_id = %context.turn.turn_id,
        queue_len = context.queue.len(),
        has_idle_inbox = context.idle_inbox.is_some(),
        "[bg-diag] run_react_loop: ENTER"
    );

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

        tracing::info!(
            step = context.turn.current_step(),
            should_continue = end_out.should_continue,
            awakened_count = end_out.awakened_messages.len(),
            queue_len_after = context.queue.len(),
            "[bg-diag] End stage: should_continue decision"
        );

        if end_out.should_continue {
            // End 阶段 drain 出的 Prompt / Defer 必须写入 transcript——
            // drain_for_end 是 destructive，不写入会物理丢失。
            // Defer（bg_results / WorkflowComplete / cron）用 <system-reminder>
            // 包裹，符合 CLAUDE.md "中途纠正消息必须用 human + reminder" 约定。
            if !end_out.awakened_messages.is_empty() {
                let mut transcript = context.transcript.write();
                // 4. 发送合成 user message 事件——在 agent 消费 MQ Defer 消息时（而非
                //    在 executor registry event pump 中）发送，消除时序竞争窗口。
                //    此时前一轮 turn 的 TurnDone 已由 ACP 层归档到 committed，
                //    TUI bridge 收到事件后推入 committed 的顺序与 agent 内部状态严格一致。
                //    见 spec/issues/2026-07-08-mq-injected-user-message-not-in-tui.md
                for msg in &end_out.awakened_messages {
                    use crate::session::queue::MessageSource;
                    if msg.source == MessageSource::SubAgentComplete {
                        let text = msg.message.content().to_string();
                        context.event_bus.emit_state(
                            crate::agent::events_v2::StateEvent::SyntheticUserMessage {
                                turn_id: context.turn_id(),
                                agent_id: context.agent_id,
                                text,
                            },
                        );
                    }
                }
                append_messages_to_transcript(&mut transcript, end_out.awakened_messages);
            }
            has_tool_calls = false;
            tracing::info!("[bg-diag] End: should_continue=true, loop continue new turn");
            continue;
        }

        // 队列空 → 如有 idle_inbox 且未被 wake 过，等异步事件续跑一次。
        // 这条路径是 c9dbfb18 移除 run_session_loop 末尾 await_wake 后的替代方案：
        // 把 await_wake 下沉到 run_react_loop 内部，由 idle_inbox: Option 控制启用。
        // TUI 路径注入 Some → idle 等异步事件续跑（cron/bg/workflow）一次。
        // stdio/print 路径 None → 直接退出，保持 PromptResponse 响应性（避免 Zed 卡死）。
        //
        // woken_once 守卫：被 wake 唤醒续跑一轮后，本轮 End queue 空时直接退出。
        // 否则 await_wake 永久阻塞 → TUI loading 卡死。后续异步事件由 TUI 发新 prompt
        // 重启 run_session_loop 处理。
        if !woken_once {
            // 只有当 idle_should_wait closure 返回 true（主 agent 有未完成异步任务）
            // 才 await_wake。否则直接退出，避免正常对话 loading 卡死。
            let should_wait = context
                .idle_should_wait
                .as_ref()
                .map(|probe| probe())
                .unwrap_or(false);
            if should_wait {
                if let Some(inbox) = &context.idle_inbox {
                    tracing::info!(
                        "[bg-diag] End: queue empty, awaiting wake (idle_should_wait=true, first-time)"
                    );
                    // 在 await_wake 阻塞之前 emit TurnSuspended：通知 TUI
                    // flush current_turn + is_loading=false（停止 loading spinner）。
                    // Agent 保持存活（await_wake 阻塞），bg callback 到达时
                    // 新 turn 的 TextChunk/ToolStarted 自动恢复 loading。
                    context.event_bus.emit_state(
                        crate::agent::events_v2::StateEvent::TurnSuspended {
                            turn_id: context.turn_id(),
                            agent_id: context.agent_id,
                        },
                    );
                    // select cancel：用户中断时立即退出，避免 await_wake 永久阻塞
                    let cancel_fut = context.turn.cancel_token.cancelled();
                    tokio::pin!(cancel_fut);
                    tokio::select! {
                        _ = inbox.await_wake() => {
                            if context.turn.is_cancelled() {
                                return LoopResult::Interrupted;
                            }
                            woken_once = true;
                            tracing::info!(
                                turn_id = %context.turn.turn_id,
                                queue_len_after_wake = context.queue.len(),
                                "[bg-diag] run_react_loop: idle inbox woken, continue new turn (no more await_wake)"
                            );
                            // 醒来后立即 drain_for_end 消费已 push 的 Defer/Prompt 写入 transcript，
                            // 让新一轮 Reason 阶段就能看到 bg/workflow 结果，避免 hallucination +
                            // 多余续跑（否则本轮 Receive 跳过 Defer，Reason 看不到，End 才写入触发又一轮）。
                            if let Some(msgs) = context.queue.drain_for_end() {
                                if !msgs.is_empty() {
                                    // 4. 发送合成 user message 事件——与 End 阶段
                                    //    should_continue 分支同模式：在 agent 消费
                                    //    MQ Defer 消息时发送，消除时序竞争窗口。
                                    for msg in &msgs {
                                        use crate::session::queue::MessageSource;
                                        if msg.source == MessageSource::SubAgentComplete {
                                            let text = msg.message.content().to_string();
                                            context.event_bus.emit_state(
                                                crate::agent::events_v2::StateEvent::SyntheticUserMessage {
                                                    turn_id: context.turn_id(),
                                                    agent_id: context.agent_id,
                                                    text,
                                                },
                                            );
                                        }
                                    }
                                    let mut transcript = context.transcript.write();
                                    append_messages_to_transcript(&mut transcript, msgs);
                                    tracing::info!(
                                        "[bg-diag] post-wake drain_for_end wrote messages to transcript"
                                    );
                                }
                            }
                            continue;
                        }
                        _ = &mut cancel_fut => return LoopResult::Interrupted,
                    }
                }
            }
        }
        tracing::info!(
            woken_once = woken_once,
            "[bg-diag] run_react_loop: exit (woken_once or idle_should_wait=false)"
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
mod tests {
    use super::*;
    use crate::messages::MessageContent;
    use crate::session::Session;
    use crate::session::queue::MessageSource;
    use crate::session::store::FrozenContext;

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

    // ── append_messages_to_transcript helper 测试 ──

    #[test]
    fn test_append_messages_prompt_kept_as_is() {
        // Prompt 消息应原样 append（用户输入不包裹 reminder）
        let ctx = make_stage_context();
        let msgs = vec![QueuedMessage::prompt(
            MessageSource::UserInput,
            BaseMessage::human(MessageContent::text("hello user")),
        )];
        {
            let mut transcript = ctx.transcript.write();
            append_messages_to_transcript(&mut transcript, msgs);
        }
        let transcript = ctx.transcript.read();
        assert_eq!(transcript.len(), 1);
        let content = transcript.entries()[0].message.content();
        assert_eq!(content, "hello user");
    }

    #[test]
    fn test_append_messages_info_wrapped_in_reminder() {
        // Info 消息应用 <system-reminder> 包裹
        let ctx = make_stage_context();
        let msgs = vec![QueuedMessage::info(
            MessageSource::SystemInjected,
            BaseMessage::human(MessageContent::text("system info")),
        )];
        {
            let mut transcript = ctx.transcript.write();
            append_messages_to_transcript(&mut transcript, msgs);
        }
        let transcript = ctx.transcript.read();
        assert_eq!(transcript.len(), 1);
        let content = transcript.entries()[0].message.content();
        assert!(content.contains("<system-reminder>"));
        assert!(content.contains("system info"));
    }

    #[test]
    fn test_append_messages_defer_wrapped_in_reminder() {
        // Defer 消息（bg_results / WorkflowComplete）应用 <system-reminder> 包裹
        // —— 这是本次修复的关键断言：mod.rs:520-528 把 awakened_messages 写入
        // transcript 时，Defer 走 reminder 包裹路径（与 Info 一致）。
        let ctx = make_stage_context();
        let msgs = vec![QueuedMessage::defer(
            MessageSource::SubAgentComplete,
            BaseMessage::human(MessageContent::text("bg-result-payload")),
        )];
        {
            let mut transcript = ctx.transcript.write();
            append_messages_to_transcript(&mut transcript, msgs);
        }
        let transcript = ctx.transcript.read();
        assert_eq!(transcript.len(), 1);
        let content = transcript.entries()[0].message.content();
        assert!(
            content.contains("<system-reminder>"),
            "Defer 应被 reminder 包裹, got: {}",
            content
        );
        assert!(
            content.contains("bg-result-payload"),
            "Defer 内容应在 transcript 中, got: {}",
            content
        );
    }

    #[tokio::test]
    async fn test_e2e_defer_written_to_transcript_when_end_awakens() {
        // e2e：push Defer → run_react_loop → 第一轮 End drain Defer →
        // mod.rs:520-528 把 awakened_messages 写入 transcript（reminder 包裹）→
        // 第二轮循环正常退出。
        //
        // 回归保护：修复前 awakened_messages 被 drop，Defer 内容物理丢失。
        let cwd: Arc<str> = Arc::from("/tmp/e2e-defer");
        let frozen = FrozenContext::builder().build();
        let session = Session::new(cwd, frozen, None);
        let turn = session.start_turn();
        let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
            .with_llm(Arc::new(FinalAnswerLLM { answer: "ok" }))
            .build();

        ctx.queue.push(QueuedMessage::defer(
            MessageSource::SubAgentComplete,
            BaseMessage::human(MessageContent::text("bg-result-payload")),
        ));

        let result = run_react_loop(ctx.clone(), 5).await;
        assert!(
            matches!(result, LoopResult::Completed),
            "expected Completed, got {:?}",
            result
        );

        // transcript 应包含 Defer 内容（reminder 包裹）
        let transcript = ctx.transcript.read();
        let combined: String = transcript
            .visible_messages()
            .iter()
            .map(|m| m.content().to_string())
            .collect::<Vec<_>>()
            .join("\n---\n");
        assert!(
            combined.contains("<system-reminder>"),
            "Defer 应被 reminder 包裹写入 transcript, got: {}",
            combined
        );
        assert!(
            combined.contains("bg-result-payload"),
            "Defer 内容应在 transcript 中, got: {}",
            combined
        );
    }
}
