//! Store-clock owner CAS for the remote canonical session store.

use peri_acp_types::{
    session_resources::{CloseSettlement, SessionResourceError, SessionResourceResult},
    thread::ThreadId,
    workspace::{
        ExecutionOwnerClaim, ExecutionOwnerToken, ExecutionWorkspaceOwnerRecord,
        PriorExecutionOwner, WorkspaceExecutionDescriptor,
    },
};
use turso_serverless::Value;

use super::super::mutation::RemoteStore;
use super::super::sql::{int_at, text_at, StatementSpec};
use super::RemoteSessionData;

const CLAIM_SQL: &str = "INSERT INTO session_execution_owners
    (root_id, epoch, nonce, expires_at_unix, released)
    SELECT t.id, 1, ?2, CAST(strftime('%s','now') AS INTEGER) + 30, 0
    FROM threads t WHERE t.id = ?1 AND t.parent_thread_id IS NULL
      AND (?4 IS NULL OR EXISTS (SELECT 1 FROM session_execution_owners p
            WHERE p.root_id = t.id AND p.epoch = ?4))
      AND ((?3 = 1 AND EXISTS (SELECT 1 FROM session_close_intents WHERE thread_id = t.id))
        OR (?3 = 0 AND NOT EXISTS (SELECT 1 FROM session_close_intents WHERE thread_id = t.id)))
    ON CONFLICT(root_id) DO UPDATE SET
      epoch = session_execution_owners.epoch + 1,
      nonce = excluded.nonce,
      expires_at_unix = excluded.expires_at_unix,
      released = 0
    WHERE session_execution_owners.epoch = ?4
      AND (session_execution_owners.released = 1
       OR session_execution_owners.expires_at_unix <= CAST(strftime('%s','now') AS INTEGER))";
const READ_SQL: &str = "SELECT epoch, nonce, expires_at_unix, released FROM session_execution_owners WHERE root_id = ?1";
const DESCRIPTOR_SQL: &str = "SELECT o.epoch, d.owner_epoch, d.endpoint, d.key_identity,
    d.agent_generation_id, d.unsupported_async_owners
    FROM session_execution_owners o JOIN session_execution_workspace_descriptors d
      ON d.root_id = o.root_id WHERE o.root_id = ?1";
const BIND_SQL: &str = "INSERT INTO session_execution_workspace_descriptors
    (root_id, owner_epoch, endpoint, key_identity, agent_generation_id, unsupported_async_owners)
    SELECT o.root_id, o.epoch, ?4, ?5, ?6, ?7 FROM session_execution_owners o
    WHERE o.root_id = ?1 AND o.epoch = ?2 AND o.nonce = ?3 AND o.released = 0
      AND o.expires_at_unix > CAST(strftime('%s','now') AS INTEGER)
    ON CONFLICT(root_id) DO UPDATE SET
      owner_epoch = excluded.owner_epoch,
      endpoint = excluded.endpoint, key_identity = excluded.key_identity,
      agent_generation_id = excluded.agent_generation_id,
      unsupported_async_owners = MAX(session_execution_workspace_descriptors.unsupported_async_owners,
                                     excluded.unsupported_async_owners)
    WHERE session_execution_workspace_descriptors.owner_epoch < excluded.owner_epoch
       OR (session_execution_workspace_descriptors.owner_epoch = excluded.owner_epoch
         AND session_execution_workspace_descriptors.endpoint = excluded.endpoint
         AND session_execution_workspace_descriptors.key_identity = excluded.key_identity
         AND session_execution_workspace_descriptors.agent_generation_id = excluded.agent_generation_id)";
const MARK_UNSUPPORTED_SQL: &str = "UPDATE session_execution_workspace_descriptors
    SET unsupported_async_owners = 1 WHERE root_id = ?1 AND owner_epoch = ?2
      AND EXISTS (SELECT 1 FROM session_execution_owners o WHERE o.root_id = ?1
        AND o.epoch = ?2 AND o.nonce = ?3 AND o.released = 0
        AND o.expires_at_unix > CAST(strftime('%s','now') AS INTEGER))";
const SETTLEMENT_SQL: &str = "SELECT epoch, nonce, released,
    CASE WHEN expires_at_unix > CAST(strftime('%s','now') AS INTEGER) THEN 1 ELSE 0 END,
    EXISTS (SELECT 1 FROM session_close_intents WHERE thread_id = ?1)
    FROM session_execution_owners WHERE root_id = ?1";
const RENEW_SQL: &str = "UPDATE session_execution_owners
    SET expires_at_unix = CAST(strftime('%s','now') AS INTEGER) + 30
    WHERE root_id = ?1 AND epoch = ?2 AND nonce = ?3 AND released = 0
      AND expires_at_unix > CAST(strftime('%s','now') AS INTEGER)";
const RELEASE_SQL: &str = "UPDATE session_execution_owners SET released = 1
    WHERE root_id = ?1 AND epoch = ?2 AND nonce = ?3 AND released = 0";
const FINISH_GUARD_SQL: &str = "INSERT INTO peri_op_ledger
    (operation_id, kind, digest, state, receipt, updated_at)
    VALUES (?1, (SELECT CASE WHEN EXISTS (
      SELECT 1 FROM session_execution_owners o JOIN session_close_intents c ON c.thread_id = o.root_id
      WHERE o.root_id = ?2 AND o.epoch = ?3 AND o.nonce = ?4 AND o.released = 0
        AND o.expires_at_unix > CAST(strftime('%s','now') AS INTEGER)
    ) THEN 'finish_close' ELSE NULL END), 'owner', 'applied', NULL, datetime('now'))";

fn owner_conflict() -> SessionResourceError {
    SessionResourceError::conflict("session execution owner is active or has changed")
}

impl RemoteSessionData {
    pub(super) async fn claim_owner(
        &self,
        root: &ThreadId,
        require_closing: bool,
        expected_previous_epoch: Option<i64>,
    ) -> SessionResourceResult<ExecutionOwnerClaim> {
        let nonce = uuid::Uuid::new_v4().to_string();
        let store = self.store().await?;
        let previous = store
            .fetch_row(&StatementSpec::new(
                READ_SQL,
                vec![Value::Text(root.clone())],
            ))
            .await?;
        let previous_epoch = previous
            .as_ref()
            .map(|row| int_at(row, 0).ok_or_else(owner_conflict))
            .transpose()?;
        if require_closing && previous_epoch != expected_previous_epoch {
            return Err(owner_conflict());
        }
        let expected = expected_previous_epoch.or(previous_epoch);
        let prior_unreleased = if previous
            .as_ref()
            .is_some_and(|row| int_at(row, 3) == Some(0))
        {
            let descriptor = store
                .fetch_row(&StatementSpec::new(
                    DESCRIPTOR_SQL,
                    vec![Value::Text(root.clone())],
                ))
                .await?;
            let generation = descriptor
                .as_ref()
                .and_then(|row| text_at(row, 4))
                .filter(|generation| !generation.is_empty())
                .map(str::to_owned);
            Some(PriorExecutionOwner {
                agent_generation_id: generation,
            })
        } else {
            None
        };
        let result = store
            .apply_owner_batch(vec![StatementSpec::new(
                CLAIM_SQL,
                vec![
                    Value::Text(root.clone()),
                    Value::Text(nonce.clone()),
                    Value::Integer(i64::from(require_closing)),
                    expected.map(Value::Integer).unwrap_or(Value::Null),
                ],
            )])
            .await;
        // A lost reply can be resolved by reading the newly minted nonce. A
        // different nonce is never accepted as this caller's generation.
        let row = store
            .fetch_row(&StatementSpec::new(
                READ_SQL,
                vec![Value::Text(root.clone())],
            ))
            .await?;
        let Some(row) = row else {
            return result.map(|_| ()).and(Err(owner_conflict()));
        };
        let epoch = int_at(&row, 0).ok_or_else(owner_conflict)?;
        let persisted_nonce = text_at(&row, 1).ok_or_else(owner_conflict)?;
        let released = int_at(&row, 3).ok_or_else(owner_conflict)?;
        if persisted_nonce != nonce || released != 0 || epoch <= 0 {
            return result.map(|_| ()).and(Err(owner_conflict()));
        }
        Ok(ExecutionOwnerClaim {
            token: ExecutionOwnerToken {
                root_id: root.clone(),
                epoch,
                nonce,
            },
            prior_unreleased,
        })
    }

    pub(super) async fn renew_owner(
        &self,
        token: &ExecutionOwnerToken,
    ) -> SessionResourceResult<()> {
        let store = self.store().await?;
        let counts = store
            .apply_owner_batch(vec![StatementSpec::new(
                RENEW_SQL,
                vec![
                    Value::Text(token.root_id.clone()),
                    Value::Integer(token.epoch),
                    Value::Text(token.nonce.clone()),
                ],
            )])
            .await?;
        if counts.first() != Some(&1) {
            return Err(owner_conflict());
        }
        Ok(())
    }

    pub(super) async fn release_owner(
        &self,
        token: &ExecutionOwnerToken,
    ) -> SessionResourceResult<()> {
        let store = self.store().await?;
        let counts = store
            .apply_owner_batch(vec![StatementSpec::new(
                RELEASE_SQL,
                vec![
                    Value::Text(token.root_id.clone()),
                    Value::Integer(token.epoch),
                    Value::Text(token.nonce.clone()),
                ],
            )])
            .await?;
        if counts.first() != Some(&1) {
            return Err(owner_conflict());
        }
        let mut tokens = self.owner_tokens.lock().unwrap();
        if tokens.get(&token.root_id) == Some(token) {
            tokens.remove(&token.root_id);
        }
        Ok(())
    }

    pub(super) async fn finish_owned_close(
        &self,
        token: &ExecutionOwnerToken,
    ) -> SessionResourceResult<()> {
        let store = self.store().await?;
        let counts = store
            .apply_owner_batch(vec![
                StatementSpec::new(
                    FINISH_GUARD_SQL,
                    vec![
                        Value::Text(uuid::Uuid::new_v4().to_string()),
                        Value::Text(token.root_id.clone()),
                        Value::Integer(token.epoch),
                        Value::Text(token.nonce.clone()),
                    ],
                ),
                StatementSpec::new(
                    "DELETE FROM session_close_intents WHERE thread_id = ?1",
                    vec![Value::Text(token.root_id.clone())],
                ),
                StatementSpec::new(
                    "DELETE FROM session_execution_workspace_descriptors WHERE root_id = ?1",
                    vec![Value::Text(token.root_id.clone())],
                ),
                StatementSpec::new(
                    RELEASE_SQL,
                    vec![
                        Value::Text(token.root_id.clone()),
                        Value::Integer(token.epoch),
                        Value::Text(token.nonce.clone()),
                    ],
                ),
            ])
            .await?;
        if counts.len() != 4 || counts[0] != 1 || counts[1] != 1 || counts[3] != 1 {
            return Err(owner_conflict());
        }
        let mut tokens = self.owner_tokens.lock().unwrap();
        if tokens.get(&token.root_id) == Some(token) {
            tokens.remove(&token.root_id);
        }
        Ok(())
    }

    pub(super) async fn read_close_settlement(
        &self,
        token: &ExecutionOwnerToken,
    ) -> SessionResourceResult<CloseSettlement> {
        self.store().await?.close_settlement(token).await
    }

    pub(super) async fn bind_workspace_owner(
        &self,
        token: &ExecutionOwnerToken,
        descriptor: &WorkspaceExecutionDescriptor,
    ) -> SessionResourceResult<()> {
        if !descriptor.valid_for_store() {
            return Err(SessionResourceError::conflict(
                "workspace owner descriptor is invalid",
            ));
        }
        let store = self.store().await?;
        let counts = store
            .apply_owner_batch(vec![StatementSpec::new(
                BIND_SQL,
                vec![
                    Value::Text(token.root_id.clone()),
                    Value::Integer(token.epoch),
                    Value::Text(token.nonce.clone()),
                    Value::Text(descriptor.endpoint.clone()),
                    Value::Text(descriptor.key_identity.clone()),
                    Value::Text(descriptor.agent_generation_id.clone()),
                    Value::Integer(i64::from(descriptor.unsupported_async_owners)),
                ],
            )])
            .await?;
        if counts.first() != Some(&1) {
            return Err(owner_conflict());
        }
        Ok(())
    }

    pub(super) async fn read_workspace_owner(
        &self,
        root: &ThreadId,
    ) -> SessionResourceResult<Option<ExecutionWorkspaceOwnerRecord>> {
        self.store().await?.workspace_owner(root).await
    }

    pub(super) async fn mark_workspace_owner_unsupported(
        &self,
        token: &ExecutionOwnerToken,
    ) -> SessionResourceResult<()> {
        let store = self.store().await?;
        let counts = store
            .apply_owner_batch(vec![StatementSpec::new(
                MARK_UNSUPPORTED_SQL,
                vec![
                    Value::Text(token.root_id.clone()),
                    Value::Integer(token.epoch),
                    Value::Text(token.nonce.clone()),
                ],
            )])
            .await?;
        if counts.first() != Some(&1) {
            return Err(owner_conflict());
        }
        Ok(())
    }
}

impl RemoteStore {
    pub(in crate::sessions::remote) async fn workspace_owner(
        &self,
        root: &ThreadId,
    ) -> SessionResourceResult<Option<ExecutionWorkspaceOwnerRecord>> {
        let row = self
            .fetch_row(&StatementSpec::new(
                DESCRIPTOR_SQL,
                vec![Value::Text(root.clone())],
            ))
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let current_epoch = int_at(&row, 0).ok_or_else(owner_conflict)?;
        let descriptor_epoch = int_at(&row, 1).ok_or_else(owner_conflict)?;
        let endpoint = text_at(&row, 2).ok_or_else(owner_conflict)?.to_owned();
        let key_identity = text_at(&row, 3).ok_or_else(owner_conflict)?.to_owned();
        let agent_generation_id = text_at(&row, 4).ok_or_else(owner_conflict)?.to_owned();
        let unsupported_async_owners = match int_at(&row, 5) {
            Some(0) => false,
            Some(1) => true,
            _ => return Err(owner_conflict()),
        };
        Ok(Some(ExecutionWorkspaceOwnerRecord {
            current_epoch,
            descriptor_epoch,
            descriptor: WorkspaceExecutionDescriptor {
                endpoint,
                key_identity,
                agent_generation_id,
                unsupported_async_owners,
            },
        }))
    }

    pub(in crate::sessions::remote) async fn close_settlement(
        &self,
        token: &ExecutionOwnerToken,
    ) -> SessionResourceResult<CloseSettlement> {
        let row = self
            .fetch_row(&StatementSpec::new(
                SETTLEMENT_SQL,
                vec![Value::Text(token.root_id.clone())],
            ))
            .await?;
        let Some(row) = row else {
            return Ok(CloseSettlement::ChangedOwner);
        };
        let epoch = int_at(&row, 0).ok_or_else(owner_conflict)?;
        let nonce = text_at(&row, 1).ok_or_else(owner_conflict)?;
        let released = int_at(&row, 2).ok_or_else(owner_conflict)?;
        let live = int_at(&row, 3).ok_or_else(owner_conflict)?;
        let intent = int_at(&row, 4).ok_or_else(owner_conflict)?;
        if epoch != token.epoch || nonce != token.nonce {
            return Ok(CloseSettlement::ChangedOwner);
        }
        match (released, live, intent) {
            (1, _, 0) => Ok(CloseSettlement::Finished),
            (0, 1, 1) => Ok(CloseSettlement::Pending),
            _ => Ok(CloseSettlement::ChangedOwner),
        }
    }
}
