//! SQLite adapter assembly and read-only fallback for native deployments.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Result;
use peri_acp_types::workspace::WorkspaceError;

use super::Resources;

impl Resources {
    /// 本机 locator 的既有装配：写打开失败且历史仍可读时降级为只读打开。
    pub(super) async fn open_local(db_path: Option<PathBuf>) -> Result<Self> {
        Self::open_with_default(
            db_path,
            crate::sessions::SessionResourcesImpl::default_database_path,
        )
        .await
    }

    /// 显式只读 locator：复用既有只读 seam，不开库、不建目录、不迁移 schema。
    pub(super) async fn open_local_read_only(db_path: Option<PathBuf>) -> Result<Self> {
        let describe = match &db_path {
            Some(path) => format!("指定 SQLite 数据库 {}", path.display()),
            None => "默认 SQLite 数据库 ~/.peri/threads/threads.db".to_owned(),
        };
        // 只读 seam 的错误分类（库不存在/不可读/schema 不兼容/数据损坏）在这里仍是
        // 类型化事实：路径只加在 context 上，`ReadOnlyThreadStoreError` 留在 source chain 里
        // 可按 kind 判别；元数据命令的九字段 DTO 与退出码映射保留在消费侧（D-04）。
        crate::sessions::open_session_resources_read_only(db_path)
            .await
            .map(Self::from_facade)
            .map_err(|error| anyhow::Error::new(error).context(format!("无法只读打开{describe}")))
    }

    /// 打开资源：写打开失败且历史仍可读时降级为只读打开。
    ///
    /// 「写打开失败」不等于「历史不可读」：schema 锁被其他实例占住、库文件不可写时，
    /// 只读打开仍能列出与读取历史。这种失败不再挡住进入——降级只记 warning，不向用户
    /// 报错。降级不假装可写：资源门禁与 SQLite 只读连接共同拒绝写入，调用方据此得到真实失败而不是看似成功的写入。
    ///
    /// 只读打开也失败时返回写打开的原错误：那才是真的读不了，不能被降级掩盖。
    pub(super) async fn open_with_default(
        db_path: Option<PathBuf>,
        default_database_path: impl FnOnce() -> Result<PathBuf>,
    ) -> Result<Self> {
        let (path, describe) = match db_path {
            Some(path) => {
                let describe = format!("指定 SQLite 数据库 {}", path.display());
                (path, describe)
            }
            None => (
                default_database_path()?,
                "默认 SQLite 数据库 ~/.peri/threads/threads.db".to_owned(),
            ),
        };
        match crate::sessions::open_facade(path.clone()).await {
            Ok(facade) => Ok(Self::new(facade)),
            Err(error) => {
                let message = format!("无法打开{describe}: {error}");
                match Self::open_read_only(&path, &error).await {
                    Some(resources) => Ok(resources),
                    None => Err(anyhow::anyhow!(message)),
                }
            }
        }
    }

    fn new(facade: crate::sessions::SessionResourcesImpl) -> Self {
        Self::from_facade(Arc::new(facade))
    }

    /// 迁移期只读打开的降级：只读打开成功即返回只读资源，否则返回 `None` 让调用方上报
    /// 写打开的原错误。
    async fn open_read_only(path: &Path, error: &anyhow::Error) -> Option<Self> {
        if !degradable_open_failure(error) {
            return None;
        }
        let facade = crate::sessions::open_facade_read_only(path).await.ok()?;
        tracing::warn!(
            path = %path.display(),
            error = %error,
            "session store opened read-only: writable open failed"
        );
        Some(Self::new(facade))
    }
}

/// 写打开失败是否允许降级为只读打开。
///
/// 这个过滤只在写打开走到版本判定（`schema::inspect`）时生效：不认识的 schema 不是
/// 可恢复的占用，按类型化错误保持原样失败。写打开在版本判定之前就失败时（schema 锁
/// 被占、库文件或 WAL 侧车文件不可写），只读打开仅按读取兼容的列形状把关
/// （`probe_load_meta_shape`），不再复查 `user_version`——由更新构建写入、列形状兼容
/// 的库因此可能被只读读取；该读取不迁移也不写入，写入仍按 `ReadOnlyStore` 拒绝。
/// 其余失败都只影响写入，历史仍可读。
fn degradable_open_failure(error: &anyhow::Error) -> bool {
    !matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::UnsupportedSchemaVersion { .. })
            | Some(WorkspaceError::UnsupportedDatabaseSchema)
    )
}
