//! v2 Machine/path 归属键。执行登记保存在另一张表，二者不能混用。

use anyhow::{bail, Result};
use peri_acp_types::workspace::WorkspaceId;
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
    execution_registration_id: Option<&WorkspaceId>,
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
    if let Some(execution_registration_id) = execution_registration_id {
        let row: (String, String) = sqlx::query_as(
            "SELECT root, discovery FROM legacy_execution_registrations WHERE id = ?1",
        )
        .bind(execution_registration_id.to_string())
        .fetch_one(&mut *connection)
        .await?;
        let discovery: Discovery = serde_json::from_str(&row.1)?;
        if discovery.root != std::path::Path::new(&row.0) {
            bail!("execution registration root differs from snapshot");
        }
        let source = if discovery.common_dir.is_some() || discovery.private_dir.is_some() {
            "discovered"
        } else {
            "unverified"
        };
        resolve_identity(connection, &machine, &row.0, source).await
    } else {
        resolve_identity(connection, &machine, saved_cwd, "unverified").await
    }
}
