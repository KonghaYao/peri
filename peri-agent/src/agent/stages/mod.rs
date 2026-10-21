//! ReAct v2 — 四阶段循环（RCRA）
//!
//! 每阶段有明确的类型契约（StageInput → StageOutput），可脱离完整 Agent 单独测试。
//! 阶段间依赖通过输入结构体声明，不读全局状态。
//!
//! 控制流：`Receive → Compact → Reason → Act → (回 Receive)`
//! Receive 是循环入口，也是退出判断点：队列空 + 无 idle 等待时退出。

pub mod act;
mod archive_capability;
mod async_context;
pub mod compact;
mod compact_progress;
mod context_builder;
mod invocation_context;
pub mod middleware_runner;
mod model_view;
mod null_llm;
mod queue_to_transcript;
pub mod reason;
pub mod receive;
pub use receive::{ReceiveInput, ReceiveOutput};
pub mod tool_dispatch;
pub use invocation_context::{
    ExecutionEntryOrigin, InvocationContext, ModelTurnFacts, PostHookSkipSink,
    SessionInvocationSource,
};
pub use null_llm::NullReactLLM;
pub use tool_dispatch::invocation_dispatcher;

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
use model_view::ModelViewScope;

/// 共享工具注册表类型别名（避免 clippy::type_complexity）
pub type SharedToolMap = Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>;

// ─── 循环控制 ───────────────────────────────────────────────────────────────

/// 循环最终结果
#[derive(Debug)]
pub enum LoopResult {
    /// 正常结束（无更多消息）
    Completed,
    /// 本次 child 收尾窗口到期，不再准入新语义迭代；由 runner 交接资源。
    /// `unprocessed_tool_calls` 是已准入 Act 产生、尚未被下一次 Reason 读取的
    /// tool call 身份；它们只存在于 transcript，不在 MQ。
    SettlementDue { unprocessed_tool_calls: Vec<String> },
    /// hook 显式停止（PostToolBatch `continue:false`）：不再发起模型请求。
    /// 已准入 Act 的 tool call 只存在于 transcript，必须随结果交给 runner。
    HookStopped { unprocessed_tool_calls: Vec<String> },
    /// 被中断
    Interrupted,
    /// 错误
    Error(crate::error::AgentError),
}

mod context;
mod micro_facts;
pub use context::{
    CompactContext, RuntimeServices, SessionHandle, StageContext, StageContextBuilder,
};

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
    pub exposure: crate::agent::compact_v2::exposure::RequestExposure,
    /// 本次请求实际投影到的后台结果；提交成功后由 Act 结清。
    pub receipt: Option<crate::session::async_result_mailbox::RequestReceipt>,
}

// ─── Act 阶段类型 ────────────────────────────────────────────────────────────

/// Act 阶段输入
pub struct ActInput {
    pub context: StageContext,
    /// Reason 阶段的推理结果
    pub reasoning: crate::agent::react::Reasoning,
    pub exposure: crate::agent::compact_v2::exposure::RequestExposure,
    /// Exact catalog pinned by Reason.
    pub catalog: Arc<SessionToolCatalogSnapshot>,
    /// Reason 记录的请求覆盖范围；只有有效提交后才能结清。
    pub receipt: Option<crate::session::async_result_mailbox::RequestReceipt>,
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

/// 阶段失败时保留后台结果的处理责任：用户 Stop 进入 Suspended，其它失败进入
/// Blocked，等待显式继续。这里不自动重试，也不把未覆盖当成已交付。
fn record_stage_failure(context: &StageContext, stage: &str, failure: &LoopResult) {
    match failure {
        LoopResult::Interrupted => context
            .session
            .async_result_mailbox
            .suspend_scope("cancelled"),
        LoopResult::Error(error) => context
            .session
            .async_result_mailbox
            .block_uncovered(stage, &error.to_string()),
        // 非失败出口：Hook stop 已在退出点挂起，结算到期由 child runner 交接，
        // 都不在这里改写处理责任。
        LoopResult::Completed
        | LoopResult::SettlementDue { .. }
        | LoopResult::HookStopped { .. } => {}
    }
}

/// 用户中断的唯一出口：先挂起未结清责任，再返回 Interrupted。
fn interrupted(context: &StageContext) -> LoopResult {
    context
        .session
        .async_result_mailbox
        .suspend_scope("cancelled");
    LoopResult::Interrupted
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
/// child 收尾截止：到期后不再消费新输入或准入新语义迭代。
pub(crate) fn settlement_due(context: &StageContext) -> bool {
    context
        .async_ctx
        .child_settlement
        .as_ref()
        .is_some_and(|window| window.expired())
}

pub async fn run_react_loop(context: StageContext, max_iterations: usize) -> LoopResult {
    let mut loop_state = LoopState::default();
    let mut semantic_iterations = 0usize;
    let execution = context.session.turn.execution_binding();
    let initial_input_watermark = context.session.queue.admission_watermark();
    // Keep one receiver for the lifetime of this loop so its observed version
    // advances across idle retries. StageContext clones are short-lived stage
    // inputs and must not own the receive cursor.
    let mut idle_registry = context.async_ctx.idle_registry.clone();
    const MAX_TRUNCATION_CONTINUATIONS: usize = 2;

    'receive_retry: loop {
        'rcra: loop {
            // 检查 cancel
            if context.session.turn.is_cancelled() {
                return interrupted(&context);
            }
            if settlement_due(&context) {
                return LoopResult::SettlementDue {
                    unprocessed_tool_calls: context.async_ctx.unprocessed_tool_calls.lock().clone(),
                };
            }

            // ── Receive（循环入口，也是退出判断点）──
            let receive_out = match run_stage(&context, Stage::Receive, || async {
                receive::run_receive_with_budget(
                    ReceiveInput {
                        context: context.clone(),
                    },
                    (semantic_iterations >= max_iterations).then_some(initial_input_watermark),
                )
                .await
            })
            .await
            {
                Ok(out) => out,
                Err(e) => return e,
            };

            // Hook 显式停止意图（PostToolBatch `continue:false`）：Receive 是循环的
            // 唯一退出口；消费到停止意图返回 `HookStopped`，不再发起模型请求。
            // 子 Agent 由 runner 按未推理义务交接；主会话由 executor 映射为正常
            // Done。停止意图已绑定执行身份，旧 run 的 Stop 不会终止新执行。
            if receive_out.stop_requested {
                // Stop 优先：未处理结果进入 Suspended，不得保留 Ready 触发自动续跑。
                context
                    .session
                    .async_result_mailbox
                    .suspend_scope("hook stop");
                // 同批已提交但不会推理的消息保留为交接义务。
                context
                    .async_ctx
                    .received_after_deadline
                    .lock()
                    .extend(receive_out.process_message_ids.clone());
                if let Some(flag) = &context.async_ctx.hook_stop_requested {
                    flag.store(true, Ordering::Release);
                }
                tracing::info!(
                    consumed = receive_out.consumed_count,
                    "Receive: hook stop intent consumed; exiting run without further model request"
                );
                return LoopResult::HookStopped {
                    unprocessed_tool_calls: context.async_ctx.unprocessed_tool_calls.lock().clone(),
                };
            }

            // Receive 的持久化等待可能跨过 D：已落库结果保留，但不再准入新语义迭代。
            if settlement_due(&context) {
                context
                    .async_ctx
                    .received_after_deadline
                    .lock()
                    .extend(receive_out.process_message_ids.clone());
                return LoopResult::SettlementDue {
                    unprocessed_tool_calls: context.async_ctx.unprocessed_tool_calls.lock().clone(),
                };
            }

            // 退出判断：本轮没有可唤醒消息且上一轮无工具调用 → 检查是否该退出。
            // Info 只做状态维护，虽被 Receive 消费，但不能单独驱动 Compact → Reason → Act。
            // 工具调用结果写入 transcript 而非队列：has_tool_calls=true 时
            // wake_up_count=0 是正常状态——继续循环让 LLM 处理工具结果。
            if receive_out.wake_up_count == 0
                && receive_out.budget_deferred_count == 0
                && !loop_state.has_tool_calls
                // 续跑请求可能没有队列消息：Ready 结果必须进入本次请求处理，
                // 不能以“队列为空”为由提前退出并留下自动续跑候选。
                && !context.session.async_result_mailbox.has_ready()
            {
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
                    let outcome = context
                        .runtime
                        .event_bus
                        .emit_render_reliable(crate::agent::events_v2::RenderEvent::TurnSuspended {
                            turn_id: context.turn_id(),
                            agent_id: context.session.agent_id,
                        })
                        .await;
                    if outcome == peri_acp_types::event_v2::EmitOutcome::Closed {
                        tracing::warn!(turn_id = %context.turn_id(), "TurnSuspended 接收端关闭；总线已记录需重同步");
                    }
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
                            return interrupted(&context);
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
                                return interrupted(&context);
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
                    if let Err(error) = crate::agent::async_tasks::handoff::write_pending_handoff(
                        &context.session,
                        &handoff,
                    )
                    .await
                    {
                        return LoopResult::Error(anyhow::anyhow!(error).into());
                    }
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
                    budget_deferred_count = receive_out.budget_deferred_count,
                    initial_input_watermark,
                    "ReAct v2 循环达到最大语义迭代次数"
                );
                let failure = LoopResult::Error(crate::error::AgentError::MaxIterationsExceeded(
                    max_iterations,
                ));
                record_stage_failure(&context, "max_iterations", &failure);
                return failure;
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
                    if matches!(e, crate::error::AgentError::Interrupted) {
                        return interrupted(&context);
                    }
                    tracing::warn!(error = %e, "[v2] before_agent hook failed");
                }
            } else if let Err(error) =
                middleware_runner::run_before_input(&context, &receive_out.input_message_ids).await
            {
                if matches!(error, crate::error::AgentError::Interrupted) {
                    return interrupted(&context);
                }
                let failure = LoopResult::Error(error);
                record_stage_failure(&context, "before_input", &failure);
                return failure;
            }

            // ── 启动闸门 hook（首批输入准备完成后、Compact 前执行一次）──
            // 只有声明启动依赖的 middleware（如 System MCP 准入）在此阻止 loop 启动：
            // Err 不得降级，本次 loop 不进入 Compact / Reason / Act；Interrupted 仍按
            // 中断分类，不算 fatal。既有 before_agent 的软失败降级不受影响（见上）。
            if !loop_state.react_start_has_run {
                match middleware_runner::run_before_react_start(&context).await {
                    Ok(()) => loop_state.react_start_has_run = true,
                    Err(crate::error::AgentError::Interrupted) => return interrupted(&context),
                    Err(error) => {
                        tracing::warn!(error = %error, "[v2] before_react_start hook failed");
                        let failure = LoopResult::Error(error);
                        record_stage_failure(&context, "before_react_start", &failure);
                        return failure;
                    }
                }
            }

            // ── Compact ──
            // Compact 输出（compacted 标志）当前无调用方：compact 的副作用已直接
            // 写入 transcript/flags 与事件流，此处仅保留阶段观测与错误传播。
            //
            // 模型视图复用槽从本轮 Compact 检查起算（本轮 Compact 判断前的 refresh
            // 与随后 Reason 的准备检查、请求快照共用同一份同版本视图）；每轮迭代
            // 入口清空，缓存不跨迭代常驻。
            context.model_view.reset();
            if let Err(e) = run_stage(&context, Stage::Compact, || async {
                compact::run_compact(CompactInput {
                    context: context.clone(),
                    has_tool_calls: loop_state.has_tool_calls,
                })
                .await
            })
            .await
            {
                record_stage_failure(&context, "compact", &e);
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
                Err(e) => {
                    record_stage_failure(&context, "reason", &e);
                    return e;
                }
            };

            // Reason 成功后上一批义务已被读取；失败时仍保留供 runner 交接。
            context.async_ctx.unprocessed_tool_calls.lock().clear();

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
            let unprocessed_tool_calls: Vec<String> = reason_out
                .reasoning
                .tool_calls
                .iter()
                .map(|call| call.id.clone())
                .collect();
            let act_out = match run_stage(&context, Stage::Act, || async {
                act::run_act(ActInput {
                    context: context.clone(),
                    reasoning: reason_out.reasoning,
                    catalog: reason_out.catalog,
                    receipt: reason_out.receipt,
                    exposure: reason_out.exposure,
                })
                .await
            })
            .await
            {
                Ok(out) => out,
                Err(e) => {
                    record_stage_failure(&context, "act", &e);
                    return e;
                }
            };

            loop_state.has_tool_calls = act_out.has_tool_calls;
            *context.async_ctx.unprocessed_tool_calls.lock() = if act_out.has_tool_calls {
                unprocessed_tool_calls
            } else {
                Vec::new()
            };
            if let Some(interruption) = stream_interruption {
                if context.session.turn.is_cancelled() {
                    return interrupted(&context);
                }
                loop_state.stream_recoveries += 1;
                if loop_state.stream_recoveries >= interruption.max_attempts as usize {
                    let failure =
                        LoopResult::Error(crate::error::AgentError::StreamRecoveryExhausted {
                            attempts: loop_state.stream_recoveries,
                            source: interruption.error,
                        });
                    record_stage_failure(&context, "stream_recovery", &failure);
                    return failure;
                }
                enqueue_stream_interruption_continuation(&context);
            }
            if truncated_without_tools {
                loop_state.consecutive_truncations += 1;
                if loop_state.consecutive_truncations > MAX_TRUNCATION_CONTINUATIONS {
                    // 终态在 v2_execute 被分类为 failure=None（stop_reason=MaxTokens、
                    // turn_status=Error），该路径没有 fatal error!；此处是缺口唯一的可见记录点。
                    tracing::warn!(
                        attempts = loop_state.consecutive_truncations,
                        max_continuations = MAX_TRUNCATION_CONTINUATIONS,
                        model = %context.runtime.llm.model_name(),
                        "连续无工具截断耗尽续跑预算，终止本轮"
                    );
                    let failure = LoopResult::Error(crate::error::AgentError::OutputTruncated {
                        attempts: loop_state.consecutive_truncations,
                    });
                    record_stage_failure(&context, "output_truncated", &failure);
                    return failure;
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
#[path = "default_iteration_budget_test.rs"]
mod default_iteration_budget_tests;

#[cfg(test)]
#[path = "terminal_wake_test.rs"]
mod terminal_wake_tests;

#[cfg(test)]
#[path = "bounded_wait_exit_test.rs"]
mod bounded_wait_exit_tests;

#[cfg(test)]
#[path = "model_view_test.rs"]
mod model_view_tests;

#[cfg(test)]
#[path = "micro_exposure_test.rs"]
mod micro_exposure_tests;

#[cfg(test)]
#[path = "micro_turn_exposure_test.rs"]
mod micro_turn_exposure_tests;
