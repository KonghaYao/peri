//! Load session context from ThreadStore (includes ancestor chain snapshots).
//!
//! 存储访问经 [`Controller::sessions`]（ARC-BOUNDARY-001 方向：ACP 不直操
//! `ThreadStore`，统一经 Controller 通道）。

use crate::transport::types::AcpError;
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::thread::ThreadId;
use peri_controller::Controller;

/// Load complete context for a session thread including ancestor snapshots.
///
/// Uses `ThreadStore::load_context` (via [`Controller::sessions`]) which assembles
/// the full message chain (ancestor snapshots + own messages) with materialized
/// caching. Returns an empty `Vec` if the thread does not exist (with a warning log).
pub async fn load_session_payloads(
    controller: &Controller,
    thread_id: &str,
) -> Result<Vec<PersistedPayload>, AcpError> {
    controller
        .sessions()
        .load_context_payloads(&ThreadId::from(thread_id.to_string()))
        .await
        .map_err(|error| AcpError::new(-32603, format!("session history load failed: {error}")))
}
