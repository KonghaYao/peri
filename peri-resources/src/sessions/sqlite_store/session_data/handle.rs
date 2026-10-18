//! 数据面句柄的身份与构造。
//!
//! 字段可见性保持在 `session_data` 及其子模块内；构造入口供 `sqlite_store` 装配层使用。

use super::*;

/// 同一份 [`SqliteSessionDatabase`] 的数据面句柄。
///
/// `closed` 是共享的：门面发给 child resume 认领 handle 的副本与门面自身看到同一个
/// 关闭状态，关闭一个即关闭全部写入入口。
#[derive(Clone)]
pub(crate) struct SqliteSessionData {
    pub(super) database: Arc<SqliteSessionDatabase>,
    /// 端口关闭后不再接受新写入；读取不受影响（历史仍可解释）。
    pub(super) closed: Arc<AtomicBool>,
}

impl SqliteSessionData {
    pub(crate) fn new(database: Arc<SqliteSessionDatabase>) -> Self {
        Self {
            database,
            closed: Arc::new(AtomicBool::new(false)),
        }
    }
}
