//! Session v2 — 会话统一入口
//!
//! Session 是 peri-agent 的顶层门面，聚合五个核心实体：
//! - [`SessionStore`]：会话生命周期数据（不可变），含 FrozenContext
//! - [`MessageQueue`]：收件箱，异步消息注入
//! - [`SessionConfig`]：可变配置（权限模式、Cancel Token、超时）
//! - [`MessageTranscript`]：对话笔录，只追加不篡改
//! - [`TurnContext`]：单次 turn 上下文，turn 结束即销毁
//!
//! 外部通过 `Session::new()` 创建，按需访问五个实体，通过 `start_turn()` 启动新 turn。
//!
//! ## 异步 Owner（v2 新增）
//!
//! Session 可选地持有两个异步 owner：
//! - [`SessionInbox`](crate::agent::session::SessionInbox)：await-wake 包装器，用于 idle 期间阻塞唤醒
//! - [`CronOwner`](crate::agent::session::CronOwner)：cron trigger → inbox 桥接
//!
//! 这些 owner 在 `peri-acp` 层通过 `set_async_owners` 注入。持有 owner 后，
//! cron 事件直接通过 inbox 唤醒 executor，无需 TUI 轮询。
//! 不设置 owner 时，TUI 轮询路径仍然有效（向后兼容）。
//!
//! cron 主路径由 `AcpSession.cron_bridge`（session 级）持有，跨 turn 存活；
//! `set_async_owners` 仅 print fallback 使用。

pub mod async_result_mailbox;
pub mod async_router;
pub mod bg_complete;
pub mod config;
pub mod cron_events;
pub mod exec;
pub mod factory;
/// 对象级内存计量与诊断采样（S1）；只读诊断，不参与业务语义。
pub mod memory_probe;
pub mod producer_reminders;
pub mod queue;
pub mod retry_events;
pub mod runtime;
pub mod store;
pub mod subagent;
pub mod tool_catalog;
pub mod transcript;

#[cfg(test)]
#[path = "test_resources.rs"]
pub(crate) mod test_resources;
pub mod turn_context;
pub use turn_context as turn;
pub mod user_input_mailbox;

pub use config::{PermissionMode, SessionConfig, ThinkingConfig};
/// MessageFlags 已下沉 peri-acp-types（store 契约），此处 re-export 保持兼容。
pub use peri_acp_types::store::MessageFlags;
pub use queue::{
    ExecutionBinding, MessageActivation, MessageDisposition, MessageKind, MessagePolicy,
    MessageQueue, MessageRequirement, MessageSource, QueueClosed, QueuedMessage, QueuedPayload,
};
pub use store::{FrozenContext, FrozenContextBuilder, SessionId, SessionStore};
pub use transcript::{MessageTranscript, StagedData, TranscriptEntry};
pub use turn::{TurnContext, TurnId};

use std::sync::Arc;

use parking_lot::RwLock;

use crate::agent::session::cron_owner::CronOwner;
use crate::agent::session::inbox::SessionInbox;
use crate::thread::ThreadId;

pub use session::AsyncOwners;

pub use session::Session;

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

#[path = "session.rs"]
#[expect(
    clippy::module_inception,
    reason = "The style guide keeps the facade declarative and names the implementation leaf after its primary type."
)]
mod session;
