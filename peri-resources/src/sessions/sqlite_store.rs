//! SQLite 会话库：数据面与执行/登记面共用同一 pool 与同一库。
//!
//! [`SqliteThreadStore`] 是消费侧迁移期间的**桥**：它把旧 `ThreadStore` 的调用转发
//! 到共享库句柄 [`database::SqliteSessionDatabase`]；新行为一律加到
//! [`data::SessionDataPort`](super::data::SessionDataPort) 的 SQLite 实现
//! （`session_data`），不在本文件扩展。E 阶段 `ThreadStore` 退出时本类型一并删除。
//! 连接、行映射、上下文、compaction 与 schema 事务由私有模块负责。

mod compaction;
mod connection;
mod context;
mod sqlite_session_database;
mod sqlite_thread_store;
pub(in crate::sessions) use super::discovery;
use sqlite_session_database as database;
mod failure;
mod local_execution;
use local_execution as local;
mod sqlite_oauth_credential_store;
use sqlite_oauth_credential_store as oauth_credentials;
pub(crate) mod row_mapping;
mod schema;
mod schema_cleanup;
mod session_data;
mod session_rows;
#[path = "storage_v11_migration.rs"]
mod storage_v11_migration;
mod workspace;
mod workspace_identity;

pub use connection::{ReadOnlyStoreErrorKind, ReadOnlyThreadStoreError};
#[cfg(test)]
use sqlite_thread_store::{
    serialize_persisted_payload, AgentStatus, BaseMessage, CompactionChange, InheritedContext,
    MessageFlags, PersistedPayload, ThreadMeta, ThreadStore,
};

#[cfg(test)]
use sqlite_thread_store::meta_from_row;

#[cfg(test)]
use sqlite_thread_store::AssertSqlSafe;

#[cfg(test)]
use sqlite_thread_store::{Arc, HashMap};

#[cfg(test)]
use sqlite_thread_store::SessionResourcesImpl;

#[cfg(test)]
use sqlite_thread_store::SqliteSessionDatabase;

pub(in crate::sessions) use failure::execution_failure;
/// 提交阶段/领域失败映射：由门面测试驱动真实写入准入，生产路径在 `session_data`
/// 与 `compaction` 内部直接引用。
#[cfg(test)]
pub(in crate::sessions) use failure::{commit_failure, write_failure};
pub(in crate::sessions) use local::LocalExecution;
#[cfg(test)]
use sqlite_thread_store::CURRENT_SCHEMA_VERSION;

/// 多行 INSERT 的分块行数；测试用它构造跨块批次，与生产共用同一个上限。
#[cfg(test)]
pub(in crate::sessions::sqlite_store) use session_data::message_insert_chunk_rows;
/// 数据面实现：生产组合从 [`LocalExecution::data_port`] 取得它，本重导出供测试夹具直接命名。
#[cfg(test)]
pub(crate) use session_data::SqliteSessionData;
#[cfg(test)]
use sqlite_thread_store::{
    classify_shape_probe_failure, REQUIRED_MESSAGE_COLUMNS, REQUIRED_THREAD_COLUMNS,
};

#[cfg(test)]
use sqlite_thread_store::SqliteConnectOptions;

pub use sqlite_thread_store::SqliteThreadStore;

#[cfg(test)]
use sqlite_thread_store::absolute_test_path;

#[cfg(test)]
#[path = "sqlite_store_test.rs"]
mod tests;

#[cfg(test)]
#[path = "sqlite_store/read_only_test.rs"]
mod read_only_tests;

#[cfg(test)]
#[path = "sqlite_inherited_context_test.rs"]
mod inherited_context_tests;

#[cfg(test)]
#[path = "sqlite_store/legacy_test.rs"]
mod legacy_tests;

#[cfg(test)]
#[path = "sqlite_store/session_data_test.rs"]
mod session_data_tests;

#[cfg(test)]
#[path = "sqlite_store/thread_child_delete_test.rs"]
mod thread_child_delete_tests;

#[cfg(test)]
#[path = "sqlite_store/session_mq_test.rs"]
mod session_mq_tests;

#[cfg(test)]
#[path = "sqlite_store/schema_v7_test.rs"]
mod schema_v7_tests;

#[cfg(test)]
#[path = "sqlite_store/schema_v10_test.rs"]
mod schema_v10_tests;

#[cfg(test)]
#[path = "sqlite_store/schema_v11_test.rs"]
mod schema_v11_tests;

#[cfg(test)]
#[path = "sqlite_store/schema_v14_test.rs"]
mod schema_v14_tests;

#[cfg(test)]
#[path = "sqlite_store/schema_v15_test.rs"]
mod schema_v15_tests;

#[cfg(test)]
#[path = "sqlite_store/schema_v16_test.rs"]
mod schema_v16_tests;

#[cfg(test)]
#[path = "sqlite_store/session_id_environment_test.rs"]
mod session_id_environment_tests;
