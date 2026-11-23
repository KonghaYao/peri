#![cfg(unix)]
//! `peri-browser` MCP server: Kitesurf CDP pages exposed as MCP tools.
//!
//! The design contract is `mcp-packages/peri-browser/README.md`. This crate is
//! the MCP half of the control plane: it owns page controllers, turns CDP into
//! tools, and registers with the optional monitor gateway. The gateway is a
//! separate process and never attaches a controller; human ownership is a
//! connection switch, not a shared connection (README §5, §6).
//!
//! Module map:
//!
//! - [`config`]: CLI and environment.
//! - [`error`]: typed failures and their stable tool codes.
//! - [`page`]: one page target, its controller and event pump.
//! - [`snapshot`]: accessibility snapshots and uid → `backendNodeId`.
//! - [`diagnostics`]: per-page console/network ring buffers.
//! - [`provision`]: machine-local create/close lock and the short browser
//!   connection.
//! - [`gateway`]: the newline-delimited registry client.
//! - [`core`]: session state and gateway orchestration.
//! - [`skills`]: bundled SEP-2640 skills.
//! - [`server`]: MCP tool, skill and resource surface.
//! - [`keys`], [`ring`]: small shared primitives.

pub mod config;
pub mod core;
pub mod diagnostics;
pub mod error;
pub mod gateway;
pub mod keys;
pub mod page;
pub mod provision;
pub mod ring;
pub mod server;
pub mod skills;
pub mod snapshot;

pub use config::BrowserConfig;
pub use core::BrowserCore;
pub use error::BrowserError;
pub use server::BrowserMcpServer;
