//! MQ continuation hints与扫描；以required EnsureProcessing事实决定执行。
//!
//! Kind/source不是准入条件，Passive与过期ContinueCurrentRun不启动新执行。
//! prompt lock之后重验代际和队列事实；扫描兜底通知丢失。
//! 持久控制及SDK唯一attempt准入由后续重构阶段接入。

use std::sync::Arc;

use crate::session::executor::ContinuationRequest;
use crate::transport::types::AcpError;
use peri_acp_types::cron::{CronContinuationRequest, CronTrigger};
use peri_acp_types::session::{MessageKind, MessageSource, QueuedMessage};
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::info;

use super::{
    dispatch_prompt_turn,
    task_scope::{HostTaskKind, HostTaskOwnerKind, HostTaskSpawner},
    AcpServerConfig, PromptLocks, SessionState, SharedSessions,
};

/// 合并通知；实际执行前仍须查询 MQ 中 required 工作。
pub(crate) fn take_continuation_for_request(
    state: &mut SessionState,
    _req: &ContinuationRequest,
) -> Option<u64> {
    if state.closing {
        return None;
    }
    state.continuation_mq_steering_pending = true;
    if state.continuation_in_flight {
        return None;
    }
    Some(state.continuation_epoch)
}

/// 续跑仍有效：用户显式新 prompt 递增 `continuation_epoch` 后，已排队但
/// 尚未运行的续跑应中止（新 prompt 会消费已 route 的 Defer 消息）。
pub(crate) fn continuation_still_valid(state: &SessionState, epoch: u64) -> bool {
    state.continuation_epoch == epoch
}

/// 在执行入口重验代际、关闭状态与 required 工作。
pub(crate) fn continuation_dispatchable(
    state: &SessionState,
    epoch: u64,
    has_required: bool,
) -> bool {
    !state.closing && continuation_still_valid(state, epoch) && has_required
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
                let Ok(permission_mode) = scheduled_permission_mode(&cfg, &sessions, &req.session_id).await else {
                    return;
                };
                let broker = super::prompt::build_transport_broker(&transport, &req.session_id);
                if !super::prompt::approve_scheduled_trigger(
                    permission_mode.as_ref(),
                    Some(&broker),
                    &req.trigger.task_id,
                    &req.trigger.prompt,
                )
                .await
                {
                    info!(session_id = %req.session_id, task_id = %req.trigger.task_id, "cron trigger rejected");
                    return;
                }
                let epoch = {
                    let mut sessions = sessions.lock().await;
                    let Some(state) = sessions.get_mut(&req.session_id) else { return };
                    if state.closing
                        || !enqueue_cron_trigger(&cfg, &req.session_id, &req.trigger)
                    {
                        return;
                    }
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
            return Err(AcpError::new(-32010, "Session is closing"));
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
    let mut scan = tokio::time::interval(std::time::Duration::from_millis(100));
    scan.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let req = tokio::select! {
            _ = shutdown.cancelled() => break,
            request = rx.recv() => match request { Some(request) => request, None => break },
            _ = scan.tick() => {
                let states = sessions.lock().await;
                let candidates = states.iter().filter(|(session_id, state)| {
                    !state.closing && !state.continuation_in_flight
                        && cfg.session_manager.v2_queue_for(session_id)
                            .is_some_and(|queue| queue.has_ensure_processing())
                }).map(|(session_id, _)| session_id.clone()).collect::<Vec<_>>();
                drop(states);
                let mut runnable = None;
                for session_id in candidates {
                    let control = cfg.session_resources.load_session_control(&session_id).await;
                    if control.is_ok_and(|state| state.status == peri_acp_types::session_resources::ControlStatus::Active) {
                        runnable = Some(session_id);
                        break;
                    }
                }
                let Some(session_id) = runnable else { continue };
                ContinuationRequest { session_id, kind: peri_acp_types::tasks::BgTaskKind::Agent, mq_steering: true }
            }
        };
        let control = cfg
            .session_resources
            .load_session_control(&req.session_id)
            .await;
        if !control.is_ok_and(|state| {
            state.status == peri_acp_types::session_resources::ControlStatus::Active
        }) {
            continue;
        }
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
