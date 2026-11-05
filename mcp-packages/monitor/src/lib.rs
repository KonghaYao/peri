//! Builtin `monitor` MCP instance.
//!
//! One command source per task: the command's stdout/stderr become line events
//! in an in-memory ring buffer. Task state is never persisted. Standard MCP
//! Tasks carry lifecycle (`tasks/get`, `tasks/cancel`); event cursors live in
//! `monitor_read`.
//!
//! Design: `docs/design/monitor-mcp.md`.

pub mod buffer;
mod buffer_budget;
pub mod server;
pub mod state;
pub mod tools;

pub use server::MonitorMcpServer;
pub use state::{
    MonitorError, MonitorState, MonitorStatus, ReadResult, StartRequest, StartedMonitor,
    TaskSnapshot,
};
