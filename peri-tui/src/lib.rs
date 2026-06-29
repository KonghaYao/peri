//! TUI interface for Rust Agent - interactive terminal playground

#![allow(
    clippy::if_same_then_else,
    clippy::needless_range_loop,
    clippy::reversed_empty_ranges,
    clippy::let_underscore_future,
    clippy::question_mark,
    clippy::collapsible_else_if,
    clippy::ptr_arg,
    clippy::infallible_try_from
)]

pub mod acp_client;
pub mod acp_server;
pub mod alloc_config;
pub mod app;
pub mod command;
pub mod config;
pub mod dto_convert;
pub mod event;
pub mod i18n;
pub mod panel;
pub mod render;
pub mod runtime;
pub mod state_machine;
pub mod sync;
pub mod thread;
pub mod ui;
pub mod update;

#[cfg(test)]
mod alloc_config_test;
