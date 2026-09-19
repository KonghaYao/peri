//! On-demand compatibility for unbound roots. Listing and history replay do not enter here.

use peri_acp_types::{
    thread::ThreadMeta,
    workspace::{ResolvedWorkspace, WorkspaceError},
};
use std::path::Path;

use super::{decode_frozen_snapshot, encode_frozen_snapshot, AcpError, AcpServerConfig};
use crate::host::workspace::workspace_error;

pub(super) async fn resolve_saved_workspace(
    cfg: &AcpServerConfig,
    meta: &ThreadMeta,
) -> Result<ResolvedWorkspace, AcpError> {
    if meta.parent_thread_id.is_some() {
        return Err(workspace_error(WorkspaceError::ExecutionLeaseRequired));
    }
    if !Path::new(&meta.cwd).is_absolute() {
        return Err(workspace_error(WorkspaceError::Unavailable));
    }
    cfg.controller
        .sessions()
        .resolve_workspace(Path::new(&meta.cwd))
        .await
        .map_err(workspace_error)
}

pub(super) async fn prepare_for_restore(
    cfg: &AcpServerConfig,
    session_id: &str,
    expected_cwd: Option<&str>,
) -> Result<(), AcpError> {
    let store = cfg.controller.sessions();
    let id = session_id.to_owned();
    // Errors and unsupported versions are never interpreted as legacy absence.
    if store
        .load_session_binding(&id)
        .await
        .map_err(workspace_error)?
        .is_some()
    {
        return Ok(());
    }
    let meta = store.load_meta(&id).await.map_err(workspace_error)?;
    let workspace = resolve_saved_workspace(cfg, &meta).await?;
    if let Some(expected) = expected_cwd {
        // 与绑定比对的是「同一目录」，不需要为此再解析登记（那会多跑一轮完整发现）。
        crate::host::workspace::expect_directory(expected, &workspace).await?;
    }
    let snapshot = match store
        .load_frozen_snapshot(&id)
        .await
        .map_err(workspace_error)?
    {
        Some(snapshot) => {
            decode_frozen_snapshot(&snapshot).map_err(workspace_error)?;
            snapshot
        }
        None => {
            // Legacy sessions never captured this state. Freeze from their saved cwd once,
            // as the pre-3.15 compatibility path did; never use the caller's terminal cwd.
            let cwd = workspace.cwd.to_string_lossy().into_owned();
            let frozen = crate::host::assemble::build_legacy_frozen_data(cfg, &cwd)
                .map_err(workspace_error)?;
            encode_frozen_snapshot(&frozen).map_err(workspace_error)?
        }
    };
    // One transaction publishes both pieces, so interruption cannot strand a bound
    // session without its frozen state. Native bound sessions still fail on missing state.
    store
        .adopt_legacy_thread(&id, &meta.cwd, &workspace, &snapshot)
        .await
        .map_err(workspace_error)
}
