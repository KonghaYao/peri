//! Durable inbox publication and SDK work-availability hints; never executes RCRA.

use super::{
    AcpServerConfig, PromptLocks, SharedSessions,
    task_scope::{HostTaskKind, HostTaskOwnerKind, HostTaskSpawner},
};
use crate::session::executor::ContinuationRequest;
#[cfg(test)]
use crate::transport::types::AcpError;
use peri_acp_types::cron::{CronContinuationRequest, CronTrigger};
use peri_acp_types::session::{MessageKind, MessageQueue, MessageSource, QueuedMessage};
use peri_acp_types::session_resources::ControlState;
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SYSTEM_REMINDER_VERSION, SystemReminder, TrustedSystemReminderFactory,
};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::warn;

pub(crate) struct CronContinuationContext {
    pub(crate) cfg: Arc<AcpServerConfig>,
    pub(crate) transport: Arc<dyn crate::transport::AcpTransport>,
    pub(crate) task_spawner: HostTaskSpawner,
    pub(crate) shutdown: CancellationToken,
}

fn same_recipient(current: &ControlState, frozen: &ControlState) -> bool {
    current.lifecycle == frozen.lifecycle
}

pub(crate) async fn run_cron_continuation_scheduler(
    mut rx: mpsc::UnboundedReceiver<CronContinuationRequest>,
    context: CronContinuationContext,
) {
    let CronContinuationContext {
        cfg,
        transport,
        task_spawner,
        shutdown,
        ..
    } = context;
    while let Some(req) = recv_until_shutdown(&mut rx, &shutdown).await {
        let cfg = Arc::clone(&cfg);
        let transport = Arc::clone(&transport);
        let _ = task_spawner.spawn(HostTaskOwnerKind::Session, HostTaskKind::ContinuationTurn, async move {
            let Ok(current) = cfg.session_resources.load_session_control(&req.session_id).await else { return };
            if !same_recipient(&current, &req.recipient_control) { return; }
            enqueue_cron_trigger(&req.inbox, &req.trigger);
            if let Err(error) = publish_and_notify(&cfg, &transport, &req.session_id,
                req.recipient_control.lifecycle, &req.inbox).await {
                warn!(session_id = %req.session_id, %error, "cron publication remains unconfirmed");
            }
        });
    }
}

#[cfg(test)]
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

pub(super) fn enqueue_cron_trigger(queue: &MessageQueue, trigger: &CronTrigger) {
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
            metadata: serde_json::json!({ "task_id": trigger.task_id, "prompt": trigger.prompt }),
        })
        .expect("cron reminder mapping must be valid");
    queue.push(QueuedMessage::system_reminder(
        MessageKind::Defer,
        MessageSource::CronTrigger,
        reminder,
    ));
}

async fn publish_and_notify(
    cfg: &AcpServerConfig,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    session_id: &str,
    lifecycle: u64,
    inbox: &MessageQueue,
) -> anyhow::Result<()> {
    publish_inbox_work(
        cfg.session_resources.clone(),
        transport,
        session_id,
        lifecycle,
        inbox,
        None,
    )
    .await
}

pub(super) async fn publish_inbox_work(
    resources: Arc<dyn peri_acp_types::session_resources::SessionResources>,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    session_id: &str,
    lifecycle: u64,
    inbox: &MessageQueue,
    observer_floor: Option<u64>,
) -> anyhow::Result<()> {
    peri_agent::agent::stages::publish_session_inbox(
        Arc::clone(&resources),
        session_id,
        lifecycle,
        inbox,
    )
    .await?;
    let snapshot = resources
        .inspect_work(&peri_acp_types::session_resources::work::WorkQuery {
            session_id: session_id.into(),
            selector: peri_acp_types::session_resources::work::WorkSelector::Availability,
            limit: 1,
            cursor: None,
        })
        .await?;
    let available = inbox_work_available(&snapshot, lifecycle)?;
    let available = match observer_floor {
        Some(floor) if available => {
            recent_inbox_work_available(resources.as_ref(), session_id, lifecycle, floor).await?
        }
        _ => available,
    };
    if available {
        transport.send_notification("session/work/available", serde_json::json!({
            "sessionId": session_id, "revision": snapshot.head.change_seq,
            "lifecycle": lifecycle, "controlGeneration": snapshot.control.control_generation,
            "executionProtocol": super::execution_admission::EXECUTION_PROTOCOL_VERSION,
        })).await.map_err(|error| anyhow::anyhow!("work availability notification failed: {error:?}"))?;
    }
    Ok(())
}

fn inbox_work_available(
    inspection: &peri_acp_types::session_resources::work::WorkInspection,
    lifecycle: u64,
) -> Result<bool, crate::transport::types::AcpError> {
    let availability = super::work_query::availability(inspection)?;
    Ok(inspection.control.lifecycle == lifecycle
        && availability.lifecycle == lifecycle
        && inspection.head.has_pending_work())
}

async fn recent_inbox_work_available(
    resources: &dyn peri_acp_types::session_resources::SessionResources,
    session_id: &str,
    lifecycle: u64,
    floor: u64,
) -> anyhow::Result<bool> {
    use peri_acp_types::session_resources::work::{WorkPage, WorkQuery, WorkSelector};
    let mut query = WorkQuery::new(session_id, WorkSelector::Inbox);
    query.cursor = Some(format!("{floor:020}:"));
    loop {
        let inspection = resources.inspect_work(&query).await?;
        if inspection.control.lifecycle != lifecycle {
            return Ok(false);
        }
        let WorkPage::Deliveries(deliveries) = inspection.page else {
            anyhow::bail!("inbox observer expected a bounded delivery page");
        };
        if deliveries
            .iter()
            .any(|delivery| observer_delivery_available(delivery, lifecycle, floor))
        {
            return Ok(true);
        }
        let Some(cursor) = inspection.next_cursor else {
            return Ok(false);
        };
        query.cursor = Some(cursor);
    }
}

fn observer_delivery_available(
    delivery: &peri_acp_types::session_resources::work::Delivery,
    lifecycle: u64,
    floor: u64,
) -> bool {
    delivery.recipient_lifecycle == lifecycle
        && delivery.admission_sequence >= floor
        && delivery.obligation == peri_acp_types::session_resources::work::ObligationStatus::Pending
        && delivery.publication.policy.ensures_processing()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_inbox_work_notifications(
    cfg: &AcpServerConfig,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    session_id: String,
    lifecycle: u64,
    observer_floor: u64,
    inbox: MessageQueue,
    cancellation: CancellationToken,
) -> Result<(), ()> {
    let mut changes = inbox.subscribe_wake();
    let resources = cfg.session_resources.clone();
    let transport = transport.clone();
    let shutdown = cfg.host_task_spawner.shutdown_token();
    cfg.host_task_spawner.spawn(
        HostTaskOwnerKind::Session,
        HostTaskKind::InboxWorkNotifications,
        async move {
            loop {
                for attempt in 0..3 {
                    let publication = publish_inbox_work(
                        resources.clone(), &transport, &session_id, lifecycle, &inbox,
                        Some(observer_floor),
                    );
                    let result = tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return,
                        _ = shutdown.cancelled() => return,
                        result = publication => result,
                    };
                    if result.is_ok() { break; }
                    warn!(%session_id, attempt, "inbox work notification remains unconfirmed");
                    tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return,
                        _ = shutdown.cancelled() => return,
                        _ = peri_time::sleep(std::time::Duration::from_millis(100 * (1 << attempt))) => {},
                    }
                }
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => return,
                    _ = shutdown.cancelled() => return,
                    changed = changes.changed() => if changed.is_err() { return; },
                    _ = peri_time::sleep(std::time::Duration::from_secs(2)) => {},
                }
            }
        },
    ).map_err(|_| ())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_continuation_scheduler(
    mut rx: mpsc::UnboundedReceiver<ContinuationRequest>,
    sessions: SharedSessions,
    _prompt_locks: PromptLocks,
    cfg: Arc<AcpServerConfig>,
    transport: Arc<dyn crate::transport::AcpTransport>,
    _cont_tx: std::sync::Weak<mpsc::UnboundedSender<ContinuationRequest>>,
    _task_spawner: HostTaskSpawner,
    shutdown: CancellationToken,
) {
    while let Some(req) = recv_until_shutdown(&mut rx, &shutdown).await {
        if !sessions
            .lock()
            .await
            .get(&req.session_id)
            .is_some_and(|state| !state.closing)
        {
            continue;
        }
        let recipient = cfg
            .session_manager
            .get_session(&req.session_id)
            .map(|session| {
                (
                    session.recipient_lifecycle,
                    session.v2_message_queue.clone(),
                )
            });
        let Some((lifecycle, inbox)) = recipient else {
            continue;
        };
        if let Err(error) =
            publish_and_notify(&cfg, &transport, &req.session_id, lifecycle, &inbox).await
        {
            warn!(session_id = %req.session_id, %error, "continuation publication remains unconfirmed");
        }
    }
}

async fn recv_until_shutdown<T>(
    rx: &mut mpsc::UnboundedReceiver<T>,
    shutdown: &CancellationToken,
) -> Option<T> {
    tokio::select! {
        biased;
        _ = shutdown.cancelled() => None,
        req = rx.recv() => req,
    }
}

#[cfg(test)]
#[path = "continuation_test.rs"]
mod tests;
