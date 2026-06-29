//! v2 面板实现。
//!
//! 每个面板一个子模块，迁移自 legacy `app/*_panel.rs`。
//! 通过 `super::registry::create_panel` 工厂接入。

pub mod agent;
pub mod betas;
pub mod config;
pub mod cron;
pub mod hooks;
pub mod login;
pub mod mcp;
pub mod memory;
pub mod model;
pub mod plugin;
pub mod status;
pub mod tasks;
pub mod thread_browser;
pub mod workflow;
