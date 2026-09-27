//! 会话历史回放读取（含继承区）。
//!
//! 存储访问经 [`Controller::sessions`]（ARC-BOUNDARY-001 方向：ACP 不直操存储，
//! 统一经 Controller 通道），拿到的是完整逻辑上下文：继承区在前、自有 payload 在后。

use crate::transport::types::AcpError;
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::thread::ThreadId;
use peri_controller::Controller;

/// Load complete context for a session thread including ancestor snapshots.
///
/// Uses [`SessionResources::load_session_history`] (via [`Controller::sessions`]) which
/// assembles the full message chain (inherited region + own messages) without materializing
/// a derived cache. Missing sessions surface the storage failure as an internal error.
///
/// [`SessionResources::load_session_history`]: peri_acp_types::session_resources::SessionResources::load_session_history
pub async fn load_session_payloads(
    controller: &Controller,
    thread_id: &str,
) -> Result<Vec<PersistedPayload>, AcpError> {
    controller
        .sessions()
        .load_session_history(&ThreadId::from(thread_id.to_string()))
        .await
        .map_err(|error| AcpError::new(-32603, format!("session history load failed: {error}")))
}
