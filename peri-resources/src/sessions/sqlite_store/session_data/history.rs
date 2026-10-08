use super::*;

impl SqliteSessionData {
    pub(super) async fn append_history_rows(
        &self,
        id: &ThreadId,
        payloads: &[PersistedPayload],
    ) -> SessionResourceResult<()> {
        self.writable()?;
        if payloads.is_empty() {
            return Ok(());
        }
        // 相同 ID 的碰撞不能静默忽略：批次内重复与库存重复都必须失败，
        // 否则「已存在但内容不同」会被当成成功。
        let mut seen = HashSet::with_capacity(payloads.len());
        for payload in payloads {
            if !seen.insert(payload.id()) {
                return Err(invalid_input("history batch repeats a message id"));
            }
        }
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, id).await?;
        if !thread_exists_on(&mut tx, id).await.map_err(read_failure)? {
            // 目标会话不存在与「外键拒绝」是两个不同的原因，前者更可诊断。
            return Err(not_found());
        }
        for payload in payloads {
            sqlx::query(
                "INSERT INTO messages (message_id, thread_id, role, content)
                 VALUES (?1, ?2, ?3, ?4)",
            )
            .bind(payload.id().as_uuid().to_string())
            .bind(id.as_str())
            .bind(payload_role(payload))
            .bind(
                serialize_persisted_payload(payload)
                    .map_err(|_| corrupt("history entry is not serializable"))?,
            )
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        }
        let now = peri_time::now_utc_rfc3339();
        let updated = sqlx::query(
            "UPDATE threads SET updated_at = ?1,
                message_count = (SELECT COUNT(*) FROM messages WHERE thread_id = ?2)
             WHERE id = ?2",
        )
        .bind(&now)
        .bind(id.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|error| map_sqlx(&error))?;
        if updated.rows_affected() != 1 {
            return Err(not_found());
        }
        let messages = payloads
            .iter()
            .filter_map(PersistedPayload::as_message)
            .cloned()
            .collect::<Vec<_>>();
        if let Some(title) = extract_title(&messages) {
            sqlx::query("UPDATE threads SET title = ?1 WHERE id = ?2 AND title IS NULL")
                .bind(&title)
                .bind(id.as_str())
                .execute(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
        }
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(id.clone())))?;
        Ok(())
    }
}

impl SqliteSessionData {
    pub(super) async fn rewind_stored_history(
        &self,
        id: &ThreadId,
        boundary: RewindBoundary,
    ) -> SessionResourceResult<()> {
        self.writable()?;
        let target = boundary.message_id().as_uuid().to_string();
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, id).await?;
        let rowid: Option<(i64,)> =
            sqlx::query_as("SELECT rowid FROM messages WHERE thread_id = ?1 AND message_id = ?2")
                .bind(id.as_str())
                .bind(&target)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
        // 未知截止点保持无变更语义：找不到目标就不动历史。
        if let Some((rowid,)) = rowid {
            let sql = match boundary {
                RewindBoundary::KeepThrough(_) => {
                    "DELETE FROM messages WHERE thread_id = ?1 AND rowid > ?2"
                }
                RewindBoundary::RemoveFrom(_) => {
                    "DELETE FROM messages WHERE thread_id = ?1 AND rowid >= ?2"
                }
            };
            sqlx::query(sql)
                .bind(id.as_str())
                .bind(rowid)
                .execute(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
            refresh_history_derivations(&mut tx, id).await?;
        }
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(id.clone())))?;
        Ok(())
    }

    pub(super) async fn remove_stored_history_entries(
        &self,
        id: &ThreadId,
        ids: &[MessageId],
    ) -> SessionResourceResult<()> {
        self.writable()?;
        if ids.is_empty() {
            return Ok(());
        }
        let unique: Vec<MessageId> = {
            let mut seen = HashSet::with_capacity(ids.len());
            ids.iter().copied().filter(|id| seen.insert(*id)).collect()
        };
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, id).await?;
        for message_id in &unique {
            // 别会话的条目不允许被「精确移除」静默命中或静默跳过。
            let owner: Option<(String,)> =
                sqlx::query_as("SELECT thread_id FROM messages WHERE message_id = ?1")
                    .bind(message_id.as_uuid().to_string())
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|error| map_sqlx(&error))?;
            match owner {
                Some((owner,)) if owner == id.as_str() => {
                    sqlx::query("DELETE FROM messages WHERE message_id = ?1 AND thread_id = ?2")
                        .bind(message_id.as_uuid().to_string())
                        .bind(id.as_str())
                        .execute(&mut *tx)
                        .await
                        .map_err(|error| map_sqlx(&error))?;
                }
                Some(_) => return Err(invalid_input("history entry belongs to another session")),
                // 已经不存在的条目是幂等删除，不产生错误。
                None => {}
            }
        }
        refresh_history_derivations(&mut tx, id).await?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(id.clone())))?;
        Ok(())
    }
}
