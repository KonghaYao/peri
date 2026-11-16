//! Event sink abstraction for ACP session event routing.
//!
//! Different frontends (TUI via MpscTransport, IDE via stdio SDK) route agent
//! execution events differently. [`EventSink`] abstracts this so the core
//! prompt execution logic can live in `peri-acp`.
//!
//! L5：trait 定义已契约化至 `peri-acp-types::event::EventSink`（命令执行体 /
//! 事件发射辅助经契约端口调用），本模块保留 ACP 协议面实现
//! （TransportEventSink / StdioEventSink 等）。

mod legacy;
mod stdio_event_sink;
use stdio_event_sink as stdio;

pub use stdio::StdioEventSink;

// Re-export SDK types used by StdioEventSink.
pub use agent_client_protocol::schema::v1::SessionId as SdkSessionId;
pub use agent_client_protocol::schema::v1::SessionNotification;
pub use agent_client_protocol::schema::v1::SessionUpdate;
pub use agent_client_protocol::Client;
pub use agent_client_protocol::ConnectionTo;
use async_trait::async_trait;
use dashmap::DashMap;
use peri_acp_types::event::{CompactStrategy, ExecutorEvent};
use peri_acp_types::PeriCaps;
use serde_json::json;
use std::sync::Arc;
use tracing::{debug, error};

use crate::event;
use crate::event::activity;
use crate::transport::AcpTransport;

/// EventSink 契约（L5：事实源 peri-acp-types::event）。
pub use peri_acp_types::event::EventSink;
use peri_acp_types::event::EventSinkError;

pub(super) use transport_event_sink::attach_child_origin;

pub use transport_event_sink::CompactCommitObserver;

pub use transport_event_sink::TransportEventSink;

#[cfg(test)]
#[path = "event_sink_test.rs"]
mod tests;

#[cfg(test)]
#[path = "event_sink/compact_observer_test.rs"]
mod compact_observer_tests;

#[path = "event_sink/transport_event_sink.rs"]
mod transport_event_sink;
