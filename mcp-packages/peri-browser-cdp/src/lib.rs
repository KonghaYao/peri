//! Shared CDP page-connection layer for the peri-browser MCP and monitor gateway.
//!
//! Both processes ride the same `/devtools/page/{targetId}` WebSocket and need
//! the same plumbing: pending-command resolution, events, typed input helpers
//! and screencast frames (README §6.4, §11). Keeping one implementation here
//! means the monitor never grows a second coordinate/key mapping.
//!
//! - [`cdp`]: the connections (`Viewer`, `Controller`), errors and helpers.
//! - [`input`]: the input enums shared with the monitor wire protocol.
//!
//! Design: `mcp-packages/peri-browser/README.md` sections 6 and 7.

pub mod cdp;
pub mod input;

pub use cdp::*;
pub use input::{KeyEventKind, MouseButton, MouseEventKind};
