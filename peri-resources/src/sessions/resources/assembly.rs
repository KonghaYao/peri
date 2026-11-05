//! 门面装配：打开本机库、只读打开与组合层端口装配。

use super::*;

impl SessionResourcesImpl {
    /// 打开或创建会话库，原地升级已知旧 schema 并保留历史数据。
    #[cfg(not(target_os = "emscripten"))]
    pub async fn open(db_path: impl Into<PathBuf>) -> anyhow::Result<Self> {
        Ok(Self::from_local(LocalExecution::open(db_path).await?))
    }

    /// 以只读方式打开已存在的会话库；不创建目录、库、schema 或锁文件。
    #[cfg(not(target_os = "emscripten"))]
    pub async fn open_existing_read_only(
        db_path: impl AsRef<Path>,
    ) -> Result<Self, ReadOnlyThreadStoreError> {
        Ok(Self::from_local(
            LocalExecution::open_existing_read_only(db_path).await?,
        ))
    }

    /// 默认数据库位置 `~/.peri/threads/threads.db`；不创建目录、数据库或连接。
    #[cfg(not(target_os = "emscripten"))]
    pub fn default_database_path() -> anyhow::Result<PathBuf> {
        LocalExecution::default_database_path()
    }

    /// 与迁移桥共享同一个库句柄（迁移期唯一装配点 `SqliteThreadStore::open_shared*` 使用）。
    ///
    /// 本机组合：数据面与执行面由同一个库句柄回答（同一条连接真相），两个端口因此只是
    /// 同一实现的两张面孔。
    #[cfg(not(target_os = "emscripten"))]
    pub(in crate::sessions) fn from_local(local: LocalExecution) -> Self {
        let data = Arc::new(local.data_port());
        Self::from_ports(data, Arc::new(local), SessionDataHome::LocalLibrary)
    }

    /// 组合层装配点：数据面 + 本机执行面，两者由调用方决定指向哪个后端。
    ///
    /// 门面不持有任何后端判断，只按 `home`（组合层给出的**事实**）决定 `create_session`
    /// 是一步还是两步；公开行为仍只有 [`SessionResources`] 这一套。
    pub(in crate::sessions) fn from_ports(
        data: Arc<dyn SessionDataPort>,
        local: Arc<dyn LocalExecutionPort>,
        home: SessionDataHome,
    ) -> Self {
        let lifecycle = Lifecycle::new();
        let gate = MutationGate::new(data, local, lifecycle.clone(), home);
        Self {
            gate,
            lifecycle,
            close_confirm: tokio::sync::Mutex::new(()),
            credential_operations: Arc::new(tokio::sync::RwLock::new(())),
            home,
        }
    }
}
