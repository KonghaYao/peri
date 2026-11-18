//! 全局 Atom 定义——替代部分 Effect 变体。
//!
//! 使用 ratatui-kit 0.7 AtomStatic<T> + AtomState<T> 作为全局状态容器。
//! 组件通过 use_atom(&ATOM) 订阅。写入自动唤醒订阅组件。
//!
//! 类型别名：pub type Handle<T> = AtomState<T>（供其他文件引用）。
//!
//! Channel 约定：
//! - SUBMIT_TX: event_handlers 按键 → submit_consumer 消费
//! - CANCEL_TX: event_handlers Ctrl+C → cancel_consumer 消费
//! - REWIND_ACTION_TX: rewind popup → rewind_consumer
//! - THREAD_LOAD_TX: thread browser → thread_load_consumer

mod mcp;

mod confirm;
pub use confirm::{CONFIRM_PAYLOAD, ConfirmAction, ConfirmPayload};

mod state;
pub use state::*;

pub use crate::kit::acp_types::BgTaskEntry;
pub use mcp::{
    McpControlSummary, McpResourceSummary, McpServerSummary, McpSubscriptionsSummary,
    McpToolSummary,
};

mod input_snapshot;
pub use input_snapshot::{INPUT_SNAPSHOT, InputSnapshot};
