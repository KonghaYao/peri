//! Session transitions and queue-side load reservations under the lifecycle gate.

mod admission;
mod close;
mod delete;
mod guards;
mod reservation;
mod startup;
mod transition;

pub(crate) use admission::AdmissionGuard;
pub(in crate::acp_client::client) use close::PendingClose;

use super::AcpTuiClient;

#[cfg(test)]
use tokio::sync::mpsc;

#[cfg(test)]
#[path = "recovery_test.rs"]
mod recovery_tests;

#[cfg(test)]
#[path = "history_v2_test.rs"]
mod history_v2_tests;

#[cfg(test)]
#[path = "history_v2_session_test.rs"]
mod history_v2_session_tests;

impl AcpTuiClient {
    /// Check whether a session has been created.
    pub fn has_session(&self) -> bool {
        self.lifecycle.has_session()
    }

    /// Get the current session ID, if any.
    pub fn current_session_id(&self) -> Option<String> {
        self.lifecycle.current_session_id()
    }

    #[cfg(test)]
    pub(super) fn install_transition_commit_hook(
        &self,
        tx: mpsc::UnboundedSender<tokio::sync::oneshot::Sender<()>>,
    ) {
        *self.transition_commit_hook.lock().unwrap() = Some(tx);
    }

    #[cfg(test)]
    pub(crate) fn install_transition_claim_hook(
        &self,
        tx: std::sync::mpsc::Sender<std::sync::mpsc::Sender<()>>,
    ) {
        *self.transition_claim_hook.lock().unwrap() = Some(tx);
    }

    /// 测试断点：此时预约已认领、路由尚未切换。同步阻塞是刻意的——调用方
    /// 仍持有预约锁，准入快照必须等待这次切换完成。
    #[cfg(test)]
    fn pause_before_session_route_change(&self) {
        let tx = self.transition_claim_hook.lock().unwrap().clone();
        if let Some(tx) = tx {
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            if tx.send(release_tx).is_ok() {
                // 测试挂死比断言失败更难排查：断点自带上限。
                let _ = release_rx.recv_timeout(std::time::Duration::from_secs(5));
            }
        }
    }

    #[cfg(test)]
    async fn pause_before_transition_commit(&self) {
        let tx = self.transition_commit_hook.lock().unwrap().clone();
        if let Some(tx) = tx {
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            if tx.send(release_tx).is_ok() {
                let _ = release_rx.await;
            }
        }
    }
}
