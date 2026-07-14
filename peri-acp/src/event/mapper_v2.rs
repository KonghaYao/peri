//! v2 事件 → v1 ExecutorEvent 桥接（re-export 自 peri-agent）
//!
//! 实际实现已迁移到 `peri_agent::agent::events_v2_mapper`，因为该 mapper 是
//! 纯函数操作 peri-agent 类型（ExecutorEvent / RenderEvent / StateEvent / ObserveEvent），
//! 多个 crate（peri-acp 主 executor / peri-middlewares SubAgent 转发器）都需要复用。
//! peri-acp 在此 re-export 保持向后兼容，避免下游引用断裂。

pub use peri_agent::agent::events_v2_mapper::{
    observe_event_to_executor, render_event_to_executor, state_event_to_executor, V2Event,
};
