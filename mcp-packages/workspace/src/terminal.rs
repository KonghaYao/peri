use std::fmt;
use std::fmt::{Debug, Formatter};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use peri_acp_types::tasks::{OnBgCompleteFn, TaskManager};
use peri_agent::agent::async_tasks;
use peri_agent::agent::async_tasks::BgTaskKind;
use peri_agent::agent::async_tasks::FOREGROUND_MAX_TIMEOUT_MS;
use peri_agent::tools::{BaseTool, ToolExecutionEvidence, ToolExecutionStatus, ToolOutput};
use peri_mcp_common::shell;
use peri_mcp_common::shell::ShellExecutionGuard;
use peri_mcp_common::shell_output::ShellOutputCapture;
use peri_time;
use serde_json::Value;
use std::time::Duration;
use tracing::warn;

// `TerminalMiddleware` 已随 v4-part-4 W3-C1 删除：`Bash` 的唯一提供面是 builtin
// `workspace` 实例的 bridge（模型面名字 `Bash`），本文件只保留
// `BashTool` 实现（由 `mcp::builtin::workspace` 的 handler 复用；AW3-02）。

#[cfg(test)]
#[path = "terminal_test.rs"]
mod tests;

#[path = "terminal/bash_tool.rs"]
mod bash_tool;
#[cfg(test)]
use bash_tool::background_cleanup_hint;
#[cfg(test)]
use bash_tool::foreground_timeout_note;
#[cfg(test)]
use bash_tool::truncate_output;
pub use bash_tool::BashTool;
pub use bash_tool::OnBgStartedFn;
