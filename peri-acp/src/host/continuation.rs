//! Durable inbox publication and SDK work-availability hints; never executes RCRA.

use super::{
    task_scope::{HostTaskKind, HostTaskOwnerKind, HostTaskSpawner},
    AcpServerConfig, PromptLocks, SharedSessions,
};
use crate::session::executor::ContinuationRequest;
#[cfg(test)]
use crate::transport::types::AcpError;
use peri_acp_types::cron::{CronContinuationRequest, CronTrigger};
use peri_acp_types::session::{MessageKind, MessageQueue, MessageSource, QueuedMessage};
use peri_acp_types::session_resources::work::WorkQuery;
use peri_acp_types::session_resources::ControlState;
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
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
    peri_agent::agent::stages::publish_session_inbox(
        Arc::clone(&cfg.session_resources),
        session_id,
        lifecycle,
        inbox,
    )
    .await?;
    let snapshot = cfg
        .session_resources
        .load_session_work(&WorkQuery {
            session_id: session_id.to_owned(),
            limit: 1,
        })
        .await?;
    if snapshot.control.lifecycle != lifecycle {
        return Ok(());
    }
    if snapshot.has_pending_current_work() {
        transport.send_notification("session/work/available", serde_json::json!({
            "sessionId": session_id, "revision": snapshot.state.revision,
            "lifecycle": lifecycle, "controlGeneration": snapshot.control.control_generation,
            "executionProtocol": 1,
        })).await.map_err(|error| anyhow::anyhow!("work availability notification failed: {error:?}"))?;
    }
    Ok(())
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
