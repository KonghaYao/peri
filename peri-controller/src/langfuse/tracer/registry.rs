//! SubAgent 身份注册表(替代无身份 LIFO 栈)。
//!
//! 归属完全按事件侧 agent_id 路由,禁止任何"栈顶/当前活跃"近似:
//!
//! - `by_agent_id`:child_agent_id → [`ActiveSubagent`],stage/generation/tool 内容
//!   事件的 parent 归属(表 1)
//! - `invocations`:(父AgentId, ToolCallId) → [`SubagentInvocation`],Agent 工具调用
//!   与 child 的关联(表 2)
//! - `ObserveEvent::SubagentStart` 缓存开始快照；`SubagentStop` 与父 `ToolEnded`
//!   双信号齐备后导出完整终态，`on_turn_end` 兜底且不重复导出。
//! - 事件乱序经"注册闸门"有界缓存 + parent-first 重放;未知/丢失一律进入
//!   [`SubagentStatus::Incomplete`] 诊断分支,禁止静默挂主 agent

use std::collections::{HashMap, VecDeque};

use crate::langfuse::tracer::tool_batch::ToolBatch;

mod closure;
mod gate;
mod registration;
mod types;

pub(crate) use types::{
    ClosedSubagent, GateEvent, IncompleteReason, Ownership, StartPending, SubagentInvocation,
    SubagentStartOutcome, SubagentStatus, SubagentStopInfo,
};

pub(crate) use subagent_registry::GATE_CACHE_LIMIT;

pub(crate) use subagent_registry::ActiveSubagent;

pub(crate) use subagent_registry::SubagentRegistry;

#[cfg(test)]
#[path = "registry_test.rs"]
mod tests;

#[cfg(test)]
#[path = "registry/lifecycle_tracer_test.rs"]
mod lifecycle_tracer_test;

#[cfg(test)]
#[path = "registry/registration_tracer_test.rs"]
mod registration_tracer_test;

#[cfg(test)]
#[path = "registry/parallel_tracer_test.rs"]
mod parallel_tracer_test;

#[cfg(test)]
#[path = "registry_lifecycle_test.rs"]
mod lifecycle_tests;

#[path = "registry/subagent_registry.rs"]
mod subagent_registry;
