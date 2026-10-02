//! peri-sessions — 会话持久化子模块（自 peri-agent/src/thread 迁入）。
//!
//! 直操 sqlite：`SqliteThreadStore` 为生产实现；`FilesystemThreadStore` 为纯测试用途。
//! 契约类型（`ThreadStore` trait / `ThreadMeta` / `BaseMessage` / `MessageFlags`）位于
//! peri-acp-types（接口契约归 peri-acp-types），本模块仅实现，不解释业务语义。

mod canonical;
mod data;
mod discovery;
mod execution;
mod filesystem;
mod local_port;
mod machine;
pub use machine::adopt_file_identity;
pub use machine::current as current_machine_id;
mod open;
mod remote;
mod schema_cleanup;
// `CredentialError` 只做 crate 内最小 re-export：分类（`classify_open_failure`）要按类型认出
// 「凭证来源不可用」，但凭证类型不进公共 API，也不向消费侧暴露 SDK 类型或凭证值。
pub(crate) use remote::{open_remote, CredentialError, RemoteEndpoint};
mod resources;
mod sqlite_store;

pub use filesystem::FilesystemThreadStore;
pub use resources::SessionResourcesImpl;
pub use sqlite_store::{ReadOnlyStoreErrorKind, ReadOnlyThreadStoreError, SqliteThreadStore};

pub(crate) use open::{AccessIntent, LocatorError, ResolvedLocator, SessionStoreOpenRequest};

use std::path::PathBuf;
use std::sync::Arc;

/// 只解析默认数据库位置；不创建目录、数据库或连接。
fn default_database_path() -> Option<PathBuf> {
    dirs_next::home_dir().map(|home| home.join(".peri").join("threads").join("threads.db"))
}

/// crate 内的只读打开 seam：按显式路径或默认路径打开已存在的会话库，返回门面实例。
///
/// 不创建目录、库、schema 或锁文件；只读打开的事实由
/// [`ReadOnlyThreadStoreError`] 如实报告（库不存在 / 不可读 / schema 不兼容 / 损坏）。
/// 返回具体实例只服务 crate 内装配（[`crate::context::Resources`] 的只读降级），本函数
/// 不交关闭权：实例上没有关闭路径，部署关闭权只由 `Resources` 装配入口交出。
pub(crate) async fn open_session_resources_read_only(
    db_path: Option<PathBuf>,
) -> Result<Arc<SessionResourcesImpl>, ReadOnlyThreadStoreError> {
    let path = match db_path {
        Some(path) => path,
        None => default_database_path().ok_or(ReadOnlyThreadStoreError::Internal)?,
    };
    let facade = SessionResourcesImpl::open_existing_read_only(path).await?;
    Ok(Arc::new(facade))
}

/// 生产装配：打开会话资源门面（写打开，必要时原地升级已知旧 schema）。
pub(crate) async fn open_facade(
    db_path: impl Into<PathBuf>,
) -> anyhow::Result<SessionResourcesImpl> {
    SessionResourcesImpl::open(db_path).await
}

/// [`open_facade`] 的只读版本。
pub(crate) async fn open_facade_read_only(
    db_path: &std::path::Path,
) -> Result<SessionResourcesImpl, ReadOnlyThreadStoreError> {
    SessionResourcesImpl::open_existing_read_only(db_path).await
}

/// 资源测试入口（仅测试夹具）：同一个库句柄上的裸存储与门面。
///
/// 夹具需要「逐条构造事实（create_thread / append …）+ 用门面消费」时配对打开；
/// 生产装配一律走 `open_facade`，不通过本函数取裸句柄——两个入口各自打开同一库
/// 文件会得到两份 owner 登记，本函数的存在正是为了不出现那种「第二个真相」。
pub async fn open_store_and_facade_for_tests(
    db_path: impl Into<PathBuf>,
) -> anyhow::Result<(SqliteThreadStore, SessionResourcesImpl)> {
    SqliteThreadStore::open_shared(db_path).await
}

// dirs-next uses the Windows profile known folder, so HOME cannot isolate these tests there.
#[cfg(all(test, unix))]
#[path = "default_path_test.rs"]
mod default_path_tests;

#[cfg(test)]
#[path = "open_test.rs"]
mod open_tests;
