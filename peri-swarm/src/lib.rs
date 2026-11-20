//! Peri Swarm 的 HTTP 契约、领域骨架与进程内 ACP 运行时。
//!
//! - [`http`]：`/v1/agents/sessions` 五条路由、Bearer 认证、错误映射与 SSE 编码。
//! - [`domain`]：会话、输入、item、游标、事件与错误等纯类型。
//! - [`store`]：`SwarmStore` 写入/读取端口与内存 fake。
//! - [`service`]：`SwarmService` 端口与内存 fake；S4 起由 [`actor`] 的进程内
//!   ACP 实现同一端口。
//! - [`actor::swarm`] + [`gateway`]：S5 的 Rivet actor 与网关适配器；actor 持有
//!   会话，网关只做 `logicalId → rootSessionId` 路由与 SSE 转发。
//! - [`bootstrap`]：S6b 的进程入口（远端 Loom 存储、部署注入、registry + axum）。
//!
//! 证据边界：内存 fake 只证明协议与顺序契约，不代表真实 Peri 执行、Loom 持久化或
//! Rivet 路由行为。真实接线以设计文档 `.peri/plans/peri-swarm-impl.md` 的 S5-S9 为准。

pub mod actor;
pub mod bootstrap;
pub mod config;
pub mod domain;
mod event_stream;
pub mod gateway;
pub mod http;
pub mod service;
pub mod store;

pub use config::{ConfigError, SwarmConfig};
pub use domain::{ErrorCode, ItemPage, ItemView, LogicalId, SessionView, StreamEvent, SwarmError};
pub use http::{build_router, AppState};
pub use service::{InMemorySwarmService, SwarmService};
pub use store::{MemorySwarmStore, RemoteSwarmStore, SwarmStore};
