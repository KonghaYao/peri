//! ReAct v2 — 四阶段循环（RCRA）
//!
//! 每阶段有明确的类型契约（StageInput → StageOutput），可脱离完整 Agent 单独测试。
//! 阶段间依赖通过输入结构体声明，不读全局状态。
//!
//! 控制流：`Receive → Compact → Reason → Act → (回 Receive)`
//! Receive 是循环入口，也是退出判断点：队列空 + 无 idle 等待时退出。

pub mod act;
mod async_context;
pub mod compact;
mod compact_progress;
mod execution_control;
pub use execution_control::run_react_loop;
pub mod middleware_runner;
mod null_llm;
mod queue_to_transcript;
pub mod reason;
pub mod receive;
pub mod tool_dispatch;
mod work_boundary;
pub use work_boundary::{SdkAdmissionObservedFn, SdkRunStartedFn};
mod context_builder;
mod work_dispatch;
pub(crate) mod work_ledger;
mod work_pipeline;
#[cfg(test)]
mod work_production_test;
mod work_reason;
mod work_receive;
mod work_recovery;
#[cfg(test)]
mod work_test_support;
pub use null_llm::NullReactLLM;
pub use work_receive::publish_session_inbox;

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;
use peri_acp_types::identity::AgentId;

use crate::agent::compact_v2::config::CompactConfig;
use crate::agent::events::{Stage, StageStatus};
use crate::agent::events_v2::{EventBus, ObserveEvent};
use crate::agent::react::ReactLLM;
use crate::agent::token::ContextBudget;
use crate::messages::BaseMessage;
use crate::middleware::chain::MiddlewareChain;
use crate::session::tool_catalog::{SessionToolCatalog, SessionToolCatalogSnapshot};
use crate::session::turn::TurnContext;
use crate::session::{MessageQueue, MessageTranscript, QueuedMessage};
use crate::tools::{BaseTool, DirectToolInvocationResolver, ToolInvocationResolver};

pub use async_context::AsyncContext;

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
    pub user_input_mailbox: Option<Arc<crate::session::user_input_mailbox::UserInputMailbox>>,
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
    pub(crate) sdk_run_started: Option<work_boundary::SdkRunStartedFn>,
    pub(crate) sdk_admission_observed: Option<work_boundary::SdkAdmissionObservedFn>,
    pub recipient_lifecycle: Option<u64>,
    pub(crate) mcp_work_binding: Option<(
        Arc<dyn peri_acp_types::ports::McpPoolPort>,
        Arc<dyn peri_acp_types::tasks::TaskManager>,
    )>,
    pub(crate) work: Arc<work_boundary::WorkBoundary>,
    pub execution_admission_port:
        Option<Arc<dyn peri_acp_types::execution_admission::ExecutionAdmissionPort>>,
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
}

impl StageContext {
    /// 兼容旧测试：仅传会话实体时构造 minimal context（运行时字段需要单独填充）
    ///
    /// **注意**：此构造函数仅用于单元测试。生产代码请用 `StageContextBuilder`。
    #[cfg(test)]
    pub fn new_best_effort_fixture(
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
            work: Arc::new(work_boundary::WorkBoundary::fixture()),
            sdk_run_started: None,
            sdk_admission_observed: None,
            recipient_lifecycle: None,
            mcp_work_binding: None,
            execution_admission_port: None,
            session: SessionHandle {
                turn: turn_arc,
                transcript,
                queue,
                user_input_mailbox: None,
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
                budget_recovery: Arc::new(parking_lot::Mutex::new(Default::default())),
            },
            async_ctx: AsyncContext {
                idle_wait_enabled: false,
                idle_should_wait: None,
                pending_handoff: None,
                handoff_deadline: None,
                idle_registry: None,
                idle_suspended_flag: None,
            },
            goal_controller: None,
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
            work: Arc::new(work_boundary::WorkBoundary::default()),
            sdk_run_started: None,
            sdk_admission_observed: None,
            recipient_lifecycle: None,
            mcp_work_binding: None,
            execution_admission_port: None,
            session: SessionHandle {
                turn: Arc::new(turn),
                transcript,
                queue,
                user_input_mailbox: None,
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
                budget_recovery: Arc::new(parking_lot::Mutex::new(Default::default())),
            },
            async_ctx: AsyncContext {
                idle_wait_enabled: false,
                idle_should_wait: None,
                pending_handoff: None,
                handoff_deadline: None,
                idle_registry: None,
                idle_suspended_flag: None,
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
    sdk_run_started: Option<work_boundary::SdkRunStartedFn>,
    sdk_admission_observed: Option<work_boundary::SdkAdmissionObservedFn>,
    recipient_lifecycle: Option<u64>,
    mcp_work_binding: Option<(
        Arc<dyn peri_acp_types::ports::McpPoolPort>,
        Arc<dyn peri_acp_types::tasks::TaskManager>,
    )>,
    work: Arc<work_boundary::WorkBoundary>,
    execution_admission_port:
        Option<Arc<dyn peri_acp_types::execution_admission::ExecutionAdmissionPort>>,
    session: SessionHandle,
    runtime: RuntimeServices,
    compact: CompactContext,
    async_ctx: AsyncContext,
    goal_controller: Option<Arc<dyn peri_acp_types::goal::GoalController>>,
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
    /// 本轮消费的消息数量（含不唤醒循环的 Info）
    pub consumed_count: usize,
    /// 本轮消费的可驱动语义续跑消息数量（Prompt / Defer）
    pub wake_up_count: usize,
    /// 本次消费的非空用户 Human 消息身份，供首次输入准备精准处理。
    pub input_message_ids: Vec<crate::messages::MessageId>,
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
    /// Immutable tool catalog used by this model request and its following Act.
    pub catalog: Arc<SessionToolCatalogSnapshot>,
    /// LLM 请求使用的消息快照（用于调试/追踪；Arc 共享，避免传递时再拷贝）
    pub messages_snapshot: std::sync::Arc<Vec<BaseMessage>>,
}

// ─── Act 阶段类型 ────────────────────────────────────────────────────────────

/// Act 阶段输入
pub struct ActInput {
    pub context: StageContext,
    /// Reason 阶段的推理结果
    pub reasoning: crate::agent::react::Reasoning,
    /// Exact catalog pinned by Reason.
    pub catalog: Arc<SessionToolCatalogSnapshot>,
}

/// Act 阶段输出
pub struct ActOutput {
    /// 是否有工具调用
    pub has_tool_calls: bool,
    /// 最终回答文本（无 tool_calls 时）
    pub final_answer: Option<String>,
}

// ─── 工具函数 ────────────────────────────────────────────────────────────────

pub use queue_to_transcript::append_messages_to_transcript;

// ─── 控制流编排 ──────────────────────────────────────────────────────────────

/// 循环运行时状态（P1-2: 显式封装 has_tool_calls，替代游离的局部变量）。
#[derive(Debug, Default)]
struct LoopState {
    /// 上一轮 Act 是否产出了 tool_calls
    has_tool_calls: bool,
    /// before_agent hooks 是否已执行（首次 Receive 后执行一次）
    before_agent_has_run: bool,
    /// 启动闸门 hook 是否已通过（首批 before_agent 后执行一次，成功才置位）
    react_start_has_run: bool,
    /// 仅无完整工具调用的截断消耗恢复预算；完整工具结果可继续正常循环。
    consecutive_truncations: usize,
    /// 本轮累计中断次数，不因工具进展重置。
    stream_recoveries: usize,
}

fn enqueue_stream_interruption_continuation(context: &StageContext) {
    use crate::session::queue::{MessageKind, MessageSource};
    use peri_acp_types::system_reminder::{ReminderCategory, ReminderDelivery, ReminderSeverity};

    let reminder = crate::session::producer_reminders::trusted_reminder(
        ReminderCategory::Guidance,
        "model_runtime",
        "stream_interrupted",
        ReminderSeverity::Warning,
        ReminderDelivery::Required,
        "上一条回复在流式传输中被上游切断，并非你主动结束。从保存的断点继续，不要重复已输出内容或已完成的工具调用；参数被截断的工具调用需要重新发起。".into(),
        Some("模型流中断，正在继续。".into()),
        serde_json::json!({"reason": "stream_interrupted"}),
    );
    context.session.queue.push(
        QueuedMessage::system_reminder(MessageKind::Defer, MessageSource::SystemInjected, reminder)
            .with_policy(
                peri_acp_types::session::MessagePolicy::continue_current_run(
                    context.session.turn.execution_binding(),
                ),
            ),
    );
}

fn enqueue_truncation_continuation(context: &StageContext) {
    use crate::session::queue::{MessageKind, MessageSource};
    use peri_acp_types::system_reminder::{ReminderCategory, ReminderDelivery, ReminderSeverity};

    let reminder = crate::session::producer_reminders::trusted_reminder(
        ReminderCategory::Guidance,
        "model_runtime",
        "output_truncated",
        ReminderSeverity::Warning,
        ReminderDelivery::Required,
        "Your previous response reached the output token limit before completing the task. Continue from the saved state with a shorter response. Break large changes into smaller tool calls. Do not repeat tool calls that already completed.".into(),
        Some("Model response was truncated; continuing.".into()),
        serde_json::json!({"stop_reason": "max_tokens"}),
    );
    context.session.queue.push(
        QueuedMessage::system_reminder(MessageKind::Defer, MessageSource::SystemInjected, reminder)
            .with_policy(
                peri_acp_types::session::MessagePolicy::continue_current_run(
                    context.session.turn.execution_binding(),
                ),
            ),
    );
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
    let start = peri_time::monotonic_now();
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
            return Err(if matches!(&e, crate::error::AgentError::Interrupted) {
                LoopResult::Interrupted
            } else {
                LoopResult::Error(e)
            });
        }
    };
    Ok(out)
}

/// 运行 ReAct v2 四阶段循环（RCRA）
///
/// 控制流：Receive → Compact → Reason → Act → (回 Receive)。
/// Receive 是循环入口，也是退出判断点。
/// 返回循环最终结果（Completed / Interrupted / Error）。
async fn run_react_loop_inner(context: StageContext, max_iterations: usize) -> LoopResult {
    let mut loop_state = LoopState::default();
    let mut semantic_iterations = 0usize;
    let execution = context.session.turn.execution_binding();
    // Keep one receiver for the lifetime of this loop so its observed version
    // advances across idle retries. StageContext clones are short-lived stage
    // inputs and must not own the receive cursor.
    let mut idle_registry = context.async_ctx.idle_registry.clone();
    const MAX_TRUNCATION_CONTINUATIONS: usize = 2;

    'receive_retry: loop {
        'rcra: loop {
            // 检查 cancel
            if context.session.turn.is_cancelled() {
                return LoopResult::Interrupted;
            }

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

            if receive_out.wake_up_count == 0 {
                match context.work.ensure(&context).await {
                    Ok(Some(_)) => break 'rcra,
                    Ok(None) => {}
                    Err(error) => return LoopResult::Error(error.into()),
                }
            }

            // 退出判断：本轮没有可唤醒消息且上一轮无工具调用 → 检查是否该退出。
            // Info 只做状态维护，虽被 Receive 消费，但不能单独驱动 Compact → Reason → Act。
            // 工具调用结果写入 transcript 而非队列：has_tool_calls=true 时
            // wake_up_count=0 是正常状态——继续循环让 LLM 处理工具结果。
            if receive_out.wake_up_count == 0 && !loop_state.has_tool_calls {
                // 竞态保护：退出前再检查一次队列是否有新消息到达
                if context.session.queue.has_required_for_run(&execution) {
                    tracing::debug!("Receive: consumed=0 but queue has wake-up, continue");
                    continue;
                }

                // A lifecycle notification may have raced with the first
                // active-count probe. Consume it before deciding to exit so a
                // register/complete transition is re-evaluated at Receive.
                if let Some(receiver) = idle_registry.as_mut() {
                    if receiver.has_changed().unwrap_or(false) {
                        receiver.borrow_and_update();
                        continue;
                    }
                }

                let should_wait = context
                    .async_ctx
                    .idle_should_wait
                    .as_ref()
                    .map(|probe| probe())
                    .unwrap_or(false);
                if should_wait && context.async_ctx.idle_wait_enabled {
                    if let Some(mailbox) = &context.session.user_input_mailbox {
                        mailbox.enter_idle();
                    }
                    // loading 期间的队首输入在 idle 边界交接，直接回 Receive，
                    // 不先发布一个并未真正等待的 TurnSuspended。
                    if context.session.queue.has_required_for_run(&execution) {
                        if let Some(mailbox) = &context.session.user_input_mailbox {
                            mailbox.leave_idle();
                        }
                        continue;
                    }
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
                    // 有界等待：界本身必须能唤醒挂起的 loop，否则只有偶发
                    // 唤醒（任务注册/终态/取消）才会重新求值，界形同虚设。
                    // 到点走 continue → Receive 退出求值 → 写交接。
                    let handoff_deadline = context
                        .async_ctx
                        .handoff_deadline
                        .as_ref()
                        .and_then(|probe| probe());
                    let deadline_tick = async move {
                        match handoff_deadline {
                            Some(deadline) => tokio::time::sleep_until(deadline).await,
                            None => std::future::pending::<()>().await,
                        }
                    };
                    tokio::pin!(deadline_tick);
                    let registry_wait = async {
                        if let Some(receiver) = idle_registry.as_mut() {
                            if receiver.changed().await.is_err() {
                                // The TaskManager is normally retained by
                                // idle_should_wait. If it is dropped, do
                                // not turn a closed watch channel into a
                                // busy loop.
                                std::future::pending::<()>().await;
                            }
                        } else {
                            std::future::pending::<()>().await;
                        }
                    };
                    tokio::pin!(registry_wait);
                    tokio::select! {
                        biased;
                        _ = &mut cancel_fut => {
                            // 醒来：无论由注入 prompt 还是 bg Defer 触发，先复位
                            // 标志——后续 Receive 会 drain 队列并继续本 turn。
                            if let Some(flag) = &context.async_ctx.idle_suspended_flag {
                                flag.store(false, Ordering::Release);
                            }
                            if let Some(mailbox) = &context.session.user_input_mailbox {
                                mailbox.leave_idle();
                            }
                            return LoopResult::Interrupted;
                        }
                        _ = context.session.queue.await_wake_for_run(&execution) => {
                            // 醒来：无论由注入 prompt 还是 bg Defer 触发，先复位
                            // 标志——后续 Receive 会 drain 队列并继续本 turn。
                            if let Some(flag) = &context.async_ctx.idle_suspended_flag {
                                flag.store(false, Ordering::Release);
                            }
                            if let Some(mailbox) = &context.session.user_input_mailbox {
                                mailbox.leave_idle();
                            }
                            if context.session.turn.is_cancelled() {
                                return LoopResult::Interrupted;
                            }
                            tracing::debug!(
                                turn_id = %context.session.turn.turn_id(),
                                queue_len_after_wake = context.session.queue.len(),
                                "run_react_loop: idle inbox woken, continue to Receive"
                            );
                            // 醒来直接 continue 回 Receive——下一轮 Receive 用 drain_all()
                            // 统一处理所有消息（Prompt + Info + Defer），不再需要 post-wake drain_for_end
                            continue;
                        }
                        _ = &mut deadline_tick => {
                            if let Some(flag) = &context.async_ctx.idle_suspended_flag {
                                flag.store(false, Ordering::Release);
                            }
                            if let Some(mailbox) = &context.session.user_input_mailbox {
                                mailbox.leave_idle();
                            }
                            tracing::debug!(
                                turn_id = %context.session.turn.turn_id(),
                                "run_react_loop: bounded wait deadline reached, re-evaluate exit"
                            );
                            // 到点必须回到退出判断，由 pending_handoff 写交接记录。
                            continue;
                        }
                        _ = &mut registry_wait => {
                            if let Some(flag) = &context.async_ctx.idle_suspended_flag {
                                flag.store(false, Ordering::Release);
                            }
                            if let Some(mailbox) = &context.session.user_input_mailbox {
                                mailbox.leave_idle();
                            }
                            tracing::debug!(
                                turn_id = %context.session.turn.turn_id(),
                                queue_len_after_wake = context.session.queue.len(),
                                "run_react_loop: registry activity changed, continue to Receive"
                            );
                            // The signal carries no task result. Receive must
                            // re-check the queue and registry state itself.
                            continue;
                        }
                    }
                }
                if context.session.queue.has_required_for_run(&execution) {
                    tracing::debug!(
                        queue_len = context.session.queue.len(),
                        "run_react_loop: queue has pending messages, continue Receive"
                    );
                    continue;
                }
                // Re-check the derived signal after active_count and the
                // second queue check. A callback may enqueue and then commit
                // terminal state in this narrow window; either observation
                // must send us through Receive once more.
                if let Some(receiver) = idle_registry.as_mut() {
                    if receiver.has_changed().unwrap_or(false) {
                        receiver.borrow_and_update();
                        continue;
                    }
                }
                tracing::debug!(
                    idle_should_wait = should_wait,
                    queue_len = context.session.queue.len(),
                    "run_react_loop: exit (queue empty, no idle wait)"
                );
                // 有界等待到期：仍有未结算任务时写可观测交接后结束本轮，
                // 不再无限等待（结果由 scope owner 对账后在 resume/下一轮可见）。
                if let Some(handoff) = context
                    .async_ctx
                    .pending_handoff
                    .as_ref()
                    .and_then(|probe| probe())
                {
                    crate::agent::async_tasks::handoff::write_pending_handoff(
                        &context.session,
                        &handoff,
                    )
                    .await;
                }
                break 'rcra;
            }

            // Receive、队列竞态重试与 idle wake 都不消耗语义迭代预算。只有确实需要
            // 进入 Compact → Reason → Act 时才检查并推进预算；因此最后一轮 Act 提交
            // final answer 后，下一次 Receive 仍可作为唯一正常退出入口。
            if semantic_iterations >= max_iterations {
                tracing::warn!(
                    max_iterations,
                    semantic_iterations,
                    "ReAct v2 循环达到最大语义迭代次数"
                );
                return LoopResult::Error(crate::error::AgentError::MaxIterationsExceeded(
                    max_iterations,
                ));
            }
            semantic_iterations += 1;
            context.session.turn.advance_step();

            // ── before_agent hooks（首次 Receive 后执行一次）──
            // RCRA 下 Receive 是唯一队列消费点，消息已通过 drain_all() 写入 transcript，
            // 此时 before_agent 钩子（SkillPreloadMiddleware / AtMentionMiddleware 等）
            // 可通过 state.messages() 读取用户输入。
            // 替代原来在 run_react_loop 外部的 Phase 6.7 调用。
            if !loop_state.before_agent_has_run {
                loop_state.before_agent_has_run = true;
                if let Err(e) =
                    middleware_runner::run_before_agent(&context, &receive_out.input_message_ids)
                        .await
                {
                    tracing::warn!(error = %e, "[v2] before_agent hook failed");
                }
            } else if let Err(error) =
                middleware_runner::run_before_input(&context, &receive_out.input_message_ids).await
            {
                if matches!(error, crate::error::AgentError::Interrupted) {
                    return LoopResult::Interrupted;
                }
                return LoopResult::Error(error);
            }

            // ── 启动闸门 hook（首批输入准备完成后、Compact 前执行一次）──
            // 只有声明启动依赖的 middleware（如 System MCP 准入）在此阻止 loop 启动：
            // Err 不得降级，本次 loop 不进入 Compact / Reason / Act；Interrupted 仍按
            // 中断分类，不算 fatal。既有 before_agent 的软失败降级不受影响（见上）。
            if !loop_state.react_start_has_run {
                match middleware_runner::run_before_react_start(&context).await {
                    Ok(()) => loop_state.react_start_has_run = true,
                    Err(crate::error::AgentError::Interrupted) => return LoopResult::Interrupted,
                    Err(error) => {
                        tracing::warn!(error = %error, "[v2] before_react_start hook failed");
                        return LoopResult::Error(error);
                    }
                }
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

            // 完整工具仍照常执行；无工具截断必须与自然完成区分，不能靠空队列判成功。
            let truncated_without_tools = reason_out.reasoning.stop_reason
                == peri_model::StopReason::MaxTokens
                && !reason_out.reasoning.needs_tool_call();

            let stream_interruption = reason_out
                .reasoning
                .stream_interruption
                .clone()
                .filter(|_| !reason_out.reasoning.needs_tool_call());

            // ── Act ──
            let act_out = match run_stage(&context, Stage::Act, || async {
                act::run_act(ActInput {
                    context: context.clone(),
                    reasoning: reason_out.reasoning,
                    catalog: reason_out.catalog,
                })
                .await
            })
            .await
            {
                Ok(out) => out,
                Err(e) => return e,
            };

            loop_state.has_tool_calls = act_out.has_tool_calls;
            if let Some(interruption) = stream_interruption {
                if context.session.turn.is_cancelled() {
                    return LoopResult::Interrupted;
                }
                loop_state.stream_recoveries += 1;
                if loop_state.stream_recoveries >= interruption.max_attempts as usize {
                    return LoopResult::Error(crate::error::AgentError::StreamRecoveryExhausted {
                        attempts: loop_state.stream_recoveries,
                        source: interruption.error,
                    });
                }
                enqueue_stream_interruption_continuation(&context);
            }
            if truncated_without_tools {
                loop_state.consecutive_truncations += 1;
                if loop_state.consecutive_truncations > MAX_TRUNCATION_CONTINUATIONS {
                    return LoopResult::Error(crate::error::AgentError::OutputTruncated {
                        attempts: loop_state.consecutive_truncations,
                    });
                }
                enqueue_truncation_continuation(&context);
            } else {
                loop_state.consecutive_truncations = 0;
            }
            // RCRA：无论 has_tool_calls 是 true 或 false，统一回 Receive 开始新一轮迭代
            continue;
        }

        if context.session.queue.has_required_for_run(&execution) {
            loop_state.has_tool_calls = false;
            continue 'receive_retry;
        }
        return LoopResult::Completed;
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "execution_policy_test.rs"]
mod execution_policy_tests;

#[cfg(test)]
#[path = "stages_test.rs"]
mod tests;

#[cfg(test)]
#[path = "truncation_test.rs"]
mod truncation_tests;

#[cfg(test)]
#[path = "budget_recovery_integration_test.rs"]
mod budget_recovery_integration_tests;

#[cfg(test)]
#[path = "terminal_wake_test.rs"]
mod terminal_wake_tests;

#[cfg(test)]
#[path = "bounded_wait_exit_test.rs"]
mod bounded_wait_exit_tests;
