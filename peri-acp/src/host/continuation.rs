//! ACP server — 内部 AsyncContinuation scheduler（session-scoped, per-session coalesce）。
//!
//! # 背景
//!
//! bg subagent 的完成结果进入 SessionInbox（Defer + wake）后，session 级
//! activation listener 把待消费工作转为 [`ContinuationRequest`]。
//! 主 prompt 自然结束后允许内部续跑；`session/cancel` 则仅保留一次已授权的
//! 独立 bg agent 结果续跑。dispatch 收尾再次检查队列，覆盖结束与投递竞态。
//! scheduler 复用主 prompt 执行路径，让父 agent 消费 deferred callback。
//!
//! # 语义约束
//!
//! - **每 session coalesce**：`SessionState::continuation_armed` 由 `session/cancel`
//!   置位（只影响当前 prompt）；bg agent（`BgTaskKind::Agent`）完成通知到达后
//!   原子 take，只运行一次。Shell/Workflow 完成不触发。
//! - **cancel ↔ bg callback race 兜底**：bg 完成通知可能在 cancel 置位前被
//!   scheduler 跳过（armed=false），但其结果已 route 为 Defer/SubAgentComplete。
//!   `session/cancel` 检查队列确有 pending SubAgentComplete Defer 时，在锁外
//!   经 continuation sender 补发 `BgTaskKind::Agent` 请求（`notify.rs`），
//!   保证 Defer 不会永久滞留。Shell/Workflow 不产生 SubAgentComplete Defer，
//!   不会误触发。
//! - **取消续跑不链式**：取消正在执行的 continuation（`continuation_in_flight`）
//!   不置位 armed——否则形成"取消续跑 → 再续跑"的自动链式续跑。
//! - **dispatch 前确认 Defer 仍在**：scheduler 拿锁并校验代际后，再确认队列
//!   仍有 SubAgentComplete Defer；没有则跳过空跑（不触发无意义 LLM 调用）。
//! - **同一执行路径**：续跑通过与用户 prompt 完全相同的 [`dispatch_prompt_turn`]
//!   （pool 取出/归还、per-session prompt lock、run_prompt 后处理），不复制
//!   agent execution。
//! - **用户显式新 prompt 清除未运行的续跑**：prompt dispatch 置位前清除
//!   `continuation_armed` 并递增 `continuation_epoch`；scheduler 在**获取
//!   prompt lock 之后**校验代际，代际变化（新 prompt 已排队/已执行）则放弃。
//! - **严禁**由 TUI kit bridge / `SubmitRequest::KeepGoing` 触发 agent loop：
//!   KeepGoing 仍只能是用户按钮；本 scheduler 是唯一的内部触发方。

use std::sync::Arc;

use crate::session::executor::ContinuationRequest;
use peri_acp_types::cron::{
    cron_trigger_reminder, CronContinuationRequest, CronTrigger, CronTriggerReminderError,
};
use peri_acp_types::session::{MessageKind, MessageQueue, MessageSource, QueuedMessage};
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminder, TrustedSystemReminderFactory,
    SYSTEM_REMINDER_VERSION,
};
use peri_acp_types::tasks::BgTaskKind;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::{
    dispatch_prompt_turn,
    task_scope::{HostTaskKind, HostTaskOwnerKind, HostTaskSpawner},
    AcpServerConfig, PromptLocks, SessionState, SharedSessions,
};

/// 判定并**原子 take** session 的 continuation 标记（每 session coalesce）。
///
/// 仅当请求 kind 为 bg agent（`BgTaskKind::Agent`）且标记已置位时返回
/// `Some(epoch)`（调用方随后运行一次续跑）；其余情况返回 `None`（跳过）。
/// take 后标记立即清除——同一取消轮次的后续 bg 完成不会重复续跑。
pub(crate) fn take_continuation_if_armed(
    state: &mut SessionState,
    kind: BgTaskKind,
) -> Option<u64> {
    if kind != BgTaskKind::Agent || !state.continuation_armed {
        return None;
    }
    state.continuation_armed = false;
    Some(state.continuation_epoch)
}

/// 消费 [`ContinuationRequest`]：bg cancel 续跑或 loop 后 MQ steering 续跑。
pub(crate) fn take_continuation_for_request(
    state: &mut SessionState,
    req: &ContinuationRequest,
) -> Option<u64> {
    if req.mq_steering {
        if state.continuation_in_flight {
            // 续跑执行中：不立即调度，但保留 pending，待 in_flight 清除后由
            // dispatch_prompt_turn 尾端补发 ContinuationRequest。
            state.continuation_mq_steering_pending = true;
            return None;
        }
        state.continuation_mq_steering_pending = true;
        return Some(state.continuation_epoch);
    }
    take_continuation_if_armed(state, req.kind)
}

/// `session/cancel` 是否应置位 continuation 标记。
///
/// 取消**正在执行的 continuation**（`continuation_in_flight`）时不置位：
/// 否则用户取消续跑后 bg Defer 再次触发 scheduler，形成"取消续跑 → 再续跑"
/// 的自动链式续跑。被取消续跑遗留的 Defer 由后续用户 prompt 消费。
pub(crate) fn cancel_arms_continuation(state: &SessionState) -> bool {
    !state.continuation_in_flight
}

/// `session/cancel` 是否需要**立即**补发一次 continuation 请求（race 兜底）。
///
/// Race 场景：bg callback 已 route 为 Defer/SubAgentComplete（队列可见），但其
/// continuation 通知恰在 cancel 置位前被 scheduler 跳过（armed=false 时 take
/// 失败）。此后不会有新的 bg 完成通知，Defer 将永久滞留。cancel 检查队列确有
/// pending SubAgentComplete Defer 时补发 `BgTaskKind::Agent` 请求（Shell/Workflow
/// 完成不产生 SubAgentComplete Defer，不会误触发）。
///
/// 需要同时满足：cancel 会置位 armed（非 in_flight）且队列存在待消费的
/// SubAgentComplete Defer。若取消的是续跑本身（in_flight），不补发。
pub(crate) fn cancel_should_schedule_continuation(
    state: &SessionState,
    has_pending_subagent_defer: bool,
) -> bool {
    cancel_arms_continuation(state) && has_pending_subagent_defer
}

/// 续跑仍有效：用户显式新 prompt 递增 `continuation_epoch` 后，已排队但
/// 尚未运行的续跑应中止（新 prompt 会消费已 route 的 Defer 消息）。
pub(crate) fn continuation_still_valid(state: &SessionState, epoch: u64) -> bool {
    state.continuation_epoch == epoch
}

/// 续跑是否真正可 dispatch：代际未变 **且** 队列中仍有待消费的
/// SubAgentComplete Defer。两者缺一即跳过空跑——
/// - 代际变化：用户新 prompt 已排队/已执行（Defer 由新 prompt 消费）；
/// - 队列无 Defer：Defer 已被其他路径消费，续跑空转一次 LLM 无意义。
pub(crate) fn continuation_dispatchable(
    state: &SessionState,
    epoch: u64,
    has_pending_subagent_defer: bool,
    has_pending_mq: bool,
    mq_steering: bool,
) -> bool {
    if !continuation_still_valid(state, epoch) {
        return false;
    }
    if mq_steering {
        return has_pending_mq;
    }
    has_pending_subagent_defer
}

pub(crate) struct CronContinuationContext {
    pub(crate) sessions: SharedSessions,
    pub(crate) prompt_locks: PromptLocks,
    pub(crate) cfg: Arc<AcpServerConfig>,
    pub(crate) transport: Arc<dyn crate::transport::AcpTransport>,
    pub(crate) cont_tx: Arc<mpsc::UnboundedSender<ContinuationRequest>>,
    pub(crate) task_spawner: HostTaskSpawner,
    pub(crate) shutdown: CancellationToken,
}

pub(crate) async fn run_cron_continuation_scheduler(
    mut rx: mpsc::UnboundedReceiver<CronContinuationRequest>,
    context: CronContinuationContext,
) {
    let CronContinuationContext {
        sessions,
        prompt_locks,
        cfg,
        transport,
        cont_tx,
        task_spawner,
        shutdown,
    } = context;
    while let Some(req) = recv_until_shutdown(&mut rx, &shutdown).await {
        let sessions = sessions.clone();
        let locks = prompt_locks.clone();
        let cfg = Arc::clone(&cfg);
        let transport = Arc::clone(&transport);
        let cont_tx = Arc::clone(&cont_tx);
        let spawn_session = req.session_id.clone();
        let admitted = task_spawner.spawn(
            HostTaskOwnerKind::Session,
            HostTaskKind::ContinuationTurn,
            async move {
                let environment = sessions.lock().await.get(&req.session_id).and_then(|state| state.environment.clone());
                let deployment = environment.as_ref().map(|env| &env.cfg).unwrap_or(&cfg);
                match super::execution::approve_schedule(&req, &sessions, &locks, deployment, &transport).await {
                    Ok(true) => {}
                    Ok(false) => {
                        info!(session_id = %req.session_id, task_id = %req.trigger.task_id, "cron trigger rejected or cancelled");
                        return;
                    }
                    Err(error) => {
                        tracing::error!(session_id = %req.session_id, code = error.code, error = %error.message, "cron approval failed");
                        return;
                    }
                }
                let epoch = {
                    let mut sessions = sessions.lock().await;
                    let Some(state) = sessions.get_mut(&req.session_id) else { return };
                    let Some(current_inbox) = deployment.session_manager.v2_queue_for(&req.session_id) else { return };
                    if !current_inbox.subscribe_wake().same_channel(&req.inbox.subscribe_wake()) { return; }
                    if state.closing {
                        return;
                    }
                    // M13：本次触发必须可观察地被接纳或拒绝，不得静默丢触发。
                    // 拒绝即不执行任务指令——超长提示绝不截断后照常执行。
                    match enqueue_cron_trigger(&current_inbox, &req.trigger) {
                        CronTriggerAdmission::Delivered => {}
                        CronTriggerAdmission::Rejected { reason } => {
                            warn!(
                                session_id = %req.session_id,
                                task_id = %req.trigger.task_id,
                                %reason,
                                "cron trigger rejected: task instruction was not executed"
                            );
                            return;
                        }
                    }
                    state.continuation_mq_steering_pending = true;
                    state.continuation_epoch
                };
                let result = dispatch_prompt_turn(
                    continuation_params(&req.session_id),
                    super::PromptOrigin::Scheduled,
                    Some(epoch),
                    &sessions,
                    &locks,
                    &transport,
                    &cfg,
                    cont_tx.as_ref(),
                )
                .await;
                if let Err(error) = result {
                    tracing::error!(session_id = %req.session_id, code = error.code, error = %error.message, "cron dispatch failed");
                }
                super::user_input::schedule_mailbox(
                    &req.session_id,
                    &sessions,
                    &locks,
                    &cfg,
                    &transport,
                    &cont_tx,
                );
            },
        );
        if let Err(error) = admitted {
            tracing::error!(session_id = %spawn_session, error = ?error, "cron task admission failed");
        }
    }
}

pub(crate) async fn scheduled_permission_mode(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    session_id: &str,
) -> Result<Arc<peri_acp_types::permission::SharedPermissionMode>, crate::transport::types::AcpError>
{
    let permission_mode = {
        let sessions = sessions.lock().await;
        let state = sessions
            .get(session_id)
            .ok_or_else(|| crate::transport::types::AcpError::new(-32602, "session not found"))?;
        if state.closing {
            return Err(crate::transport::types::AcpError::new(
                -32010,
                "Session is closing",
            ));
        }
        state
            .environment
            .as_ref()
            .map(|env| &env.cfg)
            .unwrap_or(cfg)
            .permission_mode
            .clone()
    };
    // 派发前的先行检查：只复核已记录证据。真正的准入复核在 dispatch_prompt_turn
    // 取得 prompt lock 之后（本次准入的权威检查）。
    super::workspace::reassert_expected(cfg, session_id, None).await?;
    Ok(permission_mode)
}

/// 一次 cron 触发的准入结果；调用方必须能观察到拒绝（不得静默丢触发）。
#[derive(Debug, PartialEq, Eq)]
pub(super) enum CronTriggerAdmission {
    Delivered,
    Rejected { reason: String },
}

/// 把一次 cron 触发投递进会话队列。
///
/// - delivery_id 由 task + 本次 firing 身份派生：同一 firing 的重试/重复发布
///   只投递一次，新触发（新 firing_id）即使文案相同也各自投递一次。
/// - 超出可承载预算的历史任务不会 panic，也不会被静默截断后照常执行：
///   改为投递一条可观察的失败通知（Tui/Automation 受众，不唤醒模型推理），
///   失败证据随持久化投递保留。
pub(super) fn enqueue_cron_trigger(
    queue: &MessageQueue,
    trigger: &CronTrigger,
) -> CronTriggerAdmission {
    match cron_trigger_reminder(&trigger.task_id, &trigger.firing_id, &trigger.prompt) {
        Ok(reminder) => {
            let delivery_id =
                peri_acp_types::cron::cron_firing_delivery_id(&trigger.task_id, &trigger.firing_id);
            queue.push(QueuedMessage::system_reminder_with_delivery_id(
                MessageKind::Defer,
                MessageSource::CronTrigger,
                reminder,
                delivery_id,
            ));
            CronTriggerAdmission::Delivered
        }
        Err(error) => {
            let notice = undeliverable_cron_reminder(trigger, &error);
            match notice {
                Some(notice) => queue.push(QueuedMessage::system_reminder(
                    MessageKind::Info,
                    MessageSource::CronTrigger,
                    notice,
                )),
                None => warn!(
                    task_id = %trigger.task_id,
                    firing_id = %trigger.firing_id,
                    %error,
                    "cron trigger is undeliverable and its failure notice could not be built"
                ),
            }
            warn!(
                task_id = %trigger.task_id,
                firing_id = %trigger.firing_id,
                prompt_bytes = trigger.prompt.len(),
                %error,
                "cron trigger rejected: task instruction was not executed"
            );
            CronTriggerAdmission::Rejected {
                reason: error.to_string(),
            }
        }
    }
}

/// 可观察的投递失败通知：正文只含事件身份与限制摘要，不含原始（可能超长）指令。
fn undeliverable_cron_reminder(
    trigger: &CronTrigger,
    error: &CronTriggerReminderError,
) -> Option<TrustedSystemReminder> {
    TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Lifecycle,
            source: ReminderSource("cron".into()),
            kind: "trigger_undeliverable".into(),
            severity: ReminderSeverity::Warning,
            delivery: ReminderDelivery::Required,
            audiences: ReminderAudiences(vec![ReminderAudience::Tui, ReminderAudience::Automation]),
            body: format!(
                "Cron task {} could not be delivered and was not executed: {}",
                trigger.task_id, error
            ),
            summary: Some(format!("Cron task {} undeliverable", trigger.task_id)),
            metadata: serde_json::json!({
                "task_id": trigger.task_id,
                "firing_id": trigger.firing_id,
                "prompt_bytes": trigger.prompt.len(),
            }),
        })
        .ok()
}

/// 运行 per-session continuation scheduler（由 `run_acp_server` spawn）。
///
/// 循环消费 executor `on_bg_complete` 闭包的通知；每次合格请求 spawn 一个
/// 续跑任务：获取该 session 的 prompt lock（与用户 prompt 同一把锁）→ 校验
/// 代际 → 以 `continuation=true` 走 [`dispatch_prompt_turn`]。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_continuation_scheduler(
    mut rx: mpsc::UnboundedReceiver<ContinuationRequest>,
    sessions: SharedSessions,
    prompt_locks: PromptLocks,
    cfg: Arc<AcpServerConfig>,
    transport: Arc<dyn crate::transport::AcpTransport>,
    cont_tx: std::sync::Weak<mpsc::UnboundedSender<ContinuationRequest>>,
    task_spawner: HostTaskSpawner,
    shutdown: CancellationToken,
) {
    loop {
        let Some(req) = recv_until_shutdown(&mut rx, &shutdown).await else {
            break;
        };
        // eligibility + 原子 take（每 session 只运行一次）
        let epoch = {
            let mut sessions = sessions.lock().await;
            match sessions.get_mut(&req.session_id) {
                Some(state) => {
                    let local = state
                        .environment
                        .as_ref()
                        .map(|env| &env.cfg)
                        .unwrap_or(&cfg);
                    if state.closing
                        || (req.mq_steering
                            && !super::activation::mq_allowed(local, &req.session_id))
                    {
                        None
                    } else {
                        take_continuation_for_request(state, &req)
                    }
                }
                None => None,
            }
        };
        let Some(epoch) = epoch else {
            continue;
        };
        info!(
            session_id = %req.session_id,
            mq_steering = req.mq_steering,
            "continuation: scheduling AsyncContinuation"
        );

        let session_id = req.session_id.clone();
        let sessions2 = sessions.clone();
        let locks2 = prompt_locks.clone();
        let cfg2 = Arc::clone(&cfg);
        let transport2 = Arc::clone(&transport);
        let cont_tx2 = cont_tx.clone();
        let admitted = task_spawner.spawn(
            HostTaskOwnerKind::Session,
            HostTaskKind::ContinuationTurn,
            async move {
                let Some(cont_tx2) = cont_tx2.upgrade() else {
                    return;
                };
                // dispatch_prompt_turn 在获取同一把 prompt lock 后校验 epoch 和
                // SubAgentComplete Defer，避免本处预先持锁后再次获取导致死锁。
                let params = continuation_params(&session_id);
                let result = dispatch_prompt_turn(
                    params,
                    super::PromptOrigin::Continuation {
                        mq_steering: req.mq_steering,
                    },
                    Some(epoch),
                    &sessions2,
                    &locks2,
                    &transport2,
                    &cfg2,
                    cont_tx2.as_ref(),
                )
                .await;
                if let Err(error) = result {
                    tracing::error!(session_id = %session_id, code = error.code, error = %error.message, "continuation dispatch failed");
                }
                super::user_input::schedule_mailbox(
                    &session_id,
                    &sessions2,
                    &locks2,
                    &cfg2,
                    &transport2,
                    &cont_tx2,
                );
            },
        );
        if let Err(error) = admitted {
            tracing::error!(session_id = %req.session_id, error = ?error, "continuation task admission failed");
        }
    }
}

async fn recv_until_shutdown<T>(
    rx: &mut mpsc::UnboundedReceiver<T>,
    shutdown: &CancellationToken,
) -> Option<T> {
    tokio::select! {
        _ = shutdown.cancelled() => None,
        req = rx.recv() => req,
    }
}

/// 构造内部续跑请求参数：仅携带 sessionId + 空 message。
///
/// 空 content 在 `run_prompt` 中解析为空 `MessageContent`，配合
/// `continuation=true` 不写入空 human prompt、不触发 keepgoing 语义。
fn continuation_params(session_id: &str) -> Value {
    serde_json::json!({
        "sessionId": session_id,
        "message": { "role": "user", "content": [] },
    })
}

#[cfg(test)]
#[path = "continuation_test.rs"]
mod tests;
