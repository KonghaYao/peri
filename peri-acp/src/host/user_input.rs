//! User input control and session event wiring. Scheduling decisions remain in Agent.

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
    fn new(ticket: UserInputRunTicket) -> Self {
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
    let event = mailbox
        .run_started_event(ticket)
        .ok_or_else(|| AcpError::new(-32800, "user input attempt superseded"))?;
    let mut subscriber = cfg.controller.subscribe();
    let (bus, handles) = EventBus::new(EventBusConfig::default());
    bus.emit_state(event);
    drop(bus);
    let controller = Arc::clone(&cfg.controller);
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
                        return TransportEventSink::new(
                            Arc::clone(transport),
                            cfg.session_manager.caps_registry(),
                        )
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

pub(super) fn ensure_mailbox(
    session_id: &str,
    cfg: &AcpServerConfig,
    transport: &Arc<dyn AcpTransport>,
) -> Result<Arc<UserInputMailbox>, AcpError> {
    if let Some(mailbox) = cfg.session_manager.user_input_mailbox_for(session_id) {
        return Ok(mailbox);
    }
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
    let (bus, handles) = EventBus::new(EventBusConfig::default());
    let mailbox = UserInputMailbox::new(
        session_id.to_string(),
        inbox,
        Arc::new(move |event| bus.emit_state(event)),
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
    sessions: &SharedSessions,
    prompt_locks: &PromptLocks,
    cfg: &Arc<AcpServerConfig>,
    transport: &Arc<dyn AcpTransport>,
    cont_tx: &Arc<
        tokio::sync::mpsc::UnboundedSender<crate::session::executor::ContinuationRequest>,
    >,
) {
    let Some(mailbox) = cfg.session_manager.user_input_mailbox_for(session_id) else {
        return;
    };
    let Some(ticket) = mailbox.reserve_run() else {
        return;
    };
    let failed_mailbox = Arc::clone(&mailbox);
    let failed_ticket = ticket.clone();
    let sid = session_id.to_string();
    let sessions = Arc::clone(sessions);
    let prompt_locks = Arc::clone(prompt_locks);
    let transport = Arc::clone(transport);
    let cont_tx = Arc::clone(cont_tx);
    let cfg = Arc::clone(cfg);
    let spawner = cfg.host_task_spawner.clone();
    let result = spawner.spawn(
        task_scope::HostTaskOwnerKind::Session,
        task_scope::HostTaskKind::ContinuationTurn,
        async move {
            let mut next_ticket = Some(ticket);
            while let Some(ticket) = next_ticket {
                // 覆盖等待 prompt lock 期间的任务丢弃；进入 Agent 后由本轮 guard 结算。
                let _reserved_guard = InputAttemptGuard::new(Arc::clone(&mailbox), ticket.clone());
                let run = UserInputRun::new(ticket.clone());
                let params = serde_json::json!({
                    "sessionId": sid,
                    "requestId": ticket.id,
                    "message": { "role": "user", "content": [] },
                });
                let result = super::prompt_dispatch::dispatch_prompt_turn_with_input(
                    params,
                    true,
                    None,
                    &sessions,
                    &prompt_locks,
                    &transport,
                    &cfg,
                    &cont_tx,
                    Some(run.clone()),
                )
                .await;
                if let Err(error) = result {
                    mailbox.fail_reserved(&ticket);
                    if !run.terminal_delivered.load(Ordering::Acquire) {
                        TransportEventSink::new(Arc::clone(&transport), cfg.session_manager.caps_registry())
                            .push_done(&sid, "error", Some(&ticket.id)).await;
                    }
                    tracing::warn!(session_id = %sid, code = error.code, "user input execution failed");
                }
                next_ticket = mailbox.reserve_run();
            }
        },
    );
    if result.is_err() {
        failed_mailbox.fail_reserved(&failed_ticket);
    }
}

pub(super) fn starts_execution(method: &str) -> bool {
    matches!(method, "session/input/enqueue" | "session/input/dispatch")
}
