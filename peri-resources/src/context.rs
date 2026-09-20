//! Resources context — 外部系统访问通道的唯一实例化入口。
//!
//! 本迁移点先落 context 形状 + 唯一实例化入口（TUI 启动处）；
//! Controller/Runtime 建成后消费方随 L2/L3/L5 跟进接入（属预期过渡态，
//! 接口按目标态设计，避免二次返工）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use peri_acp_types::store::ThreadStore;
use peri_acp_types::workspace::WorkspaceError;

use crate::sessions::SqliteThreadStore;

/// 外部系统资源门面
#[derive(Clone)]
pub struct Resources {
    thread_store: Arc<dyn ThreadStore>,
}

impl Resources {
    /// 打开全部资源（当前为会话存储）。
    ///
    /// 默认路径 `~/.peri/threads/threads.db` 写打开失败时会降级为只读打开，
    /// 见 [`Resources::open_with`]。
    pub async fn open() -> Result<Self> {
        Self::open_with(None).await
    }

    /// 按显式路径打开全部资源（当前为会话存储）。
    ///
    /// `Some(path)` 使用指定路径；`None` 使用默认路径
    /// `~/.peri/threads/threads.db`。两条路径都先尝试写打开，写打开不可用时再尝试
    /// 只读打开；只有两者都失败才返回包含路径的错误——不静默 fallback 到共享临时
    /// 数据库。
    pub async fn open_with(db_path: Option<PathBuf>) -> Result<Self> {
        Self::open_with_default(db_path, SqliteThreadStore::default_database_path).await
    }

    /// 打开资源：写打开失败且历史仍可读时降级为只读打开。
    ///
    /// 「写打开失败」不等于「历史不可读」：schema 锁被其他实例占住、库文件不可写时，
    /// 只读打开仍能列出与读取历史。这种失败不再挡住进入——降级只记 warning，不向用户
    /// 报错。降级不假装可写：只读 store 自身拒绝写入（执行所有权与 SQLite 只读连接
    /// 双重把关），调用方据此得到真实失败而不是看似成功的写入。
    ///
    /// 只读打开也失败时返回写打开的原错误：那才是真的读不了，不能被降级掩盖。
    async fn open_with_default(
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
        match SqliteThreadStore::new(path.clone()).await {
            Ok(store) => Ok(Self::read_write(store)),
            Err(error) => {
                let message = format!("无法打开{describe}: {error}");
                match Self::open_read_only(&path, &error).await {
                    Some(resources) => Ok(resources),
                    None => Err(anyhow::anyhow!(message)),
                }
            }
        }
    }

    fn read_write(store: SqliteThreadStore) -> Self {
        Self {
            thread_store: Arc::new(store),
        }
    }

    /// 写打开失败后的降级：只读打开成功即返回只读资源，否则返回 `None` 让调用方上报
    /// 写打开的原错误。
    async fn open_read_only(path: &Path, error: &anyhow::Error) -> Option<Self> {
        if !degradable_open_failure(error) {
            return None;
        }
        let store = SqliteThreadStore::open_existing_read_only(path)
            .await
            .ok()?;
        tracing::warn!(
            path = %path.display(),
            error = %error,
            "session store opened read-only: writable open failed"
        );
        Some(Self {
            thread_store: Arc::new(store),
        })
    }

    /// 会话存储句柄（trait object，供 Agent/ACP/TUI 注入）
    pub fn thread_store(&self) -> Arc<dyn ThreadStore> {
        self.thread_store.clone()
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

#[cfg(test)]
#[path = "context_test.rs"]
mod tests;
