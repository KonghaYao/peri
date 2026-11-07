//! Ordinary notification routing and new-session buffering.

use crate::acp_client::client::AcpNotification;

use super::{
    BufferedOrdinaryNotification, InteractionLifecycle, InteractionLifecycleState,
    NEW_SESSION_EVENT_BUFFER_CAPACITY, OrdinaryDecision, SessionRoute, TransitionKind,
};

impl InteractionLifecycle {
    /// 原子取出一批待投递的普通通知。
    ///
    /// 非空批次返回后 pending 仍为真：调用方投递这批通知期间到达的新通知会继续
    /// 入队，下一批因此保持 wire 顺序。只有再次加锁发现队列为空时，才在同一临界区
    /// 清除 pending 并返回 `None`；此后通知恢复即时 Forward。
    pub fn take_buffered_batch(&self) -> Option<Vec<AcpNotification>> {
        let mut state = self.state.lock().unwrap();
        if !state.new_flush_pending {
            return None;
        }
        let SessionRoute::Stable { session_id, .. } = &state.route else {
            state.new_buffer.clear();
            state.new_flush_pending = false;
            return None;
        };
        let session_id = session_id.clone();
        let batch: Vec<AcpNotification> = state
            .new_buffer
            .drain(..)
            .filter(|item| item.session_id == session_id)
            .map(|item| item.notification)
            .collect();
        if batch.is_empty() {
            state.new_flush_pending = false;
            None
        } else {
            Some(batch)
        }
    }

    pub fn route_ordinary(
        &self,
        session_id: String,
        notification: AcpNotification,
    ) -> OrdinaryDecision {
        if session_id.is_empty() {
            return OrdinaryDecision::Forward(Box::new(notification));
        }
        let mut state = self.state.lock().unwrap();
        if state.deleted_session_ids.contains(&session_id) {
            return OrdinaryDecision::Drop;
        }
        match &state.route {
            SessionRoute::Stable {
                session_id: current,
                ..
            } if current == &session_id => {
                if !state.new_flush_pending {
                    OrdinaryDecision::Forward(Box::new(notification))
                } else {
                    buffer_ordinary(&mut state, session_id, notification);
                    OrdinaryDecision::Buffered
                }
            }
            SessionRoute::Transitioning {
                kind: TransitionKind::Load,
                target: Some(target),
                ..
            } if target == &session_id => OrdinaryDecision::Forward(Box::new(notification)),
            SessionRoute::Transitioning {
                kind: TransitionKind::New,
                target: None,
                ..
            } => {
                buffer_ordinary(&mut state, session_id, notification);
                OrdinaryDecision::Buffered
            }
            _ => OrdinaryDecision::Drop,
        }
    }
}

fn buffer_ordinary(
    state: &mut InteractionLifecycleState,
    session_id: String,
    notification: AcpNotification,
) {
    if state.new_buffer.len() < NEW_SESSION_EVENT_BUFFER_CAPACITY {
        state.new_buffer.push_back(BufferedOrdinaryNotification {
            session_id,
            notification,
        });
    } else if !state.warned_buffer_full {
        state.warned_buffer_full = true;
        tracing::warn!(
            capacity = NEW_SESSION_EVENT_BUFFER_CAPACITY,
            "ACP client: new-session ordinary notification buffer full"
        );
    }
}
