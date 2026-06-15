//! PTY server 库入口，供集成测试引用。

pub mod config;
pub mod http_routes;
pub mod pty_session;
pub mod session_state;
pub mod ws_handler;

#[cfg(test)]
mod config_test;
#[cfg(test)]
mod http_routes_test;
#[cfg(test)]
mod pty_session_test;
#[cfg(test)]
mod ws_handler_test;

// 供 `#[cfg(test)] mod xxx_test` 中 `use super::*` 找到顶层类型
// （config_test 用 Config，pty_session_test 用 PtySession，ws_handler_test 用 WsQuery）
#[cfg(test)]
use config::Config;
#[cfg(test)]
use pty_session::PtySession;
#[cfg(test)]
use ws_handler::WsQuery;
