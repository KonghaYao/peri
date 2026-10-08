//! Machine/path 归属键：同机同路径只有一个 Workspace，归属行的执行证据由写路径刷新。

use anyhow::{bail, Result};
use peri_acp_types::workspace::{WorkspaceError, WorkspaceId};
use sqlx::SqliteConnection;

use super::discovery::Discovery;

pub(super) async fn ensure_current_machine(connection: &mut SqliteConnection) -> Result<String> {
    let machine = crate::sessions::machine::current()?.to_owned();
    sqlx::query(
        "INSERT OR IGNORE INTO machines(id, name, identity_kind) VALUES (?1, '我的电脑', 'known')",
    )
    .bind(&machine)
    .execute(&mut *connection)
    .await?;
    Ok(machine)
}

/// 同机同 path 只能有一个 ID。发现信息只允许从 unverified 升级，不重写归属。
pub(super) async fn resolve_identity(
    connection: &mut SqliteConnection,
    machine: &str,
    path: &str,
    source: &str,
) -> Result<WorkspaceId> {
    if !std::path::Path::new(path).is_absolute() {
        bail!("workspace path must be absolute");
    }
    let id = WorkspaceId::new();
    sqlx::query(
        "INSERT INTO workspaces(id, machine_id, path, path_source) VALUES (?1, ?2, ?3, ?4)
        ON CONFLICT(machine_id, path) DO NOTHING",
    )
    .bind(id.to_string())
    .bind(machine)
    .bind(path)
    .bind(source)
    .execute(&mut *connection)
    .await?;
    if source == "discovered" {
        sqlx::query("UPDATE workspaces SET path_source = 'discovered' WHERE machine_id = ?1 AND path = ?2 AND path_source = 'unverified'")
            .bind(machine)
            .bind(path)
            .execute(&mut *connection)
            .await?;
    }
    let (winner,): (String,) =
        sqlx::query_as("SELECT id FROM workspaces WHERE machine_id = ?1 AND path = ?2")
            .bind(machine)
            .bind(path)
            .fetch_one(&mut *connection)
            .await?;
    Ok(winner.parse()?)
}

pub(super) async fn identity_for_new_thread(
    connection: &mut SqliteConnection,
    parent_id: Option<&str>,
    owner_workspace_id: Option<&WorkspaceId>,
    saved_cwd: &str,
) -> Result<WorkspaceId> {
    if let Some(parent_id) = parent_id {
        let (owner,): (String,) = sqlx::query_as("SELECT workspace_id FROM threads WHERE id = ?1")
            .bind(parent_id)
            .fetch_one(&mut *connection)
            .await?;
        return Ok(owner.parse()?);
    }
    let machine = ensure_current_machine(connection).await?;
    if let Some(owner_workspace_id) = owner_workspace_id {
        // 调用方手上的归属 id 必须能读回路径与执行证据：路径决定归属键，证据决定
        // 这条路径是「已验证的 Git 根」还是「未验证目录」。
        let row: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT path, discovery FROM workspaces WHERE id = ?1")
                .bind(owner_workspace_id.to_string())
                .fetch_optional(&mut *connection)
                .await?;
        let (path, snapshot) = row.ok_or(WorkspaceError::InvalidBinding)?;
        let discovery: Discovery =
            serde_json::from_str(snapshot.as_deref().ok_or(WorkspaceError::InvalidBinding)?)
                .map_err(|_| WorkspaceError::InvalidBinding)?;
        if discovery.root != std::path::Path::new(&path) {
            bail!("workspace evidence root differs from its path");
        }
        let source = if discovery.common_dir.is_some() || discovery.private_dir.is_some() {
            "discovered"
        } else {
            "unverified"
        };
        resolve_identity(connection, &machine, &path, source).await
    } else {
        resolve_identity(connection, &machine, saved_cwd, "unverified").await
    }
}
