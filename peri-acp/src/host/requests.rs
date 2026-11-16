//! ACP Request dispatch — handles all ACP protocol request methods.
//! Extracted from original acp_server.rs (2026-05-20 split).

use std::collections::HashMap;
use std::sync::Arc;

use crate::session::event_sink::TransportEventSink;
use peri_acp_types::tasks::BgRegistryEvent;
use serde_json::json;
use serde_json::Value;
use std::sync::atomic::Ordering;

use crate::transport::types::AcpError;

use super::{AcpServerConfig, SessionState};
use crate::host::connection::ConnectionFacts;

pub(crate) mod acp_mcp;
pub(crate) mod config_options;
mod cron;
pub(crate) mod mcp_control;
mod mcp_oauth;
mod plugin;
mod rewind;
pub(super) mod session_caps;
pub(super) mod session_io;
pub(crate) mod session_lifecycle;
mod storage_v2;
mod user_input;
mod workflow;

#[cfg(test)]
pub(crate) use request_dispatch::handle_request;

pub(crate) use request_dispatch::handle_request_for_connection;

pub(crate) use request_dispatch::bind_session_tasks;

pub(super) use request_dispatch::task_snapshot_value;

#[cfg(test)]
#[path = "requests_test.rs"]
pub(super) mod tests;

#[path = "requests/request_dispatch.rs"]
mod request_dispatch;
