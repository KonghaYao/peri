use super::McpClientPool;
use peri_acp_types::event::BackgroundTaskResult;
use peri_acp_types::session::{MessageKind, MessageSource};
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource as CanonicalReminderSource, SystemReminder, TrustedSystemReminderFactory,
    SYSTEM_REMINDER_VERSION,
};
use peri_acp_types::tasks::{BgTaskKind, ExternalTaskRegistration, TaskManager};
use rmcp::model::CancelTaskParams;
use serde_json::json;
use std::sync::Arc;

impl McpClientPool {
    /// Deliver a terminal reminder to the session that initiated the task.
    ///
    /// Delivery is the canonical commit (idempotent by delivery ID); the queue
    /// push only wakes a live initiator. A registered session inbox is preferred
    /// for the wake because it carries the executor wake handle; otherwise the
    /// delivery route lands the reminder in the initiator's own queue.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn deliver_task_reminder(
        &self,
        target_session: &str,
        owner_session: &str,
        server: &str,
        result: &BackgroundTaskResult,
        kind: BgTaskKind,
        delivery_id: peri_acp_types::messages::MessageId,
        delivery: Option<&Arc<dyn peri_acp_types::tasks::TaskTerminalDelivery>>,
    ) -> Result<(), String> {
        let is_shell = kind == BgTaskKind::Shell;
        let source = if is_shell {
            MessageSource::ShellComplete
        } else {
            MessageSource::DynamicMcpNotification
        };
        let reminder = TrustedSystemReminderFactory::for_producer()
            .construct(SystemReminder {
                version: SYSTEM_REMINDER_VERSION,
                category: if is_shell {
                    ReminderCategory::Task
                } else {
                    ReminderCategory::ExternalEvent
                },
                source: CanonicalReminderSource(if is_shell { "shell" } else { "mcp" }.into()),
                kind: if result.success {
                    "completed"
                } else {
                    "failed"
                }
                .into(),
                severity: if result.success {
                    ReminderSeverity::Info
                } else {
                    ReminderSeverity::Error
                },
                delivery: if is_shell {
                    ReminderDelivery::Configurable
                } else {
                    ReminderDelivery::Required
                },
                audiences: ReminderAudiences(vec![
                    ReminderAudience::Model,
                    ReminderAudience::Tui,
                    ReminderAudience::Automation,
                ]),
                body: if is_shell {
                    result.to_notification()
                } else {
                    result.output.clone()
                },
                summary: Some("MCP task completed".into()),
                metadata: json!({
                    "server": server,
                    "task_id": result.task_id,
                    "initiator": target_session,
                    "task_owner": owner_session,
                    "delivery": "initiator",
                }),
            })
            .map_err(|error| error.to_string())?;
        if let Some(delivery) = delivery {
            delivery
                .deliver(delivery_id, &reminder, source.clone())
                .await?;
        }
        if let Some(inbox) = self.session_inboxes.read().get(target_session).cloned() {
            inbox.push_system_reminder_with_delivery_id(
                MessageKind::Defer,
                source,
                reminder,
                delivery_id,
            );
            return Ok(());
        }
        if delivery.is_some() {
            // Canonical commit already landed; the queue wake is best-effort.
            return Ok(());
        }
        Err("session inbox unavailable".to_owned())
    }
    // 外部任务登记事实来自跨层调用，字段固定且按调用顺序直传；与下面的
    // external_task_registration 共用同一组参数，不为此再拆一层结构。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn register_external_task(
        self: &Arc<Self>,
        session_id: &str,
        initiator_session_id: Option<&str>,
        delivery: Option<Arc<dyn peri_acp_types::tasks::TaskTerminalDelivery>>,
        server: &str,
        raw_task_id: &str,
        kind: BgTaskKind,
        summary: &str,
        scoped_workspace: bool,
        started_at: &str,
    ) -> Result<String, String> {
        let (manager, request) = self.external_task_registration(
            session_id,
            initiator_session_id,
            delivery,
            server,
            raw_task_id,
            kind,
            summary,
            scoped_workspace,
            started_at,
        )?;
        manager.register_external(request)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn external_task_registration(
        self: &Arc<Self>,
        session_id: &str,
        initiator_session_id: Option<&str>,
        delivery: Option<Arc<dyn peri_acp_types::tasks::TaskTerminalDelivery>>,
        server: &str,
        raw_task_id: &str,
        kind: BgTaskKind,
        summary: &str,
        scoped_workspace: bool,
        started_at: &str,
    ) -> Result<(Arc<dyn TaskManager>, ExternalTaskRegistration), String> {
        let initiator_session_id = initiator_session_id
            .filter(|initiator| !initiator.is_empty())
            .ok_or_else(|| "Unroutable: external task initiator missing".to_owned())?;
        if initiator_session_id != session_id {
            return Err("Unroutable: task catalog must belong to the initiator".into());
        }
        let manager = self
            .session_tasks
            .read()
            .get(session_id)
            .cloned()
            .ok_or_else(|| "session task manager unavailable".to_owned())?;
        let raw_id_for_cancel = raw_task_id.to_owned();
        let meta = scoped_workspace
            .then(|| self.task_scope_meta_for(server, session_id))
            .flatten();
        if scoped_workspace && meta.is_none() {
            return Err("workspace task scope unavailable".into());
        }
        let weak_for_cancel = Arc::downgrade(self);
        let server_for_cancel = server.to_owned();
        let cancel = Arc::new(move || {
            let weak = weak_for_cancel.clone();
            let server = server_for_cancel.clone();
            let raw_id = raw_id_for_cancel.clone();
            let meta = meta.clone();
            Box::pin(async move {
                let pool = weak
                    .upgrade()
                    .ok_or_else(|| "MCP pool unavailable".to_owned())?;
                let peer = pool
                    .clients
                    .read()
                    .get(&server)
                    .and_then(|client| client.peer.clone())
                    .ok_or_else(|| "MCP task owner disconnected".to_owned())?;
                let mut params = CancelTaskParams::new(raw_id);
                params.meta = meta;
                peer.cancel_task(params)
                    .await
                    .map_err(|error| error.to_string())
            })
                as std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>
        });
        let weak = Arc::downgrade(self);
        let owner_session = session_id.to_owned();
        let target_session = initiator_session_id.to_owned();
        let server_name = server.to_owned();
        let on_terminal = Arc::new(
            move |result: &BackgroundTaskResult,
                  delivery_id: peri_acp_types::messages::MessageId| {
                let result = result.clone();
                let delivery = delivery.clone();
                let target_session = target_session.clone();
                let owner_session = owner_session.clone();
                let server_name = server_name.clone();
                let weak = weak.clone();
                Box::pin(async move {
                    let pool = weak
                        .upgrade()
                        .ok_or_else(|| "MCP pool unavailable".to_owned())?;
                    pool.deliver_task_reminder(
                        &target_session,
                        &owner_session,
                        &server_name,
                        &result,
                        kind,
                        delivery_id,
                        delivery.as_ref(),
                    )
                    .await
                })
                    as std::pin::Pin<
                        Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'static>,
                    >
            },
        );
        let owner_identity = self
            .clients
            .read()
            .get(server)
            .map(|client| format!("mcp:{server}:{:?}", client.source))
            .ok_or_else(|| "MCP task owner unavailable".to_owned())?;
        Ok((
            manager,
            ExternalTaskRegistration {
                session_id: session_id.to_owned(),
                initiator_session_id: Some(initiator_session_id.to_owned()),
                owner_identity,
                owner_task_id: raw_task_id.to_owned(),
                kind,
                summary: summary.to_owned(),
                started_at: Some(started_at.to_owned()),
                cancel,
                on_terminal,
            },
        ))
    }
}
