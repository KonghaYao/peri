//! v2 事件流契约 — 三层分级事件总线（事实源 `peri-acp-types::event_v2`，
//! 本模块 re-export 保兼容）。
//!
//! 所有事件强制携带 `turn_id` 与 `agent_id`；三层分法（渲染/状态/观测）、
//! v1 兼容映射（`*_event_to_executor`）与 EventBus 定义均随契约层迁移。

pub use peri_acp_types::event_v2::{
    observe_event_to_executor, render_event_to_executor, state_event_to_executor, Event, EventBus,
    EventBusConfig, EventHandles, ObserveEvent, RenderEvent, StateEvent, TurnErrorReason,
};

#[cfg(test)]
#[path = "events_v2_test.rs"]
mod tests;

#[cfg(test)]
#[path = "events_v2_mapper_test.rs"]
mod v1_compat_tests;
