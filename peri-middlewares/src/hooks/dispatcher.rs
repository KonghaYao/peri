//! Hook 分发引擎 + standalone 路径。
//!
//! 把原本分散在 `HookMiddleware::fire_event` 与 `fire_standalone_lifecycle_hooks`
//! 中重复的"hook 查找 / matcher 过滤 / async spawn / 同步执行 / once 预留"
//! 收敛到一个 [`HookDispatcher`]。Standalone 路径通过相同的分发逻辑执行
//! （差异：无 LLM factory，因此 Prompt/Agent hook 被跳过）。

use std::collections::HashMap;
use std::fmt;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

use parking_lot::RwLock;
use peri_acp_types::tasks::TaskManager;
use peri_agent::agent::react::ReactLLM;

use crate::hooks::executor;
use crate::hooks::input_builder;
use crate::hooks::matcher;
use crate::hooks::once_tracker::OnceTracker;
use crate::hooks::types::HookAction;
use crate::hooks::types::HookEvent;
use crate::hooks::types::HookInput;
use crate::hooks::types::HookType;
use crate::hooks::types::PermissionDecision;
use crate::hooks::types::RegisteredHook;

#[cfg(test)]
#[path = "hook_action_merge_test.rs"]
mod hook_action_merge_tests;

#[cfg(test)]
#[path = "model_call_context_test.rs"]
mod model_call_context_tests;

#[path = "dispatcher/hook_dispatcher.rs"]
mod hook_dispatcher;
#[cfg(test)]
pub(crate) use hook_dispatcher::async_hook_output_summary;
pub use hook_dispatcher::fire_standalone_lifecycle_hooks;
pub use hook_dispatcher::fire_standalone_lifecycle_hooks_owned;
#[cfg(test)]
use hook_dispatcher::merge_hook_actions;
#[cfg(test)]
pub(crate) use hook_dispatcher::AsyncHookOutputSummary;
pub use hook_dispatcher::HookDispatcher;
