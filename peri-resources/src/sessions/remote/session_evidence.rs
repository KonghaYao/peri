//! Guarded completion of migrated remote execution evidence.

use super::session_data::{invalid_input, unsupported_behavior, RemoteSessionData};
use super::sql::StatementSpec;
use peri_acp_types::session_resources::{SessionResourceError, SessionResourceResult};
use peri_acp_types::thread::ThreadId;
use peri_acp_types::workspace::{ResolvedWorkspace, SessionBinding};
use turso_serverless::Value;

impl RemoteSessionData {
    pub(super) async fn read_binding_discovery_snapshot(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Option<String>> {
        if self.schema_version <= 11 {
            return Ok(None);
        }
        let row = self
            .store()
            .await?
            .fetch_row(&StatementSpec::new(
                "SELECT discovery_snapshot FROM session_bindings WHERE thread_id = ?1",
                vec![Value::Text(id.clone())],
            ))
            .await?;
        match row.as_deref().and_then(|row| row.first()) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Text(snapshot)) => Ok(Some(snapshot.clone())),
            _ => Err(super::session_codec::corrupt(
                "invalid remote binding evidence",
            )),
        }
    }

    pub(super) async fn write_complete_legacy_binding_discovery(
        &self,
        id: &ThreadId,
        binding: &SessionBinding,
        workspace: &ResolvedWorkspace,
    ) -> SessionResourceResult<()> {
        if self.schema_version <= 11 {
            return Err(unsupported_behavior(
                "v2 binding evidence requires schema 12",
            ));
        }
        let snapshot = workspace
            .discovery_snapshot
            .as_ref()
            .ok_or_else(|| invalid_input("verified workspace evidence is missing"))?;
        let machine = crate::sessions::machine::current()
            .map_err(|_| invalid_input("machine identity is not initialized"))?;
        let statement = StatementSpec::new(
            "UPDATE session_bindings SET discovery_snapshot = ?1, evidence_origin = 'legacy_last_observation'
             WHERE thread_id = ?2 AND workspace_id = ?3 AND discovery_snapshot IS NULL
             AND evidence_origin = 'legacy_missing' AND EXISTS (
                 SELECT 1 FROM threads t JOIN workspaces w ON w.id = t.workspace_id
                 WHERE t.id = ?2 AND t.workspace_id = ?4 AND w.machine_id = ?5)",
            vec![Value::Text(snapshot.clone()), Value::Text(id.clone()),
                Value::Text(binding.workspace_id.to_string()),
                Value::Text(workspace.workspace_id.to_string()), Value::Text(machine.to_owned())],
        );
        let counts = self
            .commit_effects(
                "complete_legacy_binding_evidence",
                &[
                    id.clone(),
                    binding.workspace_id.to_string(),
                    workspace.workspace_id.to_string(),
                    snapshot.clone(),
                ],
                vec![statement],
                id,
            )
            .await?;
        if !matches!(counts.first(), None | Some(1) | Some(0)) {
            return Err(super::session_codec::corrupt(
                "legacy binding evidence update affected multiple rows",
            ));
        }
        let stored = self.read_binding_discovery_snapshot(id).await?;
        if stored.as_deref() != Some(snapshot) {
            return Err(SessionResourceError::conflict(
                "legacy binding evidence differs from the verified local registration",
            ));
        }
        Ok(())
    }
}
