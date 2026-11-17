//! `/rewind` 命令 — 回滚对话到指定消息。
//!
//! 参数形态（Phase 5 Step 5 由 serde_json 迁入 ArgsSchema）：
//! `/rewind <target_message_id> [--no-revert-files]`；`--no-revert-files`
//! 缺省时 revert_files=true（现状 serde default_true 语义保持）。
//! 定位目标消息后：
//! 1. 截断 history 到目标消息之前
//! 2. 从被移除的消息中提取 Write/Edit 工具调用（含 builtin `workspace` 实例的
//!    effective name，如 `mcp__workspace__Write`），带 tool-call id 交给 Workspace 逆向恢复；
//!    Write 只能恢复调用时记录的 pre-image，缺失即拒绝；Edit 可用调用自带的 old/new 反转
//! 3. 验证保留消息的 ToolUse/ToolResult 配对完整性
//! 4. 从 SQLite 持久化中删除被移除的消息
//! 5. 提交成功后把 `threads.context_usage_tokens` 写回 NULL（旧快照随截断失效，§2.2）
//! 6. 发送 RewindCompleted 事件通知 TUI 刷新（重建信号，保留原样）
//!
//! 事件载荷是「提交成功后**有效**的保留前缀」：调用方按边界计划算出（见
//! [`DurableRewindCommit::Boundary`] 的 `effective_history`），与发布到热态、以及 RPC 响应里的
//! wire `history` 同源；canonical 前缀（含仍被存活摘要覆盖的正文）不得进事件，
//! 否则 TUI 以它重建出的 transcript 与模型、存储都不一致。
//!
//! **无持久化后端的上下文**（[`CommandContext::session_resources`] 为 `None` 且
//! `thread_id` 也为 `None`，即该结构文档声明的打印/测试路径）只截断内存历史：这种上下文
//! 没有可与之分叉的存储，durable 提交按「无事可做」处理并**显式记一条日志**；只有一半句柄
//! 缺失（有门面无 thread id，或反之）是装配错误，按硬失败处理、不发事件。
//!
//! RPC 路径（`dispatch/rewind.rs`）复用本模块的 `execute_rewind` 共享执行体，
//! RPC wire（serde RewindArgs + preview_fingerprint）保持不变。

mod events;

use peri_acp_types::builtin_mcp;
use peri_acp_types::command::{ArgKind, ArgSpec, ArgsSchema, FlagSpec};
use peri_acp_types::messages::{BaseMessage, ContentBlock, MessageId};
use peri_acp_types::session_resources::RewindOutcome;
use tracing::{debug, error, warn};

use super::{CommandContext, CommandFeedback, CommandResult, FeedbackChannel, FeedbackLevel};
use crate::session::executor::PromptStopReason;

pub use rewind_command::RewindCommand;

pub(crate) use rewind_command::FileChange;

pub use rewind_command::DurableRewindCommit;

pub(crate) use rewind_command::RewindDurability;

pub(crate) use rewind_command::execute_rewind;

pub(crate) use rewind_command::extract_file_changes;

#[cfg(test)]
#[path = "rewind_test.rs"]
mod tests;

#[cfg(test)]
#[path = "rewind_context_usage_test.rs"]
mod context_usage_tests;

#[path = "rewind/rewind_command.rs"]
mod rewind_command;
