//! peri-sessions — 会话持久化子模块（自 peri-agent/src/thread 迁入）。
//!
//! 直操 sqlite：`SqliteThreadStore` 为生产实现；`FilesystemThreadStore` 为纯测试用途。
//! 契约类型（`ThreadStore` trait / `ThreadMeta` / `BaseMessage` / `MessageFlags`）位于
//! peri-acp-types（接口契约归 peri-acp-types），本模块仅实现，不解释业务语义。

mod filesystem;
mod sqlite_store;

pub use filesystem::FilesystemThreadStore;
pub use sqlite_store::{ReadOnlyStoreErrorKind, ReadOnlyThreadStoreError, SqliteThreadStore};

use std::path::PathBuf;
use std::sync::Arc;

use peri_acp_types::store::ThreadStore;

/// 只读打开显式路径或默认路径下已存在的 thread database。
pub async fn open_thread_store_read_only(
    db_path: Option<PathBuf>,
) -> Result<Arc<dyn ThreadStore>, ReadOnlyThreadStoreError> {
    let path = match db_path {
        Some(path) => path,
        None => dirs_next::home_dir()
            .map(|home| home.join(".peri").join("threads").join("threads.db"))
            .ok_or(ReadOnlyThreadStoreError::Internal)?,
    };
    let store = SqliteThreadStore::open_existing_read_only(path).await?;
    Ok(Arc::new(store))
}
