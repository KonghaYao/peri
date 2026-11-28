//! Receive loop and request-class dispatch; long-running work stays host-owned.

mod history;
mod notification;
mod stdio_preview_config;
mod stdio_preview_input;

use std::sync::Arc;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::connection::ConnectionContext;
use super::diagnostics::ResponseDiagnostics;
use super::history_page;
use super::history_window;
use super::mcp_apps;
use super::requests;
use super::session_endpoint;
use super::task_scope;
use super::AcpServerConfig;
use super::PromptLocks;
use super::SharedSessions;
use crate::transport::types::{AcpError, IncomingMessage, RequestId};

pub(super) use server_loop::ServerLoop;

#[path = "server_loop/server_loop.rs"]
#[expect(
    clippy::module_inception,
    reason = "The facade preserves API paths; the leaf is named for its primary type as required by the Rust guide."
)]
mod server_loop;
