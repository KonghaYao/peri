//! Remote machine and workspace catalog operations.

use super::session_data::{invalid_input, not_found, unsupported_behavior, RemoteSessionData};
use super::sql::StatementSpec;
use peri_acp_types::session_resources::SessionResourceResult;
use turso_serverless::Value;

impl RemoteSessionData {
    pub(super) async fn catalog_machines(
        &self,
    ) -> SessionResourceResult<Vec<peri_acp_types::workspace::MachineInfo>> {
        if self.schema_version <= 11 {
            return Err(unsupported_behavior("machine catalog requires schema 12"));
        }
        use peri_acp_types::workspace::{MachineIdentityKind, MachineInfo};
        let store = self.store().await?;
        let rows = store
            .fetch_rows(&StatementSpec::bare(
                "SELECT id, name, identity_kind FROM machines ORDER BY name, id",
            ))
            .await?;
        let current = crate::sessions::machine::current().ok();
        rows.into_iter()
            .map(|row| {
                let id = super::sql::text_at(&row, 0)
                    .ok_or_else(|| super::session_codec::corrupt("invalid machine id"))?
                    .to_owned();
                let name = super::sql::text_at(&row, 1)
                    .ok_or_else(|| super::session_codec::corrupt("invalid machine name"))?
                    .to_owned();
                let identity_kind = match super::sql::text_at(&row, 2) {
                    Some("known") => MachineIdentityKind::Known,
                    Some("legacy_unknown") => MachineIdentityKind::LegacyUnknown,
                    _ => {
                        return Err(super::session_codec::corrupt(
                            "invalid machine identity kind",
                        ))
                    }
                };
                Ok(MachineInfo {
                    is_current: Some(id.as_str()) == current,
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
        if self.schema_version <= 11 {
            return Err(unsupported_behavior("workspace catalog requires schema 12"));
        }
        use peri_acp_types::workspace::{WorkspaceInfo, WorkspacePathSource};
        let store = self.store().await?;
        let rows = store.fetch_rows(&StatementSpec::new(
            "SELECT id, path, path_source FROM workspaces WHERE machine_id = ?1 ORDER BY path, id",
            vec![Value::Text(machine_id.to_owned())],
        )).await?;
        rows.into_iter()
            .map(|row| {
                let id = super::sql::text_at(&row, 0)
                    .ok_or_else(|| super::session_codec::corrupt("invalid workspace id"))?
                    .parse()
                    .map_err(|_| super::session_codec::corrupt("invalid workspace id"))?;
                let path = super::sql::text_at(&row, 1)
                    .ok_or_else(|| super::session_codec::corrupt("invalid workspace path"))?
                    .into();
                let path_source = match super::sql::text_at(&row, 2) {
                    Some("discovered") => WorkspacePathSource::Discovered,
                    Some("derived_legacy") => WorkspacePathSource::DerivedLegacy,
                    Some("unverified") => WorkspacePathSource::Unverified,
                    _ => {
                        return Err(super::session_codec::corrupt(
                            "invalid workspace path source",
                        ))
                    }
                };
                Ok(WorkspaceInfo {
                    id,
                    machine_id: machine_id.to_owned(),
                    path,
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
        if self.schema_version <= 11 {
            return Err(unsupported_behavior("machine rename requires schema 12"));
        }
        let name = name.trim();
        if name.is_empty() || name.len() > 200 {
            return Err(invalid_input(
                "machine name must contain 1 to 200 characters",
            ));
        }
        let counts = self
            .commit_effects(
                "rename_machine",
                &[machine_id.to_owned(), name.to_owned()],
                vec![StatementSpec::new(
                    "UPDATE machines SET name = ?1 WHERE id = ?2",
                    vec![
                        Value::Text(name.to_owned()),
                        Value::Text(machine_id.to_owned()),
                    ],
                )],
                &machine_id.to_owned(),
            )
            .await?;
        match counts.first() {
            None | Some(1) => Ok(()),
            Some(0) => Err(not_found()),
            _ => Err(super::session_codec::corrupt(
                "machine rename affected multiple rows",
            )),
        }
    }
}
