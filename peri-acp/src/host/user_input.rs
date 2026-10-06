//! User input events report durable Work facts; SDK owns admission and scheduling.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use peri_acp_types::event::{EventSink, ExecutorEvent};
use peri_acp_types::event_v2::{EventBus, EventBusConfig};
use peri_agent::session::user_input_mailbox::{
    UserInputAttemptOutcome, UserInputMailbox, UserInputRunTicket,
};

use super::{task_scope, AcpServerConfig, PromptLocks, SharedSessions};
use crate::session::event_sink::TransportEventSink;
use crate::transport::{types::AcpError, AcpTransport};

#[derive(Clone)]
pub(crate) struct UserInputRun {
    pub(super) ticket: UserInputRunTicket,
    terminal_delivered: Arc<AtomicBool>,
}

impl UserInputRun {
    pub(super) fn new(ticket: UserInputRunTicket) -> Self {
        Self {
            ticket,
            terminal_delivered: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(super) fn mark_terminal_delivered(&self) {
        self.terminal_delivered.store(true, Ordering::Release);
    }
}

pub(super) struct InputAttemptGuard {
    mailbox: Arc<UserInputMailbox>,
    ticket: UserInputRunTicket,
    finished: bool,
}

impl InputAttemptGuard {
    pub(super) fn new(mailbox: Arc<UserInputMailbox>, ticket: UserInputRunTicket) -> Self {
        Self {
            mailbox,
            ticket,
            finished: false,
        }
    }

    pub(super) fn finish(&mut self, result: &crate::session::executor::PromptResult) {
        let outcome = if result.failure.is_some() || result.persistence_inconsistent {
            UserInputAttemptOutcome::Failed
        } else if result.ok {
            UserInputAttemptOutcome::Completed
        } else {
            UserInputAttemptOutcome::Interrupted
        };
        self.mailbox.finish_attempt(&self.ticket, outcome);
        self.finished = true;
    }
}

impl Drop for InputAttemptGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.mailbox.fail_reserved(&self.ticket);
        }
    }
}

pub(super) async fn publish_run_started(
    session_id: &str,
    mailbox: &UserInputMailbox,
    ticket: &UserInputRunTicket,
    cfg: &AcpServerConfig,
    transport: &Arc<dyn AcpTransport>,
) -> Result<(), AcpError> {
    publish_run_started_owned(
        session_id,
        mailbox,
        ticket,
        &cfg.controller,
        cfg.session_manager.caps_registry(),
        transport,
    )
    .await
}

async fn publish_run_started_owned(
    session_id: &str,
    mailbox: &UserInputMailbox,
    ticket: &UserInputRunTicket,
    controller: &Arc<peri_controller::Controller>,
    caps: Arc<dashmap::DashMap<String, peri_acp_types::PeriCaps>>,
    transport: &Arc<dyn AcpTransport>,
) -> Result<(), AcpError> {
    let event = mailbox
        .run_started_event(ticket)
        .ok_or_else(|| AcpError::new(-32800, "user input attempt superseded"))?;
    let mut subscriber = controller.subscribe();
    let (bus, handles) = EventBus::new(EventBusConfig::default());
    bus.emit_state(event);
    drop(bus);
    let controller = Arc::clone(controller);
    let sid = session_id.to_string();
    crate::event::forward_eventbus(
        handles,
        move |source, event| {
            controller.publish_event(&sid, &source, event);
        },
        None,
    )
    .await;
    // The client must open reverse-interaction admission before Agent can request HITL.
    loop {
        match subscriber.try_recv() {
            Ok(Some(message)) if message.envelope.session_id == session_id => {
                if let Some(ExecutorEvent::UserInputRunStarted {
                    generation,
                    request_id,
                }) = message.event
                {
                    if request_id == ticket.id {
                        return TransportEventSink::new(Arc::clone(transport), caps)
                            .push_user_input_started(session_id, generation, request_id)
                            .await;
                    }
                }
            }
            Ok(Some(_)) => {}
            _ => return Err(AcpError::new(-32603, "user input start event unavailable")),
        }
    }
}

pub(super) fn sdk_run_started_publisher(
    session_id: String,
    mailbox: Arc<UserInputMailbox>,
    cfg: &AcpServerConfig,
    transport: Arc<dyn AcpTransport>,
    already_published: Option<String>,
) -> peri_agent::agent::stages::SdkRunStartedFn {
    let controller = Arc::clone(&cfg.controller);
    let caps = cfg.session_manager.caps_registry();
    Arc::new(move |admission| {
        let mailbox = Arc::clone(&mailbox);
        let controller = Arc::clone(&controller);
        let caps = Arc::clone(&caps);
        let transport = Arc::clone(&transport);
        let session_id = session_id.clone();
        let already_published = already_published.clone();
        Box::pin(async move {
            if admission.session_id != session_id {
                return Err("SDK RunStarted recipient conflict".to_owned());
            }
            let ticket = mailbox
                .observe_sdk_run(&admission)
                .await
                .map_err(|error| error.to_string())?;
            if already_published.as_ref() == Some(&ticket.id) {
                return Ok(());
            }
            publish_run_started_owned(
                &session_id,
                &mailbox,
                &ticket,
                &controller,
                caps,
                &transport,
            )
            .await
            .map_err(|error| error.to_string())
        })
    })
}

pub(super) async fn ensure_mailbox(
    session_id: &str,
    cfg: &AcpServerConfig,
    transport: &Arc<dyn AcpTransport>,
) -> Result<Arc<UserInputMailbox>, AcpError> {
    if let Some(mailbox) = cfg.session_manager.user_input_mailbox_for(session_id) {
        return Ok(mailbox);
    }
    let work = cfg
        .session_resources
        .load_session_work(&peri_acp_types::session_resources::work::WorkQuery {
            session_id: session_id.to_owned(),
            limit: 1,
        })
        .await
        .map_err(|_| AcpError::new(-32603, "durable user input store unavailable"))?;
    let inbox = cfg
        .session_manager
        .session_inbox_for(session_id)
        .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
    let mut session = cfg
        .session_manager
        .get_session_mut(session_id)
        .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
    if let Some(mailbox) = &session.user_input_mailbox {
        return Ok(Arc::clone(mailbox));
    }
    super::continuation::spawn_inbox_work_notifications(
        cfg,
        transport,
        session_id.to_owned(),
        work.control.lifecycle,
        session.v2_message_queue.clone(),
        session.user_input_events_cancel.clone(),
    )
    .map_err(|_| AcpError::new(-32800, "session is closing"))?;
    let (bus, handles) = EventBus::new(EventBusConfig::default());
    let mailbox = UserInputMailbox::new_durable(
        session_id.to_string(),
        inbox,
        Arc::new(move |event| bus.emit_state(event)),
        Arc::clone(&cfg.session_resources),
        work.control.lifecycle,
    );
    let controller = Arc::downgrade(&cfg.controller);
    let sid = session_id.to_string();
    let forward_sid = sid.clone();
    let cancellation = session.user_input_events_cancel.clone();
    let shutdown = cfg.host_task_spawner.shutdown_token();
    let mut subscriber = cfg.controller.subscribe();
    let sink = TransportEventSink::new(Arc::clone(transport), cfg.session_manager.caps_registry());
    let forward = crate::event::forward_eventbus(
        handles,
        move |source, event| {
            if let Some(controller) = controller.upgrade() {
                controller.publish_event(&forward_sid, &source, event);
            }
        },
        None,
    );
    let generation = mailbox.generation().to_owned();
    cfg.host_task_spawner
        .spawn(
            task_scope::HostTaskOwnerKind::Session,
            task_scope::HostTaskKind::UserInputEvents,
            async move {
                let delivery = async move {
                    loop {
                        match subscriber.recv().await {
                            Ok(message) if message.envelope.session_id == sid => {
                                if let Some(ExecutorEvent::UserInputQueueChanged(snapshot)) =
                                    message.event
                                {
                                    if snapshot.generation == generation {
                                        sink.push_event(
                                            &sid,
                                            &ExecutorEvent::UserInputQueueChanged(snapshot),
                                            0,
                                        )
                                        .await;
                                    }
                                }
                            }
                            Ok(_) => {}
                            Err(peri_controller::SubscriptionError::Lagged(skipped)) => {
                                tracing::warn!(skipped, "user input queue subscription lagged");
                            }
                            Err(peri_controller::SubscriptionError::Closed) => break,
                        }
                    }
                };
                tokio::select! {
                    _ = cancellation.cancelled() => {}
                    _ = shutdown.cancelled() => {}
                    _ = async { tokio::join!(forward, delivery); } => {}
                }
            },
        )
        .map_err(|_| AcpError::new(-32800, "session is closing"))?;
    session.user_input_mailbox = Some(Arc::clone(&mailbox));
    Ok(mailbox)
}

pub(super) fn schedule_mailbox(
    session_id: &str,
    _sessions: &SharedSessions,
    _prompt_locks: &PromptLocks,
    cfg: &Arc<AcpServerConfig>,
    transport: &Arc<dyn AcpTransport>,
    _cont_tx: &Arc<
        tokio::sync::mpsc::UnboundedSender<crate::session::executor::ContinuationRequest>,
    >,
) {
    let sid = session_id.to_string();
    let transport = Arc::clone(transport);
    let cfg = Arc::clone(cfg);
    let spawner = cfg.host_task_spawner.clone();
    let _ = spawner.spawn(
        task_scope::HostTaskOwnerKind::Session,
        task_scope::HostTaskKind::ContinuationTurn,
        async move {
            if let Some(mailbox) = cfg.session_manager.user_input_mailbox_for(&sid) {
                if let Err(error) = mailbox.publish_next_durable().await {
                    tracing::warn!(session_id = %sid, %error, "pending input publication unconfirmed");
                    return;
                }
            }
            let query = peri_acp_types::session_resources::work::WorkQuery {
                session_id: sid.clone(),
                limit: 1,
            };
            if let Ok(work) = cfg.session_resources.load_session_work(&query).await {
                if work.has_pending_current_work() {
                    let _ = transport
                        .send_notification(
                            "session/work/available",
                            serde_json::json!({
                                "sessionId": sid,
                                "revision": work.state.revision,
                                "lifecycle": work.control.lifecycle,
                                "controlGeneration": work.control.control_generation,
                                "executionProtocol": 1,
                            }),
                        )
                        .await;
                }
            }
        },
    );
}

pub(super) fn starts_execution(method: &str) -> bool {
    matches!(method, "session/input/enqueue" | "session/input/dispatch")
}
