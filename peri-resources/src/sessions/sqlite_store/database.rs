//! 会话库的私有所有权：连接池、访问模式、canonical 路径与执行租约登记。
//!
//! 数据面（[`super::session_data::SqliteSessionData`]）与执行/登记面
//! （[`super::SqliteThreadStore`]）共用本结构，因此同一个库只有一条连接真相：
//! 一面写入的数据，另一面在同一事务语义下立即可见，不需要第二份 pool 或第二个
//! 数据库文件。
//!
//! 事务、CAS、连接与锁文件都留在这里的实现里，不跨 `SessionDataPort` 暴露。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use peri_acp_types::thread::ThreadId;
use sqlx::SqlitePool;

use super::execution::ExecutionLease;

/// 本机 SQLite 会话库的唯一 owner。
///
/// 迁移期可见性放宽到 `crate::sessions`：桥与门面的共享构造点（`sqlite_store.rs`
/// 的 `open_shared*`）与门面实现需要同一个句柄类型；字段仍只在 `sqlite_store` 内可见。
pub(in crate::sessions) struct SqliteSessionDatabase {
    pub(super) pool: SqlitePool,
    pub(super) read_only: bool,
    #[cfg(test)]
    pub(super) db_path: PathBuf,
    /// 保留当前运行句柄，直到明确关闭，避免遗失调用方引用后绕过未决写入门禁。
    /// 键是 `thread_id` 原文（v10 之后只有这一个执行域）。
    pub(super) execution_leases: Mutex<HashMap<ThreadId, Arc<ExecutionLease>>>,
}

impl SqliteSessionDatabase {
    pub(super) fn new(pool: SqlitePool, read_only: bool, _db_path: PathBuf) -> Self {
        Self {
            pool,
            read_only,
            #[cfg(test)]
            db_path: _db_path,
            execution_leases: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn is_read_only(&self) -> bool {
        self.read_only
    }
}
