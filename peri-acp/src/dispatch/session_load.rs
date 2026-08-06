//! Load session context from ThreadStore (includes ancestor chain snapshots).
//!
//! 存储访问经 [`Controller::sessions`]（ARC-BOUNDARY-001 方向：ACP 不直操
//! `ThreadStore`，统一经 Controller 通道）。

use peri_acp_types::messages::BaseMessage;
use peri_acp_types::thread::ThreadId;
use peri_controller::Controller;

/// Load complete context for a session thread including ancestor snapshots.
///
/// Uses `ThreadStore::load_context` (via [`Controller::sessions`]) which assembles
/// the full message chain (ancestor snapshots + own messages) with materialized
/// caching. Returns an empty `Vec` if the thread does not exist (with a warning log).
pub async fn load_session_messages(controller: &Controller, thread_id: &str) -> Vec<BaseMessage> {
    match controller
        .sessions()
        .load_context(&ThreadId::from(thread_id.to_string()))
        .await
    {
        Ok(msgs) => msgs,
        Err(e) => {
            tracing::warn!(thread_id = %thread_id, error = %e, "session/load: thread not found, returning empty history");
            Vec::new()
        }
    }
}
