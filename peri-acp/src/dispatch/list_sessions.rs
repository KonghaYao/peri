//! List sessions via [`ThreadStore`], returning ACP [`SessionInfo`] entries.

use agent_client_protocol_schema::{SessionId, SessionInfo};
use anyhow::{Context, Result};
use peri_agent::thread::ThreadStore;

/// Query all sessions from persistent storage, convert to ACP
/// [`SessionInfo`] entries, and optionally filter by `cwd`.
pub async fn list_sessions_as_info(
    thread_store: &dyn ThreadStore,
    cwd_filter: Option<&str>,
) -> Result<Vec<SessionInfo>> {
    let threads = thread_store
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
