//! acp-instance —— 由原 acp-hub 单机版演化（docs/architecture.md §3.2/§12）
//!
//! 职责：每台机器一个 daemon，outbound 连 server，收 spawn/kill 指令，
//! 管理 ACP 进程树，透明转发 + 断线缓冲。
//!
//! 模块预声明：后续并行 feature agent 只写各自文件，不碰 lib.rs。

pub mod auth;
pub mod buffer;
pub mod child;
pub mod error;
pub mod global;
pub mod hub;
pub mod router;
pub mod transport;
