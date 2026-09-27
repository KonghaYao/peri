//! List sessions via 会话资源门面，返回 ACP [`SessionInfo`] entries。

use agent_client_protocol_schema::v1::{SessionId, SessionInfo};
use anyhow::{Context, Result};
use peri_acp_types::workspace::{ScopedThreadQuery, ThreadScope};
use peri_controller::Controller;

/// Query all sessions from persistent storage, convert to ACP
/// [`SessionInfo`] entries, and optionally filter by `cwd`.
///
/// 存储访问经 [`Controller::sessions`]（ARC-BOUNDARY-001 方向）；
/// 列表是轻量 metadata 投影，不加载历史。
pub async fn list_sessions_as_info(
    controller: &Controller,
    cwd_filter: Option<&str>,
) -> Result<Vec<SessionInfo>> {
    let resources = controller.sessions();
    let page = resources
        .list_sessions(&ScopedThreadQuery {
            scope: ThreadScope::All,
            cursor: None,
            // 全量列举：现有 `session/list` 语义无分页，游标由调用方决定。
            limit: u32::MAX,
        })
        .await
        .context("Failed to list sessions")?;
    Ok(page
        .entries
        .into_iter()
        .map(|entry| entry.thread)
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
