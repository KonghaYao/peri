//! Event mapping from ExecutorEvent to ACP SessionUpdate and peri/agent_event routing.
//!
//! [`AcpEvent`] is the DTO that replaces raw `ExecutorEvent` serialization on the
//! `peri/agent_event` channel. It contains only the fields that TUI consumers need,
//! avoiding a direct `ExecutorEvent` dependency in the TUI.

pub mod activity;
mod forwarder;
#[cfg(test)]
mod forwarder_test;
pub mod mapper;
pub mod oauth;
pub(crate) mod tool_projection;

pub(crate) use self::forwarder::{forward_eventbus, spawn_eventbus_forwarder};
pub use mapper::{map_event, MappedEvent};
pub use peri_acp_types::summary::{
    CompactFileInfoDto, StopReasonDto, TodoItemDto, TodoStatusDto, TokenUsageDto,
    WorkflowProgressDto,
};
// v1 兼容映射（v2 → ExecutorEvent）保留在 ACP 协议面
// `peri_acp_types::event_v2`（`2026-07-18-events-v2-mapper-removal.md`：
// events_v2_mapper 模块已退役；3.0 M-event-chain + 批 2「v1-retire」：
// Agent 层发射统一 v2（EventBus），v1 `ExecutorEvent` 中间态退役、仅保留为
// 协议序列化面载体——发射点经 `Controller::publish_event`
// （Controller → Runtime 补打身份 → 订阅广播），
// 协议化消费经 `Controller::subscribe` 订阅——事件
// 三层化的统一出口在 Controller，见 `event/forwarder.rs` 与
// 事件泵（`peri-agent::session::exec::executor_helpers::spawn_event_pump`，
// 经 `peri_acp_types::event::{EventPublisher, EventSubscriber}` 端口接入））。
pub use peri_acp_types::event_v2::{
    observe_event_to_executor, render_event_to_executor, state_event_to_executor,
};

use serde::{Deserialize, Serialize};

pub use event_mapping::AcpEvent;

#[cfg(test)]
mod variant_coverage_test;

#[path = "acp_event.rs"]
mod acp_event;
use acp_event as event_mapping;
