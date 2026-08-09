//! 状态层（Feature F4）：Y.Doc 聚合面。
//!
//! 职责（`docs/plans/f4-state.md`，权威 `docs/architecture.md` §5–§8.5/§17.2）：
//! 把 ACPChannel（F5）产出的 [`NormalizedEvent`] 经幂等聚合投影到每 chat
//! 双 Doc（Chat/Control），并承担 Registry Doc 的 server 状态源单写、权限 CAS、
//! ACP 会话历史列表投影与全局 Degraded 判定（§17.2）。所有 Y.Doc 写入必须经
//! [`DocManager`] 唯一提交边界（§5.6），yrs `transact_mut()` 并发 panic 由每
//! chat 单写者排除（§7.4）。
//!
//! 模块内依赖方向（单向）：`normalized ← aggregator ← doc_manager`；
//! `factory/view_store/chat_writer` 是 doc_manager 的实现细节；`permission/
//! session_list` 被 aggregator 与 doc_manager 复用；`registry` 独立于 per-chat
//! 链路，仅经 DocManager 提交（Registry Doc 也是 Doc，受唯一提交边界约束）。
//!
//! 脱敏纪律（§9.3）：本模块日志只记录 `chat_id/seq/epoch/kind/applied/
//! reason/bytes` 等元数据，**不记录消息正文、工具参数、token、密钥**；
//! [`Aggregator::apply`] 是纯函数（无 I/O、无日志副作用），脱敏日志由
//! DocManager writer 在返回后统一打。
//!
//! 边界声明（不属本 feature）：ACPChannel（F5）产出 [`NormalizedEvent`]；
//! persist（F6）实现 [`UpdateSink`]；broadcaster（F7）订阅
//! [`DocManager::subscribe_updates`]；command-coordinator（F7）经
//! [`DocManager::submit_command`] 提交写入命令。

pub mod aggregator;
pub mod chat_writer;
pub mod doc_manager;
pub mod doc_pair;
pub mod factory;
pub mod normalized;
pub mod permission;
pub mod registry;
pub mod session_list;
pub mod view_store;

#[cfg(test)]
mod aggregator_test;
#[cfg(test)]
mod doc_manager_test;
#[cfg(test)]
mod factory_test;
#[cfg(test)]
mod permission_test;
#[cfg(test)]
mod registry_test;
#[cfg(test)]
mod session_list_test;

pub use aggregator::{Aggregator, ApplyReason, ApplyResult};
pub use chat_writer::ContentKind;
pub use doc_manager::{
    BatchConfig, DocCommand, DocManager, DocManagerError, DocUpdate, PersistError, SubmitError,
    SubmitResult, UpdateSink,
};
pub use doc_pair::{DocPair, StreamState};
pub use factory::{DocKind, Factory, FactoryError};
pub use normalized::{EventBody, NormalizedEvent};
pub use permission::{resolve, expire, CasOutcome};
pub use registry::{DegradeCause, RegistryError, RegistryState};
pub use session_list::{apply_diff, diff, SessionListDiff};
pub use view_store::{
    encode_state_as_update, merge_updates_v1, TransactionCtx, ViewStore, ViewStoreError,
    ViewStoreSubscription,
};
