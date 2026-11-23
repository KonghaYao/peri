#![cfg(unix)]
//! 一个 loopback listener 提供有状态 MCP 和 token 监控；浏览器归属按 MCP 会话划分。

pub mod cdp;
pub mod config;
mod core;
mod diagnostics;
pub mod error;
mod handoff;
pub mod http;
mod input;
mod keys;
mod mcp_http;
mod onboarding;
mod page;
mod protocol;
mod provision;
mod ring;
mod server;
mod session;
mod skills;
mod snapshot;
mod stream;
mod token;
mod ws;

pub use http::serve;
