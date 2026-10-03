//! Local Session metadata updates and tree deletion.

use super::*;

impl SqliteSessionData {
    /// Admission guard shared by lifecycle and mutation paths.
    pub(super) fn writable(&self) -> SessionResourceResult<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(unavailable("session data port is closed"));
        }
        if self.database.is_read_only() {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::ReadOnlyStore,
            ));
        }
        Ok(())
    }

    /// 未发布创建的撤销：`require_uncommitted` 决定是否叠加「未提交 frozen」判据。
    ///
    /// 两条入口共用同一份删除顺序（子会话守卫 → 子表 → threads 行），只有
    /// 判据按调用方语义分档：
    ///
    /// - `false`：write-once 完整创建（fork 等）的失败补偿——目标创建即带 frozen，
    ///   撤销就是把它整条删掉（可由 source 重生成）；
    /// - `true`：两阶段草稿（`SessionInitialization::abandon`）——已定稿的草稿是
    ///   「已提交、未发布」的合法中间态，必须拒绝删除（typed 冲突），保留按 ID 打开。
    pub(super) async fn revoke_created_row(
        &self,
        id: &ThreadId,
        require_uncommitted: bool,
    ) -> SessionResourceResult<()> {
        self.writable()?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.assert_owner(&mut tx, id).await?;
        // 撤销只针对「本次未发布的创建」：已经派生过子会话的 identity 不能被补偿掉，
        // 否则子会话会指向一个不存在的父节点。
        let children: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM threads WHERE parent_thread_id = ?1")
                .bind(id.as_str())
                .fetch_one(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
        if children.0 > 0 {
            return Err(invalid_input(
                "session has published children and cannot be revoked",
            ));
        }
        if require_uncommitted {
            let committed: Option<(Option<String>,)> =
                sqlx::query_as("SELECT frozen_context FROM threads WHERE id = ?1")
                    .bind(id.as_str())
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|error| map_sqlx(&error))?;
            if matches!(committed, Some((Some(_),))) {
                return Err(SessionResourceError::conflict(
                    "session has a committed frozen snapshot and cannot be revoked",
                ));
            }
        }
        // 子表行同样显式删除，不借 `ON DELETE CASCADE`：那份级联只在 SQLite 上存在，
        // 远端执行器没有（见 [`session_rows::THREAD_CHILD_DELETES`]）。先子后父。
        delete_thread_child_rows(&mut tx, id.as_str())
            .await
            .map_err(|error| map_sqlx(&error))?;
        sqlx::query(session_rows::DELETE_THREAD_SQL)
            .bind(id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(id.clone())))?;
        Ok(())
    }

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
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|e| map_sqlx(&e))?;
        self.assert_owner(&mut tx, id).await?;
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
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        if updated.rows_affected() != 1 {
            return Err(not_found());
        }
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(id.clone())))?;
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
        self.assert_owner(&mut tx, id).await?;
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
