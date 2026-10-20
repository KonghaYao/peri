use std::sync::{atomic::Ordering, Arc};

use peri_acp_types::session::{MessageQueue, MessageSource};
use peri_acp_types::tasks::BgTaskKind;
use peri_agent::session::async_result_mailbox::AsyncResultMailbox;
use tokio::sync::mpsc;

use super::{task_scope, AcpServerConfig, SessionState, SharedSessions};
use crate::session::executor::ContinuationRequest;
use crate::session::SessionActivation;
use crate::transport::types::AcpError;

pub(super) fn pending_request(
    session_id: &str,
    state: &SessionState,
    queue: &MessageQueue,
    activation: &SessionActivation,
    mailbox: &AsyncResultMailbox,
) -> Option<ContinuationRequest> {
    if state.closing || state.cancel_token.is_some() || state.continuation_in_flight {
        return None;
    }
    let (mq_steering, pending_result) = if activation.can_activate(queue) {
        (true, false)
    } else if state.continuation_armed && queue.has_pending_defer(&MessageSource::SubAgentComplete)
    {
        (false, false)
    } else if activation.can_activate_result(mailbox) {
        // 通知已出队并写入历史，但主模型尚未处理：即使队列没有新工作，也构成
        // 一次允许的自动尝试。Blocked / Suspended 不在候选内。
        (false, true)
    } else {
        return None;
    };
    Some(ContinuationRequest {
        session_id: session_id.to_owned(),
        kind: BgTaskKind::Agent,
        mq_steering,
        pending_result,
    })
}

pub(super) fn ensure_listener(
    session_id: &str,
    sessions: &SharedSessions,
    cfg: &AcpServerConfig,
    sender: &mpsc::UnboundedSender<ContinuationRequest>,
) -> Result<(), AcpError> {
    let runtime = cfg
        .session_manager
        .get_session(session_id)
        .ok_or_else(|| AcpError::new(-32602, "session runtime not found"))?;
    let activation = Arc::clone(&runtime.activation);
    if activation.listener_started.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    let queue = runtime.v2_message_queue.clone();
    let cancellation = runtime.cancel_token.clone();
    let mailbox = Arc::clone(&runtime.async_result_mailbox);
    drop(runtime);
    let mut wake = queue.subscribe_wake();
    let mut result_changed = mailbox.subscribe_state();
    let shutdown = cfg.host_task_spawner.shutdown_token();
    let manager = cfg.session_manager.clone();
    let sessions = Arc::clone(sessions);
    let session_id = session_id.to_owned();
    let sender = sender.clone();
    let listener_activation = Arc::clone(&activation);
    let result = cfg.host_task_spawner.spawn(
        task_scope::HostTaskOwnerKind::Session,
        task_scope::HostTaskKind::SessionActivation,
        async move {
            loop {
                if cancellation.is_cancelled() || shutdown.is_cancelled() {
                    return;
                }
                wake.borrow_and_update();
                result_changed.borrow_and_update();
                let request = {
                    let sessions = tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return,
                        _ = shutdown.cancelled() => return,
                        sessions = sessions.lock() => sessions,
                    };
                    let Some(state) = sessions.get(&session_id) else {
                        return;
                    };
                    let Some(runtime) = manager.get_session(&session_id) else {
                        return;
                    };
                    if !Arc::ptr_eq(&runtime.activation, &listener_activation) {
                        return;
                    }
                    pending_request(
                        &session_id,
                        state,
                        &queue,
                        &runtime.activation,
                        &runtime.async_result_mailbox,
                    )
                };
                if let Some(request) = request {
                    if sender.send(request).is_err() {
                        tracing::error!(%session_id, "session activation channel closed");
                        return;
                    }
                }
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => return,
                    _ = shutdown.cancelled() => return,
                    changed = wake.changed() => {
                        if changed.is_err() { return; }
                    }
                    changed = result_changed.changed() => {
                        if changed.is_err() { return; }
                    }
                }
            }
        },
    );
    if let Err(error) = result {
        activation.listener_started.store(false, Ordering::Release);
        return Err(AcpError::new(
            -32010,
            format!("session activation unavailable: {error:?}"),
        ));
    }
    Ok(())
}

pub(super) fn mq_allowed(cfg: &AcpServerConfig, session_id: &str) -> bool {
    cfg.session_manager
        .get_session(session_id)
        .is_some_and(|runtime| {
            !runtime.cancel_token.is_cancelled()
                && runtime.activation.can_activate(&runtime.v2_message_queue)
        })
}

/// Ready 结果是否仍允许自动尝试（Stop 抑制与失败水位在 owner 内统一判定）。
pub(super) fn result_allowed(cfg: &AcpServerConfig, session_id: &str) -> bool {
    cfg.session_manager
        .get_session(session_id)
        .is_some_and(|runtime| {
            !runtime.cancel_token.is_cancelled()
                && runtime
                    .activation
                    .can_activate_result(&runtime.async_result_mailbox)
        })
}
