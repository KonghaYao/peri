//! Store-issued execution ownership for the local SQLite adapter.

use super::*;

const OWNER_TTL_SECONDS: i64 = 30;

impl SqliteSessionData {
    pub(super) async fn bind_workspace_owner(
        &self,
        token: &ExecutionOwnerToken,
        descriptor: &WorkspaceExecutionDescriptor,
    ) -> SessionResourceResult<()> {
        self.writable()?;
        if !descriptor.valid_for_store() {
            return Err(invalid_input(
                "workspace execution descriptor is incomplete",
            ));
        }
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|e| map_sqlx(&e))?;
        self.assert_owner(&mut tx, &token.root_id).await?;
        if self.execution_owner_token(&token.root_id).as_ref() != Some(token) {
            return Err(SessionResourceError::conflict(
                "workspace descriptor owner token is stale",
            ));
        }
        let old: Option<(i64, String, String, String, i64)> = sqlx::query_as(
            "SELECT owner_epoch, endpoint, key_identity, agent_generation_id, unsupported_async_owners
             FROM session_execution_workspace_descriptors WHERE root_id = ?1",
        )
        .bind(token.root_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| map_sqlx(&e))?;
        if let Some((epoch, endpoint, key, generation, _unsupported)) = old.as_ref() {
            if *epoch == token.epoch
                && (endpoint != &descriptor.endpoint
                    || key != &descriptor.owner_identity
                    || generation != &descriptor.agent_generation_id)
            {
                return Err(SessionResourceError::conflict(
                    "workspace execution descriptor changed",
                ));
            }
        }
        sqlx::query(
            "INSERT INTO session_execution_workspace_descriptors
             (root_id, owner_epoch, endpoint, key_identity, agent_generation_id, unsupported_async_owners)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(root_id) DO UPDATE SET owner_epoch = excluded.owner_epoch,
                endpoint = excluded.endpoint, key_identity = excluded.key_identity,
                agent_generation_id = excluded.agent_generation_id,
                unsupported_async_owners = MAX(session_execution_workspace_descriptors.unsupported_async_owners,
                                               excluded.unsupported_async_owners)",
        )
        .bind(token.root_id.as_str())
        .bind(token.epoch)
        .bind(&descriptor.endpoint)
        .bind(&descriptor.owner_identity)
        .bind(&descriptor.agent_generation_id)
        .bind(i64::from(descriptor.unsupported_async_owners))
        .execute(&mut *tx)
        .await
        .map_err(|e| map_sqlx(&e))?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(token.root_id.clone())))
    }

    pub(super) async fn read_workspace_owner(
        &self,
        root: &ThreadId,
    ) -> SessionResourceResult<Option<ExecutionWorkspaceOwnerRecord>> {
        let row: Option<(i64, i64, String, String, String, i64)> = sqlx::query_as(
            "SELECT o.epoch, d.owner_epoch, d.endpoint, d.key_identity,
                    d.agent_generation_id, d.unsupported_async_owners
             FROM session_execution_owners o
             JOIN session_execution_workspace_descriptors d ON d.root_id = o.root_id
             WHERE o.root_id = ?1",
        )
        .bind(root.as_str())
        .fetch_optional(&self.database.pool)
        .await
        .map_err(|e| map_sqlx(&e))?;
        Ok(row.map(
            |(
                current_epoch,
                descriptor_epoch,
                endpoint,
                owner_identity,
                agent_generation_id,
                unsupported,
            )| ExecutionWorkspaceOwnerRecord {
                current_epoch,
                descriptor_epoch,
                descriptor: WorkspaceExecutionDescriptor {
                    endpoint,
                    owner_identity,
                    agent_generation_id,
                    unsupported_async_owners: unsupported != 0,
                },
            },
        ))
    }

    pub(super) async fn mark_unsupported_workspace_owner(
        &self,
        token: &ExecutionOwnerToken,
    ) -> SessionResourceResult<()> {
        self.writable()?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|e| map_sqlx(&e))?;
        self.assert_owner(&mut tx, &token.root_id).await?;
        if self.execution_owner_token(&token.root_id).as_ref() != Some(token) {
            return Err(SessionResourceError::conflict(
                "workspace descriptor owner token is stale",
            ));
        }
        let changed = sqlx::query(
            "UPDATE session_execution_workspace_descriptors SET unsupported_async_owners = 1
             WHERE root_id = ?1 AND owner_epoch = ?2",
        )
        .bind(token.root_id.as_str())
        .bind(token.epoch)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_sqlx(&e))?;
        if changed.rows_affected() != 1 {
            return Err(SessionResourceError::conflict(
                "workspace execution descriptor is missing",
            ));
        }
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(token.root_id.clone())))
    }

    pub(super) async fn claim_owner(
        &self,
        root: &ThreadId,
        require_closing: bool,
        expected_previous_epoch: Option<i64>,
    ) -> SessionResourceResult<ExecutionOwnerClaim> {
        self.writable()?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|e| map_sqlx(&e))?;
        let claim = self
            .claim_owner_on(&mut tx, root, require_closing, expected_previous_epoch)
            .await?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(root.clone())))?;
        Ok(claim)
    }

    /// [`Self::claim_owner`] 的事务内核心：调用方持有 `BEGIN IMMEDIATE` 并负责提交，
    /// 提交被回滚时数据库不会留下任何代际。
    ///
    /// 代际的进程内可见性（`owner_tokens`）由调用方在**提交之后**安装：提交前的失败
    /// 不会留下无主登记。
    pub(super) async fn claim_owner_on(
        &self,
        tx: &mut SqliteConnection,
        root: &ThreadId,
        require_closing: bool,
        expected_previous_epoch: Option<i64>,
    ) -> SessionResourceResult<ExecutionOwnerClaim> {
        let actual_root = thread_root_on(&mut *tx, root).await.map_err(read_failure)?;
        if actual_root != *root {
            return Err(invalid_input("execution owner must claim a root session"));
        }
        let close: Option<(i64,)> =
            sqlx::query_as("SELECT 1 FROM session_close_intents WHERE thread_id = ?1")
                .bind(root.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| map_sqlx(&e))?;
        if require_closing && close.is_none() {
            return Err(SessionResourceError::conflict(
                "session has no close intent",
            ));
        }
        if !require_closing && close.is_some() {
            return Err(SessionResourceError::conflict("session is closing"));
        }
        let previous: Option<(i64, i64, i64)> = sqlx::query_as(
            "SELECT epoch, released, expires_at_unix FROM session_execution_owners WHERE root_id = ?1",
        )
        .bind(root.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| map_sqlx(&e))?;
        if let Some(expected) = expected_previous_epoch {
            if previous.as_ref().map(|row| row.0) != Some(expected) {
                return Err(SessionResourceError::conflict(
                    "execution owner epoch changed",
                ));
            }
        }
        let prior_unreleased = if previous.as_ref().is_some_and(|row| row.1 == 0) {
            let generation: Option<(String,)> = sqlx::query_as(
                "SELECT agent_generation_id FROM session_execution_workspace_descriptors
                 WHERE root_id = ?1 AND owner_epoch = ?2",
            )
            .bind(root.as_str())
            .bind(previous.as_ref().expect("checked above").0)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| map_sqlx(&e))?;
            Some(PriorExecutionOwner {
                agent_generation_id: generation.map(|row| row.0),
            })
        } else {
            None
        };
        let nonce = uuid::Uuid::new_v4().to_string();
        let changed = sqlx::query(
            "INSERT INTO session_execution_owners(root_id, epoch, nonce, expires_at_unix, released)
             VALUES (?1, 1, ?2, CAST(strftime('%s','now') AS INTEGER) + ?3, 0)
             ON CONFLICT(root_id) DO UPDATE SET
                epoch = session_execution_owners.epoch + 1,
                nonce = excluded.nonce,
                expires_at_unix = excluded.expires_at_unix,
                released = 0
             WHERE session_execution_owners.released = 1
                OR session_execution_owners.expires_at_unix <= CAST(strftime('%s','now') AS INTEGER)",
        )
        .bind(root.as_str())
        .bind(&nonce)
        .bind(OWNER_TTL_SECONDS)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_sqlx(&e))?;
        if changed.rows_affected() != 1 {
            return Err(SessionResourceError::conflict(
                "session execution owner is still active",
            ));
        }
        let (epoch,): (i64,) =
            sqlx::query_as("SELECT epoch FROM session_execution_owners WHERE root_id = ?1")
                .bind(root.as_str())
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| map_sqlx(&e))?;
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
        self.writable()?;
        let updated = sqlx::query(
            "UPDATE session_execution_owners
             SET expires_at_unix = CAST(strftime('%s','now') AS INTEGER) + ?1
             WHERE root_id = ?2 AND epoch = ?3 AND nonce = ?4 AND released = 0
               AND expires_at_unix > CAST(strftime('%s','now') AS INTEGER)",
        )
        .bind(OWNER_TTL_SECONDS)
        .bind(token.root_id.as_str())
        .bind(token.epoch)
        .bind(&token.nonce)
        .execute(&self.database.pool)
        .await
        .map_err(|e| map_sqlx(&e))?;
        if updated.rows_affected() != 1 {
            return Err(SessionResourceError::conflict(
                "execution owner token is stale",
            ));
        }
        Ok(())
    }

    pub(super) async fn release_owner(
        &self,
        token: &ExecutionOwnerToken,
    ) -> SessionResourceResult<()> {
        self.writable()?;
        let updated = sqlx::query(
            "UPDATE session_execution_owners SET released = 1
             WHERE root_id = ?1 AND epoch = ?2 AND nonce = ?3 AND released = 0",
        )
        .bind(token.root_id.as_str())
        .bind(token.epoch)
        .bind(&token.nonce)
        .execute(&self.database.pool)
        .await
        .map_err(|e| map_sqlx(&e))?;
        if updated.rows_affected() != 1 {
            return Err(SessionResourceError::conflict(
                "execution owner token is stale",
            ));
        }
        let mut tokens = self
            .owner_tokens
            .lock()
            .expect("owner token mutex poisoned");
        if tokens.get(&token.root_id) == Some(token) {
            tokens.remove(&token.root_id);
        }
        Ok(())
    }

    /// The close intent remains recoverable until release and intent removal
    /// commit together. A delayed old generation cannot finish a new one.
    pub(super) async fn finish_close_owner(
        &self,
        token: &ExecutionOwnerToken,
    ) -> SessionResourceResult<()> {
        self.writable()?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|e| map_sqlx(&e))?;
        let valid: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM session_execution_owners WHERE root_id = ?1 AND epoch = ?2
             AND nonce = ?3 AND released = 0
             AND expires_at_unix > CAST(strftime('%s','now') AS INTEGER)",
        )
        .bind(token.root_id.as_str())
        .bind(token.epoch)
        .bind(&token.nonce)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| map_sqlx(&e))?;
        if valid.is_none() {
            return Err(SessionResourceError::conflict(
                "execution owner token is stale",
            ));
        }
        let intent = sqlx::query("DELETE FROM session_close_intents WHERE thread_id = ?1")
            .bind(token.root_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|e| map_sqlx(&e))?;
        if intent.rows_affected() != 1 {
            return Err(SessionResourceError::conflict(
                "session close intent is missing",
            ));
        }
        let released = sqlx::query(
            "UPDATE session_execution_owners SET released = 1
             WHERE root_id = ?1 AND epoch = ?2 AND nonce = ?3 AND released = 0",
        )
        .bind(token.root_id.as_str())
        .bind(token.epoch)
        .bind(&token.nonce)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_sqlx(&e))?;
        if released.rows_affected() != 1 {
            return Err(SessionResourceError::conflict(
                "execution owner token is stale",
            ));
        }
        sqlx::query("DELETE FROM session_execution_workspace_descriptors WHERE root_id = ?1")
            .bind(token.root_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|e| map_sqlx(&e))?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(token.root_id.clone())))?;
        let mut tokens = self
            .owner_tokens
            .lock()
            .expect("owner token mutex poisoned");
        if tokens.get(&token.root_id) == Some(token) {
            tokens.remove(&token.root_id);
        }
        Ok(())
    }

    pub(super) async fn read_close_settlement(
        &self,
        token: &ExecutionOwnerToken,
    ) -> SessionResourceResult<CloseSettlement> {
        let mut tx = self.database.pool.begin().await.map_err(|e| map_sqlx(&e))?;
        let owner: Option<(i64, String, i64)> = sqlx::query_as(
            "SELECT epoch, nonce, released FROM session_execution_owners WHERE root_id = ?1",
        )
        .bind(token.root_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| map_sqlx(&e))?;
        let intent: Option<(i64,)> =
            sqlx::query_as("SELECT 1 FROM session_close_intents WHERE thread_id = ?1")
                .bind(token.root_id.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| map_sqlx(&e))?;
        let settlement = match owner {
            Some((epoch, nonce, released)) if epoch == token.epoch && nonce == token.nonce => {
                match (released, intent.is_some()) {
                    (0, true) => CloseSettlement::Pending,
                    (1, false) => CloseSettlement::Finished,
                    _ => CloseSettlement::ChangedOwner,
                }
            }
            _ => CloseSettlement::ChangedOwner,
        };
        tx.commit().await.map_err(|e| map_sqlx(&e))?;
        Ok(settlement)
    }

    /// Must be called inside the same `BEGIN IMMEDIATE` transaction as the mutation.
    pub(super) async fn assert_owner(
        &self,
        tx: &mut SqliteConnection,
        id: &ThreadId,
    ) -> SessionResourceResult<()> {
        if !thread_exists_on(&mut *tx, id).await.map_err(read_failure)? {
            return Err(not_found());
        }
        let root = thread_root_on(&mut *tx, id).await.map_err(read_failure)?;
        let token = self
            .execution_owner_token(&root)
            .ok_or_else(|| SessionResourceError::conflict("session execution owner is absent"))?;
        let valid: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM session_execution_owners
             WHERE root_id = ?1 AND epoch = ?2 AND nonce = ?3 AND released = 0
               AND expires_at_unix > CAST(strftime('%s','now') AS INTEGER)",
        )
        .bind(root.as_str())
        .bind(token.epoch)
        .bind(&token.nonce)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| map_sqlx(&e))?;
        if valid.is_none() {
            return Err(SessionResourceError::conflict(
                "session execution owner is stale",
            ));
        }
        Ok(())
    }

    /// 迁移期会话接纳的所有权前置，必须在接纳事务内调用。
    ///
    /// 判据是**有无 owner 行**，不是有无绑定：schema 13 不给既有会话补行，因此「无行」
    /// 精确等价于「从未被新语义的执行面接管过」——首次接纳由本次事务建立第一代所有权，
    /// 与 binding / frozen 同一次提交（并发的第二个接纳者被 [`Self::claim_owner_on`] 的
    /// CAS 挡下）。存在 owner 行（含已 released）说明该会话被接管过：接纳必须持有当前
    /// 代际，无主改写仍按 [`Self::assert_owner`] 拒绝——recovered 会话不得被自动接纳。
    ///
    /// 返回 `Some` 时调用方必须在**提交之后**把它安装进 `owner_tokens`。
    pub(super) async fn claim_or_assert_legacy_adoption_owner_on(
        &self,
        tx: &mut SqliteConnection,
        id: &ThreadId,
    ) -> SessionResourceResult<Option<ExecutionOwnerToken>> {
        if !thread_exists_on(&mut *tx, id).await.map_err(read_failure)? {
            return Err(not_found());
        }
        let root = thread_root_on(&mut *tx, id).await.map_err(read_failure)?;
        let existing: Option<(i64,)> =
            sqlx::query_as("SELECT 1 FROM session_execution_owners WHERE root_id = ?1")
                .bind(root.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| map_sqlx(&e))?;
        if existing.is_some() {
            self.assert_owner(&mut *tx, id).await?;
            return Ok(None);
        }
        let claim = self.claim_owner_on(&mut *tx, &root, false, None).await?;
        Ok(Some(claim.token))
    }
}
