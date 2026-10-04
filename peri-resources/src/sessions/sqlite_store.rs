//! SQLite 会话库：数据面与执行/登记面共用同一 pool 与同一库。
//!
//! [`SqliteThreadStore`] 是消费侧迁移期间的**桥**：它把旧 `ThreadStore` 的调用转发
//! 到共享库句柄 [`database::SqliteSessionDatabase`]；新行为一律加到
//! [`data::SessionDataPort`](super::data::SessionDataPort) 的 SQLite 实现
//! （`session_data`），不在本文件扩展。E 阶段 `ThreadStore` 退出时本类型一并删除。
//! 连接、行映射、上下文、compaction 与 schema 事务由私有模块负责。

mod compaction;
mod connection;
mod context;
mod database;
pub(in crate::sessions) use super::discovery;
mod execution;
mod failure;
mod local;
mod oauth_credentials;
pub(crate) mod row_mapping;
mod schema;
mod schema_cleanup;
mod session_data;
mod session_rows;
#[path = "storage_v2_migration.rs"]
mod storage_v2_migration;
mod workspace;
mod workspace_identity;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
pub use connection::{ReadOnlyStoreErrorKind, ReadOnlyThreadStoreError};
use peri_acp_types::{
    messages::BaseMessage,
    store::{
        deserialize_persisted_payload, serialize_persisted_payload, CompactionChange,
        InheritedContext, MessageFlags, PersistedPayload, ThreadStore,
    },
    thread::{AgentStatus, ThreadId, ThreadListEntry, ThreadMeta},
};
use row_mapping::{extract_title, meta_from_row, role_of, ThreadRow, THREAD_META_COLUMNS};
use sqlx::AssertSqlSafe;
use std::{collections::HashMap, path::PathBuf, str::FromStr, sync::Arc};

use super::execution::{ExecutionWriteGuard, TransactionEffect};
use super::resources::SessionResourcesImpl;
use database::SqliteSessionDatabase;
use failure::is_persistence_uncertain;

pub(in crate::sessions) use failure::execution_failure;
/// 提交阶段/领域失败映射：由门面测试驱动真实写入准入，生产路径在 `session_data`
/// 与 `compaction` 内部直接引用。
#[cfg(test)]
pub(in crate::sessions) use failure::{commit_failure, write_failure};
pub(in crate::sessions) use local::LocalExecution;
#[cfg(test)]
use schema::CURRENT_SCHEMA_VERSION;
/// 数据面实现：生产组合从 [`LocalExecution::data_port`] 取得它，本重导出供测试夹具直接命名。
#[cfg(test)]
pub(crate) use session_data::SqliteSessionData;

#[cfg(test)]
use connection::{classify_shape_probe_failure, REQUIRED_MESSAGE_COLUMNS, REQUIRED_THREAD_COLUMNS};
#[cfg(test)]
use sqlx::sqlite::SqliteConnectOptions;

/// 基于 SQLite 的 ThreadStore 实现（迁移桥）
///
/// 使用 WAL 模式提升并发读性能，sqlx SqlitePool 连接池管理并发。连接池、canonical
/// 路径与执行租约登记都归共享库句柄所有：数据面与执行面不会各自持有一条连接真相。
pub struct SqliteThreadStore {
    database: Arc<SqliteSessionDatabase>,
}

impl SqliteThreadStore {
    /// 打开或创建会话数据库，原地升级已知旧 schema 并保留历史数据。
    pub async fn new(db_path: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            database: Arc::new(SqliteSessionDatabase::open(db_path).await?),
        })
    }

    /// 以 SQLite read-only capability 打开已存在的数据库；不创建目录、库或 schema。
    pub async fn open_existing_read_only(
        db_path: impl AsRef<std::path::Path>,
    ) -> std::result::Result<Self, ReadOnlyThreadStoreError> {
        Ok(Self {
            database: Arc::new(SqliteSessionDatabase::open_existing_read_only(db_path).await?),
        })
    }

    /// 默认数据库位置 `~/.peri/threads/threads.db`；不创建目录、数据库或连接。
    pub fn default_database_path() -> Result<PathBuf> {
        SqliteSessionDatabase::default_database_path()
    }

    /// 使用默认路径 `~/.peri/threads/threads.db` 创建
    pub async fn default_path() -> Result<Self> {
        Self::new(Self::default_database_path()?).await
    }

    /// 关闭连接池并等待全部连接释放（见 `SqliteSessionDatabase::close`）。
    pub async fn close(&self) {
        self.database.close().await;
    }

    /// 夹具装配（唯一调用点 `open_store_and_facade_for_tests`）：迁移桥与门面共用
    /// **同一个库句柄**（同一 pool、同一 owner 登记表）。
    ///
    /// 两面对同一个库既有两种句柄，又不能各开一条连接真相：桥侧的
    /// `acquire_execution_lease` 与门面的 owner 校验必须落在同一份登记表上，否则同一个
    /// 进程里会互相判成「无主」。E 阶段桥退出后，本函数一并删除，只留门面构造。
    pub(in crate::sessions) async fn open_shared(
        db_path: impl Into<PathBuf>,
    ) -> Result<(Self, SessionResourcesImpl)> {
        let database = Arc::new(SqliteSessionDatabase::open(db_path).await?);
        Ok((
            Self {
                database: Arc::clone(&database),
            },
            SessionResourcesImpl::from_local(LocalExecution::from_shared_database(database)),
        ))
    }

    /// 本桥的写入准入：先按**本机库自己的**读法取执行面要用的会话事实（绑定字节、这棵树
    /// 有没有绑定、树根），再按同一套 root-only 判定取门禁。
    ///
    /// 桥与本机数据面共用同一个库句柄（同一份连接真相），因此这里用本机数据面的读法
    /// （[`SqliteSessionDatabase::local_session_facts`]）构造事实，而不是再写一份读法。
    /// 远端组合不经过本桥：那时本机没有会话行，事实只能由门面从数据端口取。
    async fn write_guard(&self, id: &ThreadId) -> Result<Option<ExecutionWriteGuard>> {
        let facts = self.database.local_session_facts(id).await?;
        self.database.require_execution_lease(id, &facts).await
    }
}

// ── ThreadStore impl（迁移桥转发） ─────────────────────────────────────────────

#[async_trait]
impl ThreadStore for SqliteThreadStore {
    async fn resolve_workspace(
        &self,
        cwd: &std::path::Path,
    ) -> Result<peri_acp_types::workspace::ResolvedWorkspace> {
        self.database.resolve_workspace_impl(cwd).await
    }
    async fn create_bound_thread(
        &self,
        meta: ThreadMeta,
        workspace: &peri_acp_types::workspace::ResolvedWorkspace,
    ) -> Result<ThreadId> {
        self.database
            .create_bound_thread_impl(meta, workspace)
            .await
    }
    async fn load_session_binding(
        &self,
        id: &ThreadId,
    ) -> Result<Option<peri_acp_types::workspace::SessionBinding>> {
        self.database.load_session_binding_impl(id).await
    }
    async fn validate_session_binding(
        &self,
        id: &ThreadId,
    ) -> Result<peri_acp_types::workspace::ResolvedWorkspace> {
        self.database.validate_session_binding_impl(id).await
    }
    async fn reassert_session_binding(
        &self,
        id: &ThreadId,
    ) -> Result<peri_acp_types::workspace::ResolvedWorkspace> {
        self.database.reassert_session_binding_impl(id).await
    }
    async fn adopt_legacy_thread(
        &self,
        id: &ThreadId,
        saved_cwd: &str,
        workspace: &peri_acp_types::workspace::ResolvedWorkspace,
        frozen_snapshot: &str,
    ) -> Result<()> {
        self.database
            .adopt_legacy_thread_impl(id, saved_cwd, workspace, frozen_snapshot)
            .await
    }
    async fn list_scoped_threads(
        &self,
        query: &peri_acp_types::workspace::ScopedThreadQuery,
    ) -> Result<peri_acp_types::workspace::ScopedThreadPage> {
        self.database.list_scoped_threads_impl(query).await
    }
    async fn acquire_execution_lease(
        &self,
        id: &ThreadId,
    ) -> Result<std::sync::Arc<dyn peri_acp_types::workspace::SessionExecutionLease>> {
        let facts = self.database.local_session_facts(id).await?;
        self.database.acquire_execution_lease_impl(id, &facts).await
    }

    async fn create_thread(&self, meta: ThreadMeta) -> Result<ThreadId> {
        let id = meta.id.clone();
        let mut transaction = self.database.pool.begin_with("BEGIN IMMEDIATE").await?;
        let workspace_id = workspace_identity::identity_for_new_thread(
            &mut transaction,
            meta.parent_thread_id.as_deref(),
            None,
            &meta.cwd,
        )
        .await?;
        sqlx::query(
            "INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count,
                parent_thread_id, snapshot_at_message_id, hidden, cancel_policy, config, agent_status, workspace_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
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
        .bind(meta.agent_status.as_str())
        .bind(workspace_id.to_string())
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
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
                .fetch_all(&self.database.pool)
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
        let write_guard = self.write_guard(id).await?;
        let result = async {
            if payloads.is_empty() {
                return Ok(());
            }
            let mut tx = self.database.pool.begin().await?;
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
            .bind(peri_time::now_utc_rfc3339())
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
        self.database.load_payloads(id).await
    }

    async fn load_meta(&self, id: &ThreadId) -> Result<ThreadMeta> {
        let row: ThreadRow = match sqlx::query_as(AssertSqlSafe(format!(
            "SELECT {THREAD_META_COLUMNS} FROM threads t WHERE t.id = ?1"
        )))
        .bind(id.as_str())
        .fetch_one(&self.database.pool)
        .await
        {
            Ok(row) => row,
            Err(error) if self.database.read_only => {
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
            row.12,
        );
        if self.database.read_only {
            result.map_err(|_| {
                ReadOnlyThreadStoreError::from_kind(ReadOnlyStoreErrorKind::CorruptSessionData)
                    .into()
            })
        } else {
            result
        }
    }

    async fn update_meta(&self, id: &ThreadId, meta: ThreadMeta) -> Result<()> {
        let write_guard = self.write_guard(id).await?;
        let result = async {
            if self.database.load_session_binding_impl(id).await?.is_some() {
                let original: (String, Option<String>) =
                    sqlx::query_as("SELECT cwd, parent_thread_id FROM threads WHERE id = ?")
                        .bind(id)
                        .fetch_one(&self.database.pool)
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
                cancel_policy = ?8, config = ?9, agent_status = ?10
             WHERE id = ?11",
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
            .bind(meta.agent_status.as_str())
            .bind(id.as_str())
            .execute(&self.database.pool)
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
                .fetch_one(&self.database.pool)
                .await?;
        Ok(row.0)
    }

    async fn store_frozen_snapshot_if_absent(&self, id: &ThreadId, snapshot: &str) -> Result<bool> {
        let write_guard = self.write_guard(id).await?;
        let result = async {
            let result = sqlx::query(
                "UPDATE threads SET frozen_context = ?1 WHERE id = ?2 AND frozen_context IS NULL",
            )
            .bind(snapshot)
            .bind(id.as_str())
            .execute(&self.database.pool)
            .await?;
            if result.rows_affected() == 1 {
                return Ok(true);
            }
            let row: Option<(Option<String>,)> =
                sqlx::query_as("SELECT frozen_context FROM threads WHERE id = ?1")
                    .bind(id.as_str())
                    .fetch_optional(&self.database.pool)
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
        .fetch_all(&self.database.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                meta_from_row(
                    row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8, row.9, row.10,
                    row.11, row.12,
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
        .fetch_all(&self.database.pool)
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
        let write_guard = self.write_guard(id).await?;
        let mut effect = TransactionEffect::new();
        let mut deleted: Vec<String> = Vec::new();
        let result = async {
            let mut tx = self.database.pool.begin().await?;
            // 级联删除整个线程树：hidden 子 agent 线程沿 parent_thread_id 挂链，
            // 若不递归删除会留下永远无法通过 UI/协议访问的孤儿数据。
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
                // `messages` 与 `session_bindings` 同样显式删除，**不再**依赖
                // `ON DELETE CASCADE`：那份级联只在 SQLite 上存在，远端执行器没有
                // （见 `session_rows::THREAD_CHILD_DELETES`）。先子后父，顺序与数据面
                // 的 delete_tree 及远端一致。
                session_rows::delete_thread_child_rows(&mut tx, tid).await?;
                sqlx::query(session_rows::DELETE_THREAD_SQL)
                    .bind(tid)
                    .execute(&mut *tx)
                    .await?;
            }
            effect.enter_commit();
            tx.commit().await?;
            effect.commit_succeeded();
            deleted = to_delete;
            Ok(())
        }
        .await;
        // 只有效果确定才结清写入准入：提交自身的失败落在证明之外，交给 Drop 留下未决证据。
        effect.settle(write_guard);
        let _ = deleted;
        result
    }

    async fn update_title(&self, id: &ThreadId, title: &str) -> Result<()> {
        let write_guard = self.write_guard(id).await?;
        let result = async {
            let now = peri_time::now_utc_rfc3339();
            sqlx::query("UPDATE threads SET title = ?1, updated_at = ?2 WHERE id = ?3")
                .bind(title)
                .bind(&now)
                .bind(id.as_str())
                .execute(&self.database.pool)
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
        let write_guard = self.write_guard(thread_id).await?;
        let result = async {
            self.database
                .store_inherited_context(thread_id, inherited)
                .await
        }
        .await;
        if let Some(guard) = write_guard {
            guard.finish();
        }
        result
    }

    async fn load_inherited_context(&self, thread_id: &ThreadId) -> Result<InheritedContext> {
        self.database.load_inherited_context(thread_id).await
    }

    async fn load_context_payloads(&self, thread_id: &ThreadId) -> Result<Vec<PersistedPayload>> {
        self.database.load_context_payloads(thread_id).await
    }

    async fn load_context(&self, thread_id: &ThreadId) -> Result<Vec<BaseMessage>> {
        self.database.load_context(thread_id).await
    }

    async fn list_child_threads(&self, parent_id: &ThreadId) -> Result<Vec<ThreadMeta>> {
        self.database.list_child_threads(parent_id).await
    }

    async fn list_session_threads(&self, root_id: &ThreadId) -> Result<Vec<ThreadMeta>> {
        self.database.list_session_threads(root_id).await
    }

    async fn update_thread_status(&self, id: &ThreadId, status: &str) -> Result<()> {
        let write_guard = self.write_guard(id).await?;
        let result = async {
            // 关键约束：参数字符串必须经 FromStr 解析，非法值直接返回错误，不静默 fallback
            let status = AgentStatus::from_str(status)
                .with_context(|| format!("非法 agent_status 值: {status:?}"))?;
            let now = peri_time::now_utc_rfc3339();
            sqlx::query("UPDATE threads SET agent_status = ?1, updated_at = ?2 WHERE id = ?3")
                .bind(status.as_str())
                .bind(&now)
                .bind(id.as_str())
                .execute(&self.database.pool)
                .await?;
            Ok(())
        }
        .await;
        if let Some(guard) = write_guard {
            guard.finish();
        }
        result
    }

    async fn delete_messages(
        &self,
        thread_id: &ThreadId,
        message_ids: &[peri_acp_types::messages::MessageId],
    ) -> Result<()> {
        let write_guard = self.write_guard(thread_id).await?;
        let result =
            async { compaction::delete_messages(&self.database, thread_id, message_ids).await }
                .await;
        if let Some(guard) = write_guard {
            // 提交阶段的失败是未决持久化：范围留给 `Drop`，不在这里结清。
            if !result.as_ref().err().is_some_and(is_persistence_uncertain) {
                guard.finish();
            }
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
                .fetch_optional(&self.database.pool)
                .await?;
        let write_guard = match owner {
            Some((id,)) => self.write_guard(&id).await?,
            None => None,
        };

        let result =
            async { compaction::update_message_flags(&self.database, message_id, flags).await }
                .await;
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
        lifecycle: &CompactionChange,
    ) -> Result<()> {
        let write_guard = self.write_guard(thread_id).await?;
        let result = async {
            compaction::commit_compaction_lifecycle(&self.database, thread_id, lifecycle).await
        }
        .await;
        if let Some(guard) = write_guard {
            // 提交阶段的失败是未决持久化：范围留给 `Drop`，不在这里结清。
            if !result.as_ref().err().is_some_and(is_persistence_uncertain) {
                guard.finish();
            }
        }
        result
    }

    async fn load_message_flags(
        &self,
        thread_id: &ThreadId,
    ) -> Result<HashMap<peri_acp_types::messages::MessageId, MessageFlags>> {
        compaction::load_message_flags(&self.database, thread_id).await
    }

    async fn delete_messages_since(
        &self,
        thread_id: &ThreadId,
        message_id: &peri_acp_types::messages::MessageId,
    ) -> Result<()> {
        let write_guard = self.write_guard(thread_id).await?;
        let result = async {
            compaction::delete_messages_since(&self.database, thread_id, message_id).await
        }
        .await;
        if let Some(guard) = write_guard {
            // 提交阶段的失败是未决持久化：范围留给 `Drop`，不在这里结清。
            if !result.as_ref().err().is_some_and(is_persistence_uncertain) {
                guard.finish();
            }
        }
        result
    }
}

#[cfg(test)]
#[path = "sqlite_store_test.rs"]
mod tests;

#[cfg(test)]
#[path = "sqlite_store/read_only_test.rs"]
mod read_only_tests;

#[cfg(test)]
#[path = "sqlite_inherited_context_test.rs"]
mod inherited_context_tests;

#[cfg(test)]
#[path = "sqlite_store/legacy_test.rs"]
mod legacy_tests;

#[cfg(test)]
#[path = "sqlite_store/session_data_test.rs"]
mod session_data_tests;

#[cfg(test)]
#[path = "sqlite_store/thread_child_delete_test.rs"]
mod thread_child_delete_tests;

#[cfg(test)]
#[path = "sqlite_store/schema_v7_test.rs"]
mod schema_v7_tests;

#[cfg(test)]
#[path = "sqlite_store/schema_v10_test.rs"]
mod schema_v10_tests;

#[cfg(test)]
#[path = "sqlite_store/schema_v11_test.rs"]
mod schema_v11_tests;

#[cfg(test)]
#[path = "sqlite_store/session_id_environment_test.rs"]
mod session_id_environment_tests;
