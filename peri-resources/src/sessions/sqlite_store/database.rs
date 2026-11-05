//! 会话库的私有所有权：连接池、访问模式与访问模式。
//!
//! 数据面（[`super::session_data::SqliteSessionData`]）与执行/登记面
//! （[`super::SqliteThreadStore`]）共用本结构，因此同一个库只有一条连接真相：
//! 一面写入的数据，另一面在同一事务语义下立即可见，不需要第二份 pool 或第二个
//! 数据库文件。
//!
//! 事务、CAS、连接与锁文件都留在这里的实现里，不跨 `SessionDataPort` 暴露。

use std::path::PathBuf;

use sqlx::SqlitePool;

use super::row_mapping::{
    RECOVERY_SCOPE_FILTER, RECOVERY_SCOPE_FILTER_LEGACY, RECOVERY_SCOPE_PROJECTION,
    RECOVERY_SCOPE_PROJECTION_LEGACY, THREAD_META_COLUMNS, THREAD_META_COLUMNS_LEGACY,
};

/// 本机 SQLite 会话库的唯一 owner。
///
/// 迁移期可见性放宽到 `crate::sessions`：桥与门面的共享构造点（`sqlite_store.rs` 的
/// `open_shared*`）与门面实现需要同一个句柄类型；字段仍只在 `sqlite_store` 内可见。
pub(in crate::sessions) struct SqliteSessionDatabase {
    pub(super) pool: SqlitePool,
    pub(super) read_only: bool,
    /// metadata 投影：只读打开可能停在 v14 之前，旧库没有两列工作快照。
    pub(super) meta_columns: &'static str,
    /// 旧代际形状位：工作快照两列不存在，所有读它们的投影必须一起退化。
    pub(super) legacy_meta: bool,
    #[cfg(test)]
    pub(super) db_path: PathBuf,
}

impl SqliteSessionDatabase {
    pub(super) fn new(pool: SqlitePool, read_only: bool, _db_path: PathBuf) -> Self {
        Self {
            pool,
            read_only,
            meta_columns: THREAD_META_COLUMNS,
            legacy_meta: false,
            #[cfg(test)]
            db_path: _db_path,
        }
    }

    pub(super) fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// 本库可用的 metadata 投影；只读打开的旧库用默认值占位两列工作快照。
    pub(super) fn thread_meta_columns(&self) -> &'static str {
        self.meta_columns
    }

    /// 恢复范围查询的（投影, 过滤）片段：与 metadata 投影共用 legacy 判定。
    pub(super) fn recovery_scope_sql_parts(&self) -> (&'static str, &'static str) {
        if self.legacy_meta {
            (
                RECOVERY_SCOPE_PROJECTION_LEGACY,
                RECOVERY_SCOPE_FILTER_LEGACY,
            )
        } else {
            (RECOVERY_SCOPE_PROJECTION, RECOVERY_SCOPE_FILTER)
        }
    }

    /// 只读旧库没有工作快照列：依赖 `work_state` 的查询必须跳过而不是报错。
    pub(super) fn has_work_state_columns(&self) -> bool {
        !self.legacy_meta
    }

    /// 只读打开探测到旧形状后切换投影。
    pub(super) fn use_legacy_meta_columns(&mut self) {
        self.meta_columns = THREAD_META_COLUMNS_LEGACY;
        self.legacy_meta = true;
    }
}
