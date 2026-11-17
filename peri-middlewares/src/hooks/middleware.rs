//! Plugin hook middleware — fires registered hooks at lifecycle events.
//!
//! 本文件是 Facade：保留 [`HookMiddleware`] struct + `Middleware` trait 实现
//! 作为唯一 public 类型（API 兼容），实际职责委托给 `hooks/` 目录下同级子模块：
//!
//! - `dispatcher`：核心 hook 分发引擎（fire_event 内部循环 + standalone 路径统一）
//! - `input_builder`：`HookInput` 字面量构造集中收口
//! - `action_resolver`：`HookAction` → `AgentResult` / `ToolCall` 归约（消除 5 处重复 match）
//! - `permission_gate`：PermissionRequest 双条件门控
//! - `hook_output_delivery`：M10 hook 输出字段 → 受众投递（`additionalContext`
//!   / `systemMessage` 的 reminder 构造、承载预算与未接线降级诊断）
//! - `stop_block_guard`：Stop Block 连续次数状态机（上限 8）
//! - `once_tracker`：一次性 hook 状态跟踪
//!
//! `[TRAP]` collect_tool_results 延迟写入不变量：dispatcher 和 action_resolver
//! 不写 state（仅 `after_agent` 的 stop_block 写 state）。

// 兼容旧调用点（`crate::hooks::middleware::fire_standalone_lifecycle_hooks`）：
use peri_agent::middleware::capabilities as hook_state;
use std::fmt;
use std::fmt::{Debug, Formatter};
// 函数已迁移到 `dispatcher.rs`，此处保留 pub use 以维持 ABI。
pub use crate::hooks::dispatcher::fire_standalone_lifecycle_hooks;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_acp_types::error::ToolRejectionCause;
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};
use peri_agent::agent::react::AgentOutput;
use peri_agent::agent::react::ReactLLM;
use peri_agent::agent::react::ToolCall;
use peri_agent::agent::react::ToolResult;
use peri_agent::error::AgentError;
use peri_agent::error::AgentResult;
use peri_agent::messages::BaseMessage;
use peri_agent::middleware::r#trait::Middleware;
use peri_agent::session::MessageKind;
use peri_agent::session::MessageSource;
use peri_agent::session::QueuedMessage;
use serde_json::json;

use crate::hooks::hook_output_delivery;

use crate::permission::SharedPermissionMode;
use peri_agent::interaction::{
    ApprovalDecision, ApprovalItem, InteractionContext, InteractionResponse, UserInteractionBroker,
};
// HookType 仅 `middleware_test.rs` 通过 `use super::*` 使用。保留以维持测试不变。
#[allow(unused_imports)]
use crate::hooks::action_resolver;
use crate::hooks::dispatcher::HookDispatcher;
use crate::hooks::input_builder;
use crate::hooks::once_tracker::OnceTracker;
use crate::hooks::permission_gate;
use crate::hooks::stop_block_guard;
use crate::hooks::stop_block_guard::GuardDecision;
use crate::hooks::stop_block_guard::StopBlockGuard;
use crate::hooks::types::HookAction;
use crate::hooks::types::HookEvent;
use crate::hooks::types::HookInput;
#[cfg(test)]
use crate::hooks::types::HookType;
use crate::hooks::types::PermissionDecision;
use crate::hooks::types::RegisteredHook;

#[cfg(test)]
#[path = "middleware_test.rs"]
mod tests;

#[cfg(test)]
#[path = "post_tool_batch_test.rs"]
mod post_tool_batch_tests;

#[cfg(test)]
#[path = "async_diagnostic_test.rs"]
mod async_diagnostic_tests;

#[cfg(test)]
#[path = "ask_host_path_test.rs"]
mod ask_host_path_tests;

#[cfg(test)]
#[path = "undelivered_output_test.rs"]
mod undelivered_output_tests;

#[cfg(test)]
#[path = "hook_output_delivery_test.rs"]
mod hook_output_delivery_tests;

#[cfg(test)]
#[path = "permission_request_deny_test.rs"]
mod permission_request_deny_tests;

#[path = "middleware/hook_middleware.rs"]
mod hook_middleware;
pub use hook_middleware::HookMiddleware;
