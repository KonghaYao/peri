//! 远程会话数据 adapter：把 [`SessionDataPort`] 的会话行为落到远端 schema 上。
//!
//! ## 本阶段落地范围（C-03 第一、二批）
//!
//! | 行为 | 状态 |
//! | --- | --- |
//! | `load_snapshot` / `load_meta` / `load_binding` / `load_session_history` | 已实现（一致读取；flags 由一致快照一并返回） |
//! | `list_sessions`（scoped 分页）/ `list_children` / `list_session_tree` | 已实现 |
//! | `save_new_session` | 已实现（meta + 绑定 + frozen 一次落库） |
//! | `save_fork` | 已实现（映射后的 payload + flags，source 不变） |
//! | `save_child` | 已实现（父子/根归属 + 继承区 + root frozen 原文） |
//! | `update_meta`（title/status/cancel_policy/config 定向更新） | 已实现 |
//! | `append_history` / `apply_message_projections` / `apply_compaction` | 已实现（[`super::session_history`]：批内守卫，整批生效或整批不生效） |
//! | `rewind_history`（显式两边界）/ `remove_history_entries` | 已实现（同上；未知截止点保持无变更语义） |
//! | `delete_tree` / `revoke_unpublished_session` | 已实现（[`super::session_lifecycle`]；远端无墓碑/执行行，删除是刻意删除数据事实） |
//! | `load_child_resume_record` / `store_child_resume_record` | 已实现（`agent_status` + 由状态派生的认领标记） |
//! | `drain` | 已实现为「无队列可排空，但未结清不算已排空」（见方法文档） |
//! | `close` | 已实现（真正关闭连接，之后写入明确失败） |
//! | `recover_persistence` | 已实现：本机已无可求证的 durable 记录，会话数据可读即 `Recovered`（跨进程未决判定随 v10 撤销） |
//!
//! 写入路径：**每次调用唯一操作 id + 资格先于效果**（远端账本 `peri_op_ledger` 的资格写入
//! 与业务效果在同一个托管批里同生共死）。操作 id 不由内容派生，因此同内容的第二次、第三次
//! 领域调用都是新操作（状态 A→B→A、标题 x→y→x 不再被当成重放丢弃）；输入摘要只用于一致性
//! 校验。v10 撤销本机操作日志后，不再有「发送前本机落盘、确定终态才结清」这一步，未结清只
//! 在当前实例的 persistence gate 上表达（见 `recover_persistence` 的方法文档）。等价的公开行为仍只有门面暴露的
//! 那 33 条——adapter 不另立一套平行行为。
//!
//! Workspace 归属与执行快照保存在远端；文件系统发现和进程内运行句柄由
//! `RemoteExecution` 持有（见 `sessions::local_port`）。
//!
//! 打开的两种访问模式：
//!
//! - 可写打开：读回身份（未初始化则先建身份），只在缺表或缺必要索引时补齐 schema；
//! - 只读打开：只读回身份，不写任何东西；若 store 尚无本任务 schema（身份或会话表缺失），
//!   直接 `Unsupported`——没有 schema 就没有会话事实可读，也不越权建表。
//!
//! ## 边界（远程不做本机的事）
//!
//! - 不持有本机执行事实：运行句柄及未结清标记只在当前实例内，远端不持久化这些事实；
//! - 不解析本机目录、不发执行资格、不判定 legacy：`LegacyConfirmed` 与执行准入由门面与
//!   执行面按本机证据判定（这也是 `load_binding` 只回答绑定事实的原因）；

mod remote_session_data;

#[cfg(test)]
use remote_session_data::{ConnectionFactory, ConnectionGate};

#[cfg(test)]
use remote_session_data::StoreId;

#[cfg(test)]
use remote_session_data::HashMap;

#[cfg(test)]
use remote_session_data::Arc;

#[cfg(test)]
use remote_session_data::RwLock;

#[cfg(test)]
#[path = "session_data_fixture.rs"]
mod fixtures;
#[path = "session_close.rs"]
mod session_close;

mod connection_lifecycle;
mod construction;
mod ownership;
mod recovery;

pub(in crate::sessions::remote) use remote_session_data::StoreInitialization;

#[path = "session_open.rs"]
mod session_open;
pub(in crate::sessions::remote) use remote_session_data::RemoteSessionData;
pub(super) use session_open::{
    legacy_read_projection, open_step, open_verdict, probe_shape, verify_legacy_open_identity,
    OpenStep,
};

pub(in crate::sessions::remote) use remote_session_data::not_found;

pub(in crate::sessions::remote) use remote_session_data::invalid_input;

pub(in crate::sessions::remote) use remote_session_data::unsupported_behavior;
