//! Local Machine and Workspace catalog and Session archive operations.

use super::*;

impl SqliteSessionData {
    pub(super) async fn catalog_machines(
        &self,
    ) -> SessionResourceResult<Vec<peri_acp_types::workspace::MachineInfo>> {
        use peri_acp_types::workspace::{MachineIdentityKind, MachineInfo};
        let rows: Vec<(String, String, String)> =
            sqlx::query_as("SELECT id, name, identity_kind FROM machines ORDER BY name, id")
                .fetch_all(&self.database.pool)
                .await
                .map_err(|error| read_failure(error.into()))?;
        let current = crate::sessions::machine::current()
            .map_err(|_| unavailable("machine identity is not initialized"))?;
        rows.into_iter()
            .map(|(id, name, kind)| {
                let identity_kind = match kind.as_str() {
                    "known" => MachineIdentityKind::Known,
                    "legacy_unknown" => MachineIdentityKind::LegacyUnknown,
                    _ => return Err(corrupt("stored machine identity kind is invalid")),
                };
                Ok(MachineInfo {
                    is_current: id == current,
                    id,
                    name,
                    identity_kind,
                })
            })
            .collect()
    }
    pub(super) async fn catalog_workspaces(
        &self,
        machine_id: &str,
    ) -> SessionResourceResult<Vec<peri_acp_types::workspace::WorkspaceInfo>> {
        use peri_acp_types::workspace::{WorkspaceInfo, WorkspacePathSource};
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT id, path, path_source FROM workspaces WHERE machine_id = ?1 ORDER BY path, id",
        )
        .bind(machine_id)
        .fetch_all(&self.database.pool)
        .await
        .map_err(|error| read_failure(error.into()))?;
        rows.into_iter()
            .map(|(id, path, source)| {
                let path_source = match source.as_str() {
                    "discovered" => WorkspacePathSource::Discovered,
                    "derived_legacy" => WorkspacePathSource::DerivedLegacy,
                    "unverified" => WorkspacePathSource::Unverified,
                    _ => return Err(corrupt("stored workspace path source is invalid")),
                };
                Ok(WorkspaceInfo {
                    id: id
                        .parse()
                        .map_err(|_| corrupt("stored workspace id is invalid"))?,
                    machine_id: machine_id.to_owned(),
                    path: path.into(),
                    path_source,
                })
            })
            .collect()
    }
    pub(super) async fn catalog_rename_machine(
        &self,
        machine_id: &str,
        name: &str,
    ) -> SessionResourceResult<()> {
        self.writable()?;
        let name = name.trim();
        if name.is_empty() || name.len() > 200 {
            return Err(invalid_input(
                "machine name must contain 1 to 200 characters",
            ));
        }
        let result = sqlx::query("UPDATE machines SET name = ?1 WHERE id = ?2")
            .bind(name)
            .bind(machine_id)
            .execute(&self.database.pool)
            .await
            .map_err(|error| map_sqlx(&error))?;
        if result.rows_affected() == 0 {
            return Err(not_found());
        }
        Ok(())
    }
    pub(super) async fn catalog_set_session_archived(
        &self,
        id: &ThreadId,
        archived: bool,
    ) -> SessionResourceResult<()> {
        self.writable()?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, id).await?;
        let row: Option<(Option<String>, Option<String>)> =
            sqlx::query_as("SELECT parent_thread_id, frozen_context FROM threads WHERE id = ?1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
        match row {
            None => return Err(not_found()),
            Some((Some(_), _)) => return Err(invalid_input("child sessions cannot be archived")),
            Some((None, None)) => return Err(invalid_input("draft sessions cannot be archived")),
            Some((None, Some(_))) => {}
        }
        sqlx::query("UPDATE threads SET archived = ?1 WHERE id = ?2")
            .bind(archived)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(id.clone())))?;
        Ok(())
    }
}
