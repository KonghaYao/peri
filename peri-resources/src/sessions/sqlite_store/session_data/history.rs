use super::*;

impl SqliteSessionData {
    pub(in crate::sessions::sqlite_store) async fn append_history_rows(
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
            messages::insert_payload(&mut tx, id, payload).await?;
        }
        let now = peri_time::now_utc_rfc3339();
        let updated = sqlx::query(
            "UPDATE threads SET updated_at = ?1,
                message_count = message_count + ?3
             WHERE id = ?2",
        )
        .bind(&now)
        .bind(id.as_str())
        .bind(
            i64::try_from(payloads.len())
                .map_err(|_| invalid_input("history batch is too large"))?,
        )
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
