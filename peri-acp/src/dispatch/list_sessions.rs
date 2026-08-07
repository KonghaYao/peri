//! List sessions via Controller 存储通道，返回 ACP [`SessionInfo`] entries。

use agent_client_protocol_schema::v1::{SessionId, SessionInfo};
use anyhow::{Context, Result};
use peri_controller::Controller;

/// Query all sessions from persistent storage, convert to ACP
/// [`SessionInfo`] entries, and optionally filter by `cwd`.
///
/// 存储访问经 [`Controller::sessions`]（ARC-BOUNDARY-001 方向）。
pub async fn list_sessions_as_info(
    controller: &Controller,
    cwd_filter: Option<&str>,
) -> Result<Vec<SessionInfo>> {
    let threads = controller
        .sessions()
        .list_threads()
        .await
        .context("Failed to list sessions")?;
    Ok(threads
        .into_iter()
        .filter(|t| {
            if let Some(cwd) = cwd_filter {
                t.cwd == cwd
            } else {
                true
            }
        })
        .map(|t| {
            SessionInfo::new(
                SessionId::new(t.id.as_str()),
                std::path::PathBuf::from(&t.cwd),
            )
            .title(t.title)
            .updated_at(t.updated_at.to_rfc3339())
        })
        .collect())
}
