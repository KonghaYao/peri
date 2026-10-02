//! Local Session metadata updates and tree deletion.

use super::*;

impl SqliteSessionData {
    pub(super) async fn update_stored_meta(
        &self,
        id: &ThreadId,
        patch: &SessionMetaPatch,
    ) -> SessionResourceResult<()> {
        self.writable()?;
        if patch.title.is_none()
            && patch.status.is_none()
            && patch.cancel_policy.is_none()
            && patch.config.is_none()
        {
            // 没有字段要改：不写、也不假装写入了新时间戳。
            return Ok(());
        }
        let now = Utc::now().to_rfc3339();
        let mut builder: sqlx::QueryBuilder<sqlx::Sqlite> =
            sqlx::QueryBuilder::new("UPDATE threads SET updated_at = ");
        builder.push_bind(&now);
        if let Some(title) = &patch.title {
            builder.push(", title = ").push_bind(title.clone());
        }
        if let Some(status) = &patch.status {
            builder.push(", agent_status = ").push_bind(status.as_str());
        }
        if let Some(policy) = &patch.cancel_policy {
            builder
                .push(", cancel_policy = ")
                .push_bind(policy.as_str());
        }
        if let Some(config) = &patch.config {
            builder.push(", config = ").push_bind(config.clone());
        }
        builder.push(" WHERE id = ").push_bind(id.as_str());
        let updated = builder
            .build()
            .execute(&self.database.pool)
            .await
            .map_err(|error| map_sqlx(&error))?;
        if updated.rows_affected() != 1 {
            return Err(not_found());
        }
        Ok(())
    }

    pub(super) async fn delete_stored_tree(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.writable()?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        if !thread_exists_on(&mut tx, id).await.map_err(read_failure)? {
            return Err(not_found());
        }
        let tree = thread_tree_on(&mut tx, id).await.map_err(read_failure)?;
        // 子表行显式删除，不借 `ON DELETE CASCADE`：级联只在 SQLite 上存在，远端执行器
        // 没有（见 [`session_rows::THREAD_CHILD_DELETES`]）。顺序与远端 delete_tree 一致：
        // 子行全部先删，最后才删 threads 行。
        for thread in &tree {
            delete_thread_child_rows(&mut tx, thread)
                .await
                .map_err(|error| map_sqlx(&error))?;
        }
        for thread in &tree {
            sqlx::query(session_rows::DELETE_THREAD_SQL)
                .bind(thread)
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
