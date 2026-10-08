use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use peri_acp_types::event::EventSink;
use serde_json::Value;

use super::AcpServerConfig;
use crate::session::event_sink::TransportEventSink;
use crate::transport::{types::AcpError, AcpTransport};

pub(super) async fn approve_schedule(
    request: &peri_acp_types::cron::CronContinuationRequest,
    sessions: &super::SharedSessions,
    locks: &super::PromptLocks,
    cfg: &AcpServerConfig,
    transport: &Arc<dyn AcpTransport>,
) -> Result<bool, AcpError> {
    let permission =
        super::continuation::scheduled_permission_mode(cfg, sessions, &request.session_id).await?;
    let lock = {
        let mut locks = locks.lock().await;
        Arc::clone(
            locks
                .entry(request.session_id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    };
    let _guard = lock.lock().await;
    super::workspace::validate_expected(cfg, &request.session_id, None).await?;
    let runtime = cfg
        .session_manager
        .get_session(&request.session_id)
        .ok_or_else(|| AcpError::new(-32602, "scheduled session runtime missing"))?;
    if runtime.cancel_token.is_cancelled()
        || !runtime
            .v2_message_queue
            .subscribe_wake()
            .same_channel(&request.inbox.subscribe_wake())
    {
        return Err(AcpError::new(
            -32800,
            "scheduled session runtime superseded",
        ));
    }
    let runtime_cancel = runtime.cancel_token.clone();
    drop(runtime);
    let mailbox = super::user_input::ensure_mailbox(&request.session_id, cfg, transport)?;
    let mut params = serde_json::json!({});
    let notifications = ExecutionNotifications::from_params(&mut params)?;
    let cancel = tokio_util::sync::CancellationToken::new();
    {
        let mut sessions = sessions.lock().await;
        let state = sessions
            .get_mut(&request.session_id)
            .ok_or_else(|| AcpError::new(-32602, "scheduled session missing"))?;
        if state.closing {
            return Err(AcpError::new(-32800, "scheduled session is closing"));
        }
        state.cancel_token = Some(cancel.clone());
        state.continuation_in_flight = true;
    }
    let shutdown = cfg.host_task_spawner.shutdown_token();
    let result = async {
        notifications
            .start(&request.session_id, mailbox.generation(), cfg, transport)
            .await?;
        let broker = super::prompt::build_transport_broker(transport, &request.session_id);
        let approval = super::prompt::approve_scheduled_trigger(
            permission.as_ref(),
            Some(&broker),
            &request.trigger.task_id,
            &request.trigger.prompt,
        );
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Ok(false),
            _ = runtime_cancel.cancelled() => Ok(false),
            _ = shutdown.cancelled() => Ok(false),
            approved = approval => Ok(approved),
        }
    }
    .await;
    let reason =
        if cancel.is_cancelled() || runtime_cancel.is_cancelled() || shutdown.is_cancelled() {
            "cancelled"
        } else if result.is_err() {
            "error"
        } else {
            "end_turn"
        };
    notifications
        .finish_early(&request.session_id, reason, cfg, transport)
        .await;
    {
        let mut sessions = sessions.lock().await;
        if let Some(state) = sessions.get_mut(&request.session_id) {
            state.cancel_token = None;
            state.continuation_in_flight = false;
        }
    }
    result
}

pub(super) struct ExecutionNotifications {
    pub(super) request_id: String,
    started: AtomicBool,
    terminal: AtomicBool,
}

impl ExecutionNotifications {
    pub(super) fn from_params(params: &mut Value) -> Result<Self, AcpError> {
        let params = params
            .as_object_mut()
            .ok_or_else(|| AcpError::new(-32602, "prompt params must be an object"))?;
        let request_id = params
            .get("requestId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
        params.insert("requestId".into(), Value::String(request_id.clone()));
        Ok(Self {
            request_id,
            started: AtomicBool::new(false),
            terminal: AtomicBool::new(false),
        })
    }

    pub(super) async fn start(
        &self,
        session_id: &str,
        generation: &str,
        cfg: &AcpServerConfig,
        transport: &Arc<dyn AcpTransport>,
    ) -> Result<(), AcpError> {
        TransportEventSink::new(Arc::clone(transport), cfg.session_manager.caps_registry())
            .push_execution_started(session_id, generation.to_owned(), self.request_id.clone())
            .await?;
        self.started.store(true, Ordering::Release);
        Ok(())
    }

    pub(super) fn mark_terminal(&self) {
        self.terminal.store(true, Ordering::Release);
    }

    pub(super) async fn finish_early(
        &self,
        session_id: &str,
        reason: &str,
        cfg: &AcpServerConfig,
        transport: &Arc<dyn AcpTransport>,
    ) {
        if self.started.load(Ordering::Acquire) && !self.terminal.swap(true, Ordering::AcqRel) {
            TransportEventSink::new(Arc::clone(transport), cfg.session_manager.caps_registry())
                .push_done(session_id, reason, Some(&self.request_id))
                .await;
        }
    }
}
