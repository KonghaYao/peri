use std::process::Stdio;
use std::sync::Arc;

use super::shell;
use super::shell::ShellExecutionGuard;
use super::shell_output::ShellOutputCapture;
use futures::FutureExt;
use peri_acp_types::tasks::{BgShellHandle, BgTaskKind, ExternalExecutionGuard, OnBgCompleteFn};
use peri_agent::agent::async_tasks;
use peri_agent::agent::async_tasks::BackgroundTask;
use peri_agent::agent::async_tasks::BackgroundTaskRegistry;
use peri_agent::agent::async_tasks::BackgroundTaskStatus;
use peri_agent::agent::async_tasks::BgCancelHandle;
use peri_agent::agent::async_tasks::ShellExecutor;

#[path = "shell_executor/local_shell_executor.rs"]
mod local_shell_executor;
pub use local_shell_executor::LocalShellExecutor;
