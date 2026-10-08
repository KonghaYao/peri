//! 本机工作区发现、归属与绑定复核；执行所有权由上层管理。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use peri_acp_types::thread::ThreadId;
use peri_acp_types::workspace::{ResolvedWorkspace, SessionBinding};

use super::connection::ReadOnlyThreadStoreError;
use super::database::SqliteSessionDatabase;
use super::session_data::SqliteSessionData;
use crate::sessions::local_port::LocalExecutionPort;

/// 本机执行面的句柄：与数据面共用同一个库（同一条连接真相）。
#[derive(Clone)]
pub(in crate::sessions) struct LocalExecution {
    database: Arc<SqliteSessionDatabase>,
}

impl LocalExecution {
    pub(in crate::sessions) async fn open(db_path: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            database: Arc::new(SqliteSessionDatabase::open(db_path).await?),
        })
    }

    /// 复用既有库句柄：迁移期桥与门面必须指向同一份连接。
    pub(in crate::sessions) fn from_shared_database(database: Arc<SqliteSessionDatabase>) -> Self {
        Self { database }
    }

    pub(in crate::sessions) async fn open_existing_read_only(
        db_path: impl AsRef<Path>,
    ) -> std::result::Result<Self, ReadOnlyThreadStoreError> {
        Ok(Self {
            database: Arc::new(SqliteSessionDatabase::open_existing_read_only(db_path).await?),
        })
    }

    /// 默认数据库位置 `~/.peri/threads/threads.db`；不创建目录、数据库或连接。
    pub(in crate::sessions) fn default_database_path() -> Result<PathBuf> {
        SqliteSessionDatabase::default_database_path()
    }

    /// 数据面句柄：只有门面持有它，业务侧拿不到。
    pub(in crate::sessions) fn data_port(&self) -> SqliteSessionData {
        SqliteSessionData::new(Arc::clone(&self.database))
    }

    pub(in crate::sessions) fn is_read_only(&self) -> bool {
        self.database.is_read_only()
    }

    /// 测试用：直接读库内事实（生产侧由各行为自己的后置条件覆盖）。
    #[cfg(test)]
    pub(in crate::sessions) fn pool(&self) -> &sqlx::SqlitePool {
        &self.database.pool
    }

    // ── 发现与归属 ────────────────────────────────────────────────────────────

    /// 解析本机执行目录并刷新它的归属证据（只读打开时按 `WorkspaceError::ReadOnlyStore` 失败）。
    pub(in crate::sessions) async fn resolve_workspace(
        &self,
        cwd: &Path,
    ) -> Result<ResolvedWorkspace> {
        self.database.resolve_workspace_impl(cwd).await
    }

    /// 用调用方给出的绑定字节做本机复核（本机组合来自 `session_bindings`，远端组合来自
    /// 远端会话行）。
    ///
    /// `full` 为真时叠一次完整发现快照比对（一次准入的权威复核），否则只查关系与关键
    /// 文件对象（准入内的复核）。两种模式走的是同一套判定，因此「能不能执行」与
    /// 「绑定向哪里」不会出现两套结论。
    pub(in crate::sessions) async fn validate_binding_value(
        &self,
        binding: &SessionBinding,
        full: bool,
    ) -> Result<ResolvedWorkspace> {
        self.database
            .validate_binding_value_impl(binding, full)
            .await
    }

    /// legacy 来源证据：无绑定、无父会话、无 frozen，且保存的绝对 cwd 落在本机已知的
    /// 工作区路径内。
    ///
    /// 这是「这条历史来自本机某个已知目录」的证据，不是「可以执行」的许可；接纳本身
    /// 仍由数据面在写事务内复核（保存路径一致、归属关系一致、既有绑定只校验不覆盖）。
    pub(in crate::sessions) async fn legacy_confirmed(&self, id: &ThreadId) -> Result<bool> {
        let row: Option<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT cwd, parent_thread_id, frozen_context FROM threads WHERE id = ?1",
        )
        .bind(id.as_str())
        .fetch_optional(&self.database.pool)
        .await?;
        let Some((cwd, parent, frozen)) = row else {
            return Ok(false);
        };
        if parent.is_some() {
            return Ok(false);
        }
        if self.database.load_session_binding_impl(id).await?.is_some() {
            return Ok(false);
        }
        if super::session_data::validate_unbound_legacy_frozen(frozen.as_deref()).is_err() {
            return Ok(false);
        }
        let cwd = PathBuf::from(cwd);
        if !cwd.is_absolute() {
            return Ok(false);
        }
        // 两侧是不同时刻写入的字符串：同一目录可能一侧已解析、另一侧仍是符号链接
        // 路径（macOS 的 /var 与 /private/var）。按字面比较会把本机 legacy 误判成
        // 外来会话，因此按文件系统事实比较。
        let Ok(cwd) = cwd.canonicalize() else {
            return Ok(false);
        };
        let roots: Vec<(String,)> = sqlx::query_as("SELECT path FROM workspaces")
            .fetch_all(&self.database.pool)
            .await?;
        Ok(roots.iter().any(|(root,)| {
            Path::new(root)
                .canonicalize()
                .map(|root| cwd.starts_with(root))
                .unwrap_or(false)
        }))
    }
}

#[async_trait::async_trait]
impl LocalExecutionPort for LocalExecution {
    fn is_read_only(&self) -> bool {
        self.is_read_only()
    }

    async fn resolve_workspace(&self, cwd: &Path) -> Result<ResolvedWorkspace> {
        self.resolve_workspace(cwd).await
    }

    async fn validate_binding_value(
        &self,
        binding: &SessionBinding,
        full: bool,
    ) -> Result<ResolvedWorkspace> {
        self.validate_binding_value(binding, full).await
    }

    async fn legacy_confirmed(&self, id: &ThreadId) -> Result<bool> {
        self.legacy_confirmed(id).await
    }

    #[cfg(test)]
    fn sqlite_pool(&self) -> Option<&sqlx::SqlitePool> {
        Some(self.pool())
    }
}
