#![cfg(unix)]
//! Standalone monitor gateway for the peri-browser MCP.
//!
//! The gateway is the human-facing half of the browser control plane: MCP
//! sessions register over a Unix socket, and a browser shows each session at a
//! secret URL (`/s/{token}/`). It never attaches a page controller itself; its
//! CDP viewer is the read-only side (README §5, §6).
//!
//! This stage provides the process skeleton only:
//!
//! - [`config`]: CLI/environment parsing, secret handling, socket paths.
//! - [`token`]: session tokens derived from the gateway secret.
//! - [`protocol`]: the full wire vocabulary in both directions.
//! - [`session`]: in-memory session table with per-session broadcast.
//! - [`registry`]: newline-delimited JSON over the MCP Unix socket.
//! - [`dev`]: standalone `--dev` discovery from Kitesurf's `/json/list`.
//! - [`cdp`]: read-only viewer connections to Kitesurf pages.
//! - [`stream`]: one screencast per active page, fanned out to monitor viewers.
//! - [`http`] / [`ws`]: session list, health probe, monitor page and socket.
//!
//! The gateway never becomes a page controller: CDP connections use
//! `?role=viewer`, so Kitesurf itself rejects input, navigation and
//! `Runtime.enable`. Human control is a connection switch owned by a later
//! stage; this stage serves frames, tabs, thumbnails and the address bar while
//! the agent keeps the writer role.
//!
//! The MCP registry uses a Unix-domain socket, so the crate is Unix-only: on
//! other targets the library is empty and the binary exits with a typed error.
//!
//! Design: `mcp-packages/peri-browser/README.md` section 6.

pub mod cdp;
pub mod config;
pub mod dev;
pub mod error;
pub mod http;
pub mod ownership;
pub mod protocol;
pub mod registry;
pub mod session;
pub mod stream;
pub mod token;
pub mod ws;
