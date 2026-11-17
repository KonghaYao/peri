//! 工具分发（v2）— before_tools_batch → 并发执行 → after_tool → 统一写入
//!
//! 关键设计：
//! - **state 来源**：v2 用 `StageContext.session.transcript`（通过 middleware_runner 桥接
//!   StageContext 调用 middleware chain）
//! - **事件总线**：v2 用 `ctx.runtime.event_bus.emit_render(RenderEvent::*)`
//! - **写入语义**：完整交换经唯一 writer 原子持久化，确认后发布 transcript
//!
//! 不变量（与 v1 一致）：
//! - **延迟写入**：before_tool / after_tool 期间 transcript 不含本轮 AI 消息
//! - **deferred_error**：多工具并发循环不在中途返回，先收集所有错误
//! - **ToolEnd emit 时机**：工具完成即刻 emit，早于 after_tool 后处理

use std::collections::HashMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

mod stage_effective_tool_dispatcher;
use stage_effective_tool_dispatcher as effective_dispatcher;
mod execution;

use crate::agent::events_v2::RenderEvent;
use crate::agent::react::{Reasoning, ToolCall, ToolResult};
use crate::agent::stages::context::StageContext;
use crate::agent::stages::invocation_context::ExecutionEntryOrigin;
use crate::agent::stages::invocation_context::InvocationContext;

use crate::error::{AgentError, AgentResult};
use crate::messages::{BaseMessage, ToolCallRequest};
use crate::session::tool_catalog::SessionToolCatalogSnapshot;
use crate::tools::{BaseTool, CanonicalToolInvocation};
use execution::SettledInvocation;

pub use tool_dispatcher::invocation_dispatcher;

pub use tool_dispatcher::DispatchOutcome;

pub use tool_dispatcher::dispatch_tools;

#[cfg(test)]
use tool_dispatcher::handle_consecutive_failures;

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "tool_dispatch_test.rs"]
mod tests;

#[cfg(test)]
#[path = "tool_image_dispatch_test.rs"]
mod image_tests;

#[path = "tool_dispatch/tool_dispatcher.rs"]
mod tool_dispatcher;
