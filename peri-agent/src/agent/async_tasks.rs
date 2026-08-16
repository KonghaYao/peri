//! async tasks manager（Agent 层，per-session 实例化）。
//!
//! 3.0 归位（L1）：`BackgroundTaskRegistry` 定义与 bg shell 实际执行
//! （进程 spawn/进程组/超时/输出收集）自 `peri-middlewares` 迁入本模块。
//! `TaskManager` 是 per-session 聚合：registry + shell 执行 + 事件桥接
//! （`set_event_sender`/`clear_event_sender` 保留为过渡态，供 ACP executor
//! 注入 `BgRegistryEvent` 泵，暂不依赖 M-event-chain）。
//!
//! Middleware 只做任务定义与启动发起（经 `TaskManager` 接口），不持有管理权；
//! 任务生命周期（取消/超时/事件）跟随 session（随 session 创建/销毁）。
//!
//! Task 保持易失投影语义：不持久化，重启不复活。

mod manager;
mod registry;
mod shell;

#[cfg(test)]
use crate::agent::events::BackgroundTaskResult;
#[cfg(test)]
use tokio_util::sync::CancellationToken;

pub use manager::TaskManager;
pub use registry::{
    BackgroundRegistryError, BackgroundTask, BackgroundTaskRegistry, BackgroundTaskStatus,
    BgCancelHandle, BgTaskInfo,
};
pub use shell::{
    bg_shell_task_id, drain_pipe, finalize_bg_shell, kill_process_group,
    kill_process_group_escalating, parse_timeout, persist_truncated_output, shell_command,
    tee_pipe, truncate_bytes,
};

/// 后台任务类别（事实源 peri-acp-types::tasks）
pub use peri_acp_types::tasks::{BgShellHandle, BgTaskKind, BgTaskRegistration};

/// Registry → ACP 层事件桥接
/// 后台任务注册表事件（事实源 peri-acp-types::tasks）
pub use peri_acp_types::tasks::BgRegistryEvent;

#[cfg(test)]
#[path = "async_tasks_test.rs"]
mod tests;
