//! SQLite ThreadStore 的唯一 pool owner 与契约实现。
//! 连接、行映射、上下文和 compaction 事务由私有模块负责。

mod compaction;
mod connection;
mod context;
mod discovery;
mod execution;
mod row_mapping;
mod schema;
mod workspace;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
pub use connection::{ReadOnlyStoreErrorKind, ReadOnlyThreadStoreError};
use peri_acp_types::{
    messages::BaseMessage,
    store::{
        deserialize_persisted_payload, serialize_persisted_payload, CompactionLifecycle,
        InheritedContext, MessageFlags, PersistedPayload, ThreadStore,
    },
    thread::{AgentStatus, ThreadId, ThreadListEntry, ThreadMeta},
};
use row_mapping::{
    extract_title, meta_from_row, role_of, ThreadRow, THREAD_COLUMNS, THREAD_META_COLUMNS,
};
use sqlx::{AssertSqlSafe, SqlitePool};
use std::{collections::HashMap, str::FromStr};

#[cfg(test)]
use connection::{classify_shape_probe_failure, REQUIRED_MESSAGE_COLUMNS, REQUIRED_THREAD_COLUMNS};
#[cfg(test)]
use sqlx::sqlite::SqliteConnectOptions;

/// 基于 SQLite 的 ThreadStore 实现
///
/// 使用 WAL 模式提升并发读性能，sqlx SqlitePool 连接池管理并发。
pub struct SqliteThreadStore {
    pool: SqlitePool,
    read_only: bool,
    db_path: std::path::PathBuf,
    execution_leases:
        std::sync::Mutex<HashMap<ThreadId, std::sync::Weak<execution::ExecutionLease>>>,
}

// ── ThreadStore impl ───────────────────────────────────────────────────────────

#[async_trait]
impl ThreadStore for SqliteThreadStore {
    async fn resolve_workspace(
        &self,
        cwd: &std::path::Path,
    ) -> Result<peri_acp_types::workspace::ResolvedWorkspace> {
        self.resolve_workspace_impl(cwd).await
    }
    async fn create_bound_thread(
        &self,
        meta: ThreadMeta,
        workspace: &peri_acp_types::workspace::ResolvedWorkspace,
    ) -> Result<ThreadId> {
        self.create_bound_thread_impl(meta, workspace).await
    }
    async fn load_session_binding(
        &self,
        id: &ThreadId,
    ) -> Result<Option<peri_acp_types::workspace::SessionBinding>> {
        self.load_session_binding_impl(id).await
    }
    async fn validate_session_binding(
        &self,
        id: &ThreadId,
    ) -> Result<peri_acp_types::workspace::ResolvedWorkspace> {
        self.validate_session_binding_impl(id).await
    }
    async fn reassert_session_binding(
        &self,
        id: &ThreadId,
    ) -> Result<peri_acp_types::workspace::ResolvedWorkspace> {
        self.reassert_session_binding_impl(id).await
    }
    async fn adopt_legacy_thread(
        &self,
        id: &ThreadId,
        saved_cwd: &str,
        workspace: &peri_acp_types::workspace::ResolvedWorkspace,
        frozen_snapshot: &str,
    ) -> Result<()> {
        self.adopt_legacy_thread_impl(id, saved_cwd, workspace, frozen_snapshot)
            .await
    }
    async fn list_scoped_threads(
        &self,
        query: &peri_acp_types::workspace::ScopedThreadQuery,
    ) -> Result<peri_acp_types::workspace::ScopedThreadPage> {
        self.list_scoped_threads_impl(query).await
    }
    async fn acquire_execution_lease(
        &self,
        id: &ThreadId,
    ) -> Result<std::sync::Arc<dyn peri_acp_types::workspace::SessionExecutionLease>> {
        self.acquire_execution_lease_impl(id).await
    }

    async fn reset_dirty_execution(
        &self,
        target: &peri_acp_types::workspace::RecoveryRequiredDetails,
    ) -> Result<()> {
        self.reset_dirty_execution_impl(target).await
    }

    async fn create_thread(&self, meta: ThreadMeta) -> Result<ThreadId> {
        let id = meta.id.clone();
        sqlx::query(
            "INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count,
                parent_thread_id, snapshot_at_message_id, hidden, cancel_policy, config, cached_context, agent_status, context_cache_epoch)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 0)",
        )
        .bind(&meta.id)
        .bind(&meta.title)
        .bind(&meta.cwd)
        .bind(meta.created_at.to_rfc3339())
        .bind(meta.updated_at.to_rfc3339())
        .bind(meta.message_count as i64)
        .bind(&meta.parent_thread_id)
        .bind(&meta.snapshot_at_message_id)
        .bind(meta.hidden)
        .bind(meta.cancel_policy.as_str())
        .bind(&meta.config)
        .bind(&meta.cached_context)
        .bind(meta.agent_status.as_str())
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    async fn append_messages(&self, id: &ThreadId, msgs: &[BaseMessage]) -> Result<()> {
        let payloads = msgs
            .iter()
            .cloned()
            .map(PersistedPayload::Message)
            .collect::<Vec<_>>();
        self.append_payloads(id, &payloads).await
    }

    async fn load_messages(&self, id: &ThreadId) -> Result<Vec<BaseMessage>> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT content FROM messages WHERE thread_id = ?1 ORDER BY rowid")
                .bind(id.as_str())
                .fetch_all(&self.pool)
                .await?;

        rows.into_iter()
            .filter_map(|(content,)| match deserialize_persisted_payload(&content) {
                Ok(PersistedPayload::Message(message)) => Some(Ok(message)),
                Ok(PersistedPayload::SystemReminder { .. }) => None,
                Err(error) => Some(Err(error)),
            })
            .collect()
    }

    async fn append_payloads(&self, id: &ThreadId, payloads: &[PersistedPayload]) -> Result<()> {
        let write_guard = self.require_execution_lease(id).await?;
        let result = async {
            if payloads.is_empty() {
                return Ok(());
            }
            let mut tx = self.pool.begin().await?;
            for payload in payloads {
                let message_id = payload.id().as_uuid().to_string();
                let role = payload
                    .as_message()
                    .map(role_of)
                    .unwrap_or("system_reminder");
                let content = serialize_persisted_payload(payload)?;
                sqlx::query(
                    "INSERT OR IGNORE INTO messages (message_id, thread_id, role, content)
                 VALUES (?1, ?2, ?3, ?4)",
                )
                .bind(&message_id)
                .bind(id.as_str())
                .bind(role)
                .bind(&content)
                .execute(&mut *tx)
                .await?;
            }
            sqlx::query(
                "UPDATE threads SET updated_at = ?1,
                message_count = (SELECT COUNT(*) FROM messages WHERE thread_id = ?2)
             WHERE id = ?2",
            )
            .bind(Utc::now().to_rfc3339())
            .bind(id.as_str())
            .execute(&mut *tx)
            .await?;
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
                    .await?;
            }
            tx.commit().await?;
            Ok(())
        }
        .await;
        if let Some(guard) = write_guard {
            guard.finish();
        }
        result
    }

    async fn load_payloads(&self, id: &ThreadId) -> Result<Vec<PersistedPayload>> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT message_id, content FROM messages WHERE thread_id = ?1 ORDER BY rowid",
        )
        .bind(id.as_str())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(row_id, content)| {
                let payload = deserialize_persisted_payload(&content)?;
                if payload.id().as_uuid().to_string() != row_id {
                    anyhow::bail!("persisted payload message id mismatch");
                }
                Ok(payload)
            })
            .collect()
    }

    async fn load_meta(&self, id: &ThreadId) -> Result<ThreadMeta> {
        let row: ThreadRow = match sqlx::query_as(AssertSqlSafe(format!(
            "SELECT {THREAD_COLUMNS} FROM threads t WHERE t.id = ?1"
        )))
        .bind(id.as_str())
        .fetch_one(&self.pool)
        .await
        {
            Ok(row) => row,
            Err(error) if self.read_only => {
                let kind = if matches!(error, sqlx::Error::RowNotFound) {
                    ReadOnlyStoreErrorKind::SessionNotFound
                } else if matches!(
                    error,
                    sqlx::Error::ColumnDecode { .. } | sqlx::Error::Decode(_)
                ) {
                    ReadOnlyStoreErrorKind::CorruptSessionData
                } else {
                    ReadOnlyStoreErrorKind::DatabaseUnreadable
                };
                return Err(ReadOnlyThreadStoreError::from_kind(kind).into());
            }
            Err(error) => return Err(error.into()),
        };

        let result = meta_from_row(
            row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8, row.9, row.10, row.11,
            row.12, row.13,
        );
        if self.read_only {
            result.map_err(|_| {
                ReadOnlyThreadStoreError::from_kind(ReadOnlyStoreErrorKind::CorruptSessionData)
                    .into()
            })
        } else {
            result
        }
    }

    async fn update_meta(&self, id: &ThreadId, meta: ThreadMeta) -> Result<()> {
        let write_guard = self.require_execution_lease(id).await?;
        let result = async {
            if self.load_session_binding_impl(id).await?.is_some() {
                let original: (String, Option<String>) =
                    sqlx::query_as("SELECT cwd, parent_thread_id FROM threads WHERE id = ?")
                        .bind(id)
                        .fetch_one(&self.pool)
                        .await?;
                if original.0 != meta.cwd || original.1 != meta.parent_thread_id {
                    return Err(
                        peri_acp_types::workspace::WorkspaceError::ExecutionBindingMismatch.into(),
                    );
                }
            }
            sqlx::query(
                "UPDATE threads SET title = ?1, cwd = ?2, updated_at = ?3, message_count = ?4,
                parent_thread_id = ?5, snapshot_at_message_id = ?6, hidden = ?7,
                cancel_policy = ?8, config = ?9, cached_context = ?10, agent_status = ?11
             WHERE id = ?12",
            )
            .bind(&meta.title)
            .bind(&meta.cwd)
            .bind(meta.updated_at.to_rfc3339())
            .bind(meta.message_count as i64)
            .bind(&meta.parent_thread_id)
            .bind(&meta.snapshot_at_message_id)
            .bind(meta.hidden)
            .bind(meta.cancel_policy.as_str())
            .bind(&meta.config)
            .bind(&meta.cached_context)
            .bind(meta.agent_status.as_str())
            .bind(id.as_str())
            .execute(&self.pool)
            .await?;
            Ok(())
        }
        .await;
        if let Some(guard) = write_guard {
            guard.finish();
        }
        result
    }

    async fn load_frozen_snapshot(&self, id: &ThreadId) -> Result<Option<String>> {
        let row: (Option<String>,) =
            sqlx::query_as("SELECT frozen_context FROM threads WHERE id = ?1")
                .bind(id.as_str())
                .fetch_one(&self.pool)
                .await?;
        Ok(row.0)
    }

    async fn store_frozen_snapshot_if_absent(&self, id: &ThreadId, snapshot: &str) -> Result<bool> {
        let write_guard = self.require_execution_lease(id).await?;
        let result = async {
            let result = sqlx::query(
                "UPDATE threads SET frozen_context = ?1 WHERE id = ?2 AND frozen_context IS NULL",
            )
            .bind(snapshot)
            .bind(id.as_str())
            .execute(&self.pool)
            .await?;
            if result.rows_affected() == 1 {
                return Ok(true);
            }
            let row: Option<(Option<String>,)> =
                sqlx::query_as("SELECT frozen_context FROM threads WHERE id = ?1")
                    .bind(id.as_str())
                    .fetch_optional(&self.pool)
                    .await?;
            match row {
                Some((Some(_),)) => Ok(false),
                Some((None,)) => anyhow::bail!(
                    "frozen snapshot write lost without a persisted winner for thread: {id}"
                ),
                None => anyhow::bail!("thread 不存在，无法写入 frozen snapshot: {id}"),
            }
        }
        .await;
        if let Some(guard) = write_guard {
            guard.finish();
        }
        result
    }

    async fn list_threads(&self) -> Result<Vec<ThreadMeta>> {
        let rows: Vec<ThreadRow> = sqlx::query_as(AssertSqlSafe(format!(
            "SELECT {THREAD_META_COLUMNS} FROM threads t WHERE t.hidden = 0 ORDER BY t.updated_at DESC"
        )))
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                meta_from_row(
                    row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8, row.9, row.10,
                    row.11, row.12, row.13,
                )
            })
            .collect()
    }

    async fn list_thread_entries(&self, cwd: &str) -> Result<Vec<ThreadListEntry>> {
        let rows: Vec<(String, Option<String>, String, i64, String)> = sqlx::query_as(
            "SELECT id, title, cwd, message_count, updated_at
             FROM threads
             WHERE hidden = 0 AND message_count > 0 AND cwd = ?
             ORDER BY updated_at DESC",
        )
        .bind(cwd)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|(id, title, cwd, message_count, updated_at)| {
                Ok(ThreadListEntry {
                    id,
                    title,
                    cwd,
                    message_count: message_count as usize,
                    updated_at: DateTime::parse_from_rfc3339(&updated_at)?.with_timezone(&Utc),
                })
            })
            .collect()
    }

    async fn delete_thread(&self, id: &ThreadId) -> Result<()> {
        let write_guard = self.require_execution_lease(id).await?;
        let result = async {
            let mut tx = self.pool.begin().await?;
            // 级联删除整个线程树：hidden 子 agent 线程沿 parent_thread_id 挂链，
            // 若不递归删除会留下永远无法通过 UI/协议访问的孤儿数据（messages 表
            // 依赖 threads 行 FK ON DELETE CASCADE 一并清除）。
            let mut to_delete = vec![id.as_str().to_string()];
            let mut idx = 0;
            while idx < to_delete.len() {
                let children: Vec<(String,)> =
                    sqlx::query_as("SELECT id FROM threads WHERE parent_thread_id = ?1")
                        .bind(&to_delete[idx])
                        .fetch_all(&mut *tx)
                        .await?;
                to_delete.extend(children.into_iter().map(|(cid,)| cid));
                idx += 1;
            }
            for tid in &to_delete {
                sqlx::query("DELETE FROM threads WHERE id = ?1")
                    .bind(tid)
                    .execute(&mut *tx)
                    .await?;
            }
            tx.commit().await?;
            Ok(())
        }
        .await;
        if let Some(guard) = write_guard {
            guard.finish();
        }
        result
    }

    async fn update_title(&self, id: &ThreadId, title: &str) -> Result<()> {
        let write_guard = self.require_execution_lease(id).await?;
        let result = async {
            let now = Utc::now().to_rfc3339();
            sqlx::query("UPDATE threads SET title = ?1, updated_at = ?2 WHERE id = ?3")
                .bind(title)
                .bind(&now)
                .bind(id.as_str())
                .execute(&self.pool)
                .await?;
            Ok(())
        }
        .await;
        if let Some(guard) = write_guard {
            guard.finish();
        }
        result
    }

    async fn store_inherited_context(
        &self,
        thread_id: &ThreadId,
        inherited: &InheritedContext,
    ) -> Result<()> {
        let write_guard = self.require_execution_lease(thread_id).await?;
        let result =
            async { context::store_inherited_context(self, thread_id, inherited).await }.await;
        if let Some(guard) = write_guard {
            guard.finish();
        }
        result
    }

    async fn load_inherited_context(&self, thread_id: &ThreadId) -> Result<InheritedContext> {
        context::load_inherited_context(self, thread_id).await
    }

    async fn load_context_payloads(&self, thread_id: &ThreadId) -> Result<Vec<PersistedPayload>> {
        context::load_context_payloads(self, thread_id).await
    }

    async fn load_context(&self, thread_id: &ThreadId) -> Result<Vec<BaseMessage>> {
        context::load_context(self, thread_id).await
    }

    async fn list_child_threads(&self, parent_id: &ThreadId) -> Result<Vec<ThreadMeta>> {
        context::list_child_threads(self, parent_id).await
    }

    async fn list_session_threads(&self, root_id: &ThreadId) -> Result<Vec<ThreadMeta>> {
        context::list_session_threads(self, root_id).await
    }

    async fn update_thread_status(&self, id: &ThreadId, status: &str) -> Result<()> {
        let write_guard = self.require_execution_lease(id).await?;
        let result = async {
            // 关键约束：参数字符串必须经 FromStr 解析，非法值直接返回错误，不静默 fallback
            let status = AgentStatus::from_str(status)
                .with_context(|| format!("非法 agent_status 值: {status:?}"))?;
            let now = Utc::now().to_rfc3339();
            sqlx::query("UPDATE threads SET agent_status = ?1, updated_at = ?2 WHERE id = ?3")
                .bind(status.as_str())
                .bind(&now)
                .bind(id.as_str())
                .execute(&self.pool)
                .await?;
            Ok(())
        }
        .await;
        if let Some(guard) = write_guard {
            guard.finish();
        }
        result
    }

    async fn invalidate_context_cache(&self, thread_id: &ThreadId) -> Result<()> {
        let write_guard = self.require_execution_lease(thread_id).await?;
        let result = async { context::invalidate_context_cache(self, thread_id).await }.await;
        if let Some(guard) = write_guard {
            guard.finish();
        }
        result
    }

    async fn get_context_cache_epoch(&self, thread_id: &ThreadId) -> Result<u64> {
        context::get_context_cache_epoch(self, thread_id).await
    }

    async fn delete_messages(
        &self,
        thread_id: &ThreadId,
        message_ids: &[peri_acp_types::messages::MessageId],
    ) -> Result<()> {
        let write_guard = self.require_execution_lease(thread_id).await?;
        let result =
            async { compaction::delete_messages(self, thread_id, message_ids).await }.await;
        if let Some(guard) = write_guard {
            guard.finish();
        }
        result
    }

    async fn update_message_flags(
        &self,
        message_id: &peri_acp_types::messages::MessageId,
        flags: &MessageFlags,
    ) -> Result<()> {
        let owner: Option<(String,)> =
            sqlx::query_as("SELECT thread_id FROM messages WHERE message_id = ?")
                .bind(message_id.as_uuid().to_string())
                .fetch_optional(&self.pool)
                .await?;
        let write_guard = match owner {
            Some((id,)) => self.require_execution_lease(&id).await?,
            None => None,
        };

        let result =
            async { compaction::update_message_flags(self, message_id, flags).await }.await;
        if let Some(guard) = write_guard {
            guard.finish();
        }
        result
    }

    fn supports_compaction_lifecycle(&self) -> bool {
        true
    }

    async fn commit_compaction_lifecycle(
        &self,
        thread_id: &ThreadId,
        lifecycle: &CompactionLifecycle,
    ) -> Result<()> {
        let write_guard = self.require_execution_lease(thread_id).await?;
        let result =
            async { compaction::commit_compaction_lifecycle(self, thread_id, lifecycle).await }
                .await;
        if let Some(guard) = write_guard {
            guard.finish();
        }
        result
    }

    async fn load_message_flags(
        &self,
        thread_id: &ThreadId,
    ) -> Result<HashMap<peri_acp_types::messages::MessageId, MessageFlags>> {
        compaction::load_message_flags(self, thread_id).await
    }

    async fn delete_messages_since(
        &self,
        thread_id: &ThreadId,
        message_id: &peri_acp_types::messages::MessageId,
    ) -> Result<()> {
        let write_guard = self.require_execution_lease(thread_id).await?;
        let result =
            async { compaction::delete_messages_since(self, thread_id, message_id).await }.await;
        if let Some(guard) = write_guard {
            guard.finish();
        }
        result
    }
}

#[cfg(test)]
#[path = "sqlite_store_test.rs"]
mod tests;

#[cfg(test)]
#[path = "sqlite_inherited_context_test.rs"]
mod inherited_context_tests;

#[cfg(test)]
#[path = "sqlite_store/legacy_test.rs"]
mod legacy_tests;
