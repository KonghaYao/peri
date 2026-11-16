//! 统一 Langfuse 事件路由层。
//!
//! 定义 [`UnifiedLangfuseEvent`] 枚举（v1 ExecutorEvent + v2 RenderEvent/ObserveEvent
//! 的并集）与 [`LangfuseBridge`] 结构体，提供单一 `process_event` 入口。
//!
//! 所有 Langfuse 追踪事件只需在一处映射到 `LangfuseTracer` 方法，
//! 消除 v1 `forward_langfuse_event` 和 v2 `forward_langfuse_{render,state,observe}`
//! 双轨处理器。

use std::collections::HashMap;
use std::fmt::{self, Debug, Formatter};
use std::sync::Arc;

use parking_lot::Mutex;
use peri_agent::agent::events_v2::{ObserveEvent, RenderEvent};
use tracing;

use crate::langfuse::tracer::stages::{StageHandle, MAIN_AGENT_KEY};
use crate::langfuse::tracer::LangfuseTracer;

mod lifecycle;
mod unified_langfuse_event;
use unified_langfuse_event as unified_event;
mod v1_conversion;
mod v2_conversion;
pub use unified_event::UnifiedLangfuseEvent;

pub use langfuse_bridge::LangfuseBridge;

// ── C4 最小接入测试 ───────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "bridge/lifecycle_test.rs"]
mod tests;

#[cfg(test)]
#[path = "bridge_test.rs"]
mod bridge_test;

#[cfg(test)]
#[path = "bridge/snapshot_test.rs"]
mod snapshot_test;

#[cfg(test)]
#[path = "bridge/ordering_test.rs"]
mod ordering_test;

#[cfg(test)]
#[path = "bridge/registration_test.rs"]
mod registration_test;

#[path = "bridge/langfuse_bridge.rs"]
mod langfuse_bridge;
