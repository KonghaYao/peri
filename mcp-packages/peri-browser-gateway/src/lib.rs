#![cfg(unix)]
//! 一个 loopback listener 提供有状态 MCP 和 token 监控；浏览器归属按 MCP 会话划分。

pub mod cdp;
pub mod cli;
pub mod config;
mod core;
pub mod daemon;
mod diagnostics;
pub mod error;
mod handoff;
pub mod http;
mod input;
mod keys;
pub mod lifecycle;
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

/// Version advertised by `--version` and the MCP `initialize` response.
///
/// Release builds may inject `PERI_BROWSER_BUILD_VERSION`; source builds report
/// the crate version.
#[must_use]
pub fn version() -> &'static str {
    option_env!("PERI_BROWSER_BUILD_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}
