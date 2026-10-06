//! async tasks manager（Agent 层，per-session 实例化）。
//!
//! Agent owns admission, registration, completion and session shutdown evidence.
//! Shell spawning, process-tree cleanup, tee and durable output belong to the
//! injected execution environment (`ShellExecutor`); no concrete MCP dependency.
//!
//! Task 保持易失投影语义：不持久化，重启不复活。

mod agent_inbox;
pub(crate) mod delivery;
pub use delivery::{build_task_terminal_command, durable_task_terminal_delivery};
pub(crate) mod handoff;
mod manager;
mod registry;
mod scope;
mod shell;
mod shell_executor;

#[cfg(test)]
use crate::agent::events::BackgroundTaskResult;
#[cfg(test)]
use tokio_util::sync::CancellationToken;

pub(crate) use agent_inbox::BackgroundAgentInboxGuard;
pub use agent_inbox::{BackgroundAgentInbox, QueuedSubagentMessage, SubagentMessageError};
pub use manager::TaskManager;
pub use registry::{
    BackgroundRegistryError, BackgroundTask, BackgroundTaskRegistry, BackgroundTaskStatus,
    BgCancelHandle, BgTaskInfo,
};
pub use shell::{
    bg_shell_task_id, finalize_bg_shell, parse_background_timeout, parse_foreground_timeout,
    truncate_bytes, BACKGROUND_MAX_TIMEOUT_MS, FOREGROUND_DEFAULT_TIMEOUT_MS,
    FOREGROUND_MAX_TIMEOUT_MS,
};
pub use shell_executor::ShellExecutor;

/// 后台任务类别（事实源 peri-acp-types::tasks）
pub use peri_acp_types::tasks::{BgShellHandle, BgTaskKind, BgTaskRegistration};

/// Registry → ACP 层事件桥接
/// 后台任务注册表事件（事实源 peri-acp-types::tasks）
pub use peri_acp_types::tasks::BgRegistryEvent;

#[cfg(test)]
#[path = "async_tasks_test.rs"]
mod tests;

#[cfg(test)]
#[path = "async_tasks/registry_wake_test.rs"]
mod registry_wake_tests;

#[cfg(test)]
mod shutdown_test;
