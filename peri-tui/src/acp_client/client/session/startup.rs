//! Startup restore reservation and its retry target.

use peri_acp::transport::types::AcpError;

use crate::acp_client::client::AcpTuiClient;
use crate::acp_client::interaction_lifecycle::TransitionOperation;

use super::guards::StartupRestoreGuard;

impl AcpTuiClient {
    /// 启动恢复失败后保留的目标；存在时下一次 ensure 先重试。
    pub(super) fn startup_retry_target(&self) -> Option<String> {
        self.startup_restore_target.lock().unwrap().clone()
    }

    pub(super) fn clear_startup_restore_target(&self) {
        *self.startup_restore_target.lock().unwrap() = None;
    }

    /// Establish resume/continue ownership before submit consumers can choose a
    /// fresh session. The reservation mutation shares the lifecycle operation
    /// gate with `ensure_session`, `new_session`, and `load_session`.
    pub async fn reserve_startup_restore(&self) {
        let _operation = self.lifecycle.operation_gate().lock().await;
        self.startup_restore_tx.send_replace(true);
    }

    /// Resolve a startup restore reservation by loading its selected target.
    /// Waiters are released only after load commits Stable (or fails closed).
    pub async fn load_startup_session(
        &self,
        session_id: &str,
        cwd: &str,
        model: Option<&str>,
    ) -> Result<String, AcpError> {
        let _operation = self.lifecycle.operation_gate().lock().await;
        // 启动恢复失败后仍保留目标：下一次 ensure 先重试同一会话，不静默新建。
        *self.startup_restore_target.lock().unwrap() = Some(session_id.to_owned());
        let reservation = StartupRestoreGuard(&self.startup_restore_tx);
        let result = self
            .load_session_under_gate(
                session_id,
                cwd,
                model,
                None,
                TransitionOperation::AdoptPending,
            )
            .await;
        drop(reservation);
        result
    }

    /// Release a startup reservation when resume lookup cannot select a target.
    pub async fn release_startup_restore(&self) {
        let _operation = self.lifecycle.operation_gate().lock().await;
        self.clear_startup_restore_target();
        self.startup_restore_tx.send_replace(false);
    }
}
