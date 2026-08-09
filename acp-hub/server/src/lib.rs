//! acp-hub-server —— 常驻后台中心控制面（骨架阶段）
//!
//! 模块预声明（占位）：后续并行 feature agent 只写各自文件，不碰 lib.rs。
//! 职责映射见 docs/architecture.md §12 目录结构。

pub mod auth;
pub mod channel;
pub mod config;
pub mod control;
pub mod persist;
pub mod protocol;
pub mod state;
pub mod web;
