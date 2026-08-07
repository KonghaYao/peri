//! v2 事件流契约 — 三层分级事件总线（事实源 `peri-acp-types::event_v2`，
//! 本模块 re-export 保兼容）。
//!
//! 所有事件强制携带 `turn_id` 与 `agent_id`；三层分法（渲染/状态/观测）。
//! v1 `ExecutorEvent` 中间态已退役（批 2「v1-retire」）：发射统一 v2 形态，
//! `*_event_to_executor` 仅保留在 ACP 协议序列化面（peri-acp-types）。

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
