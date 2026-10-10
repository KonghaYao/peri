//! 远端会话 schema：直接说 canonical 形状（`sessions::canonical` 是唯一来源）。
//!
//! 远端不再有自己的会话表形状：`peri_sessions` / `peri_session_messages` 以及「meta 列 +
//! 扁平绑定列」的混合形状随统一删除。远端下发的是与**本机 SQLite 逐字同一份** DDL
//! （[`CREATE_TABLES_SQL`] / [`CREATE_INDEXES_SQL`]），因此两个 adapter 之间不再有表名或
//! 列名映射，canonical 历史顺序也与本机一致地由 `messages.rowid` 承载（原先的 `ordinal`
//! 列与它的索引一并删除）。
//!
//! 远端仍独有的是**执行器机制表**（不属于 canonical 会话 schema，本机不建）：
//!
//! | 表 | 承载 |
//! | --- | --- |
//! | `peri_store_meta` | 远端版本标记（服务端拒绝写 `PRAGMA user_version`，见 [`super::schema`]） |
//! | `peri_op_ledger` | 幂等资格账本（本机用本地事务表达同一件事） |
//!
//! 初始化语句全部 `IF NOT EXISTS`，可重复执行；调用方只在只读身份读取判定「未初始化或
//! 形状已知」时执行，已初始化时读回而不改写任何行。

use super::sql::StatementSpec;
use crate::sessions::canonical::{CREATE_V2_INDEXES, CREATE_V2_TABLES};

/// 初始化本任务 schema 的语句集：建表段 + 索引段，**一条语句一个 spec**。
///
/// 远端执行器的语句单元就是一条语句，因此清单直接逐条展开，不把多句拼进一个请求。
/// 两段的顺序有意义：`idx_threads_updated` 引用 `threads` 的列，索引必须晚于建表；
/// 与本机 `sqlite_store::schema` 的「建表 → 补列 → 建索引」顺序同源。
pub(super) fn initialization_plan() -> Vec<StatementSpec> {
    CREATE_V2_TABLES
        .iter()
        .chain(CREATE_V2_INDEXES)
        .map(|sql| StatementSpec::bare(sql))
        .collect()
}
