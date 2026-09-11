//! ACP server — 内部 AsyncContinuation scheduler（session-scoped, per-session coalesce）。
//!
//! # 背景
//!
//! bg subagent 独立运行时，主 session/prompt 被 `session/cancel` 取消后，
//! executor 的 `on_bg_complete` 闭包仍会把 bg 结果**先** route 到 SessionInbox
//! （Defer + wake），**再**通过 [`ContinuationRequest`] 通知本 scheduler。
//! 此时主 agent 已不在 loop 中，必须由本 scheduler 自动发起一次内部续跑，
//! 让父 agent 消费 deferred callback。
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
use peri_acp_types::cron::{CronContinuationRequest, CronTrigger};
use peri_acp_types::session::{MessageKind, MessageSource, QueuedMessage};
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};
use peri_acp_types::tasks::BgTaskKind;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::info;

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
) -> bool {
    if !continuation_still_valid(state, epoch) {
        return false;
    }
    if state.continuation_mq_steering_pending {
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
        let _ = task_spawner.spawn(
            HostTaskOwnerKind::Session,
            HostTaskKind::ContinuationTurn,
            async move {
                let broker = super::prompt::build_transport_broker(&transport, &req.session_id);
                if !super::prompt::approve_scheduled_trigger(
                    cfg.permission_mode.as_ref(),
                    Some(&broker),
                    &req.trigger.task_id,
                    &req.trigger.prompt,
                )
                .await
                {
                    info!(session_id = %req.session_id, task_id = %req.trigger.task_id, "cron trigger rejected");
                    return;
                }
                if !enqueue_cron_trigger(&cfg, &req.session_id, &req.trigger) {
                    return;
                }
                let epoch = {
                    let mut sessions = sessions.lock().await;
                    let Some(state) = sessions.get_mut(&req.session_id) else { return };
                    state.continuation_mq_steering_pending = true;
                    state.continuation_epoch
                };
                let _ = dispatch_prompt_turn(
                    continuation_params(&req.session_id),
                    true,
                    Some(epoch),
                    &sessions,
                    &locks,
                    &transport,
                    &cfg,
                    cont_tx.as_ref(),
                )
                .await;
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
    }
}

fn enqueue_cron_trigger(cfg: &AcpServerConfig, session_id: &str, trigger: &CronTrigger) -> bool {
    let Some(queue) = cfg.session_manager.v2_queue_for(session_id) else {
        return false;
    };
    let reminder = TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSource("cron".into()),
            kind: "triggered".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Required,
            audiences: ReminderAudiences(vec![
                ReminderAudience::Model,
                ReminderAudience::Tui,
                ReminderAudience::Automation,
                ReminderAudience::Diagnostics,
            ]),
            body: format!(
                "<goal-message>Cron task {} triggered: {}</goal-message>",
                trigger.task_id, trigger.prompt
            ),
            summary: Some(format!("Cron task {} triggered", trigger.task_id)),
            metadata: serde_json::json!({ "task_id": trigger.task_id }),
        })
        .expect("cron reminder mapping must be valid");
    queue.push(QueuedMessage::system_reminder(
        MessageKind::Defer,
        MessageSource::CronTrigger,
        reminder,
    ));
    true
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
                Some(state) => take_continuation_for_request(state, &req),
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
        let _ = task_spawner.spawn(
            HostTaskOwnerKind::Session,
            HostTaskKind::ContinuationTurn,
            async move {
                let Some(cont_tx2) = cont_tx2.upgrade() else {
                    return;
                };
                // dispatch_prompt_turn 在获取同一把 prompt lock 后校验 epoch 和
                // SubAgentComplete Defer，避免本处预先持锁后再次获取导致死锁。
                let params = continuation_params(&session_id);
                let _ = dispatch_prompt_turn(
                    params,
                    true,
                    Some(epoch),
                    &sessions2,
                    &locks2,
                    &transport2,
                    &cfg2,
                    cont_tx2.as_ref(),
                )
                .await;
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
