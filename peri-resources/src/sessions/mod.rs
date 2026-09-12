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

/// 只解析默认数据库位置；不创建目录、数据库或连接。
fn default_database_path() -> Option<PathBuf> {
    dirs_next::home_dir().map(|home| home.join(".peri").join("threads").join("threads.db"))
}

/// 只读打开显式路径或默认路径下已存在的 thread database。
pub async fn open_thread_store_read_only(
    db_path: Option<PathBuf>,
) -> Result<Arc<dyn ThreadStore>, ReadOnlyThreadStoreError> {
    let path = match db_path {
        Some(path) => path,
        None => default_database_path().ok_or(ReadOnlyThreadStoreError::Internal)?,
    };
    let store = SqliteThreadStore::open_existing_read_only(path).await?;
    Ok(Arc::new(store))
}

// dirs-next uses the Windows profile known folder, so HOME cannot isolate these tests there.
#[cfg(all(test, unix))]
#[path = "default_path_test.rs"]
mod default_path_tests;
