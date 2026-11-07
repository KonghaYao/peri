//! UI-visible and lifecycle guards for session transitions.

use tokio::sync::watch;

use crate::acp_client::client::{AcpTuiClient, ClientProjectionMode};

pub(super) struct StartupRestoreGuard<'a>(pub(super) &'a watch::Sender<bool>);

impl Drop for StartupRestoreGuard<'_> {
    fn drop(&mut self) {
        self.0.send_replace(false);
    }
}

/// 建会话期间的用户可见状态：进入时置位 `SESSION_PREPARING`，离开时清除。
///
/// 用 `Drop` 而不是在每个返回分支写清除：`session/new` 的 future 会被直接丢弃
/// （准备超时、应用关闭、取消），逐分支清理漏掉取消路径会把状态栏留在提示上。
pub(super) struct PreparingSessionGuard;

impl PreparingSessionGuard {
    pub(super) fn enter() -> Self {
        crate::kit::atoms::SESSION_PREPARING.set(true);
        Self
    }
}

impl Drop for PreparingSessionGuard {
    fn drop(&mut self) {
        crate::kit::atoms::SESSION_PREPARING.set(false);
    }
}

/// A dropped transition must also retire the UI target used for load replay.
/// This guard drops while the operation gate is still held.
pub(super) struct SessionProjectionGuard<'a> {
    pub(super) client: &'a AcpTuiClient,
    pub(super) committed: bool,
}

impl Drop for SessionProjectionGuard<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        self.client.project_execution_cwd(None);
        if self.client.projection_mode == ClientProjectionMode::Interactive
            && !crate::kit::atoms::ACTIVE_SESSION_ID
                .state()
                .read()
                .is_empty()
        {
            crate::kit::session_boundary::project_session_boundary(None);
        }
    }
}
