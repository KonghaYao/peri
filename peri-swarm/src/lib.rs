//! Peri Swarm 的 HTTP 契约与领域骨架。
//!
//! 本 crate 在 S3 阶段只交付协议面与 typed 端口，不接触真实 ACP/Rivet/Loom：
//!
//! - [`http`]：`/v1/agents/sessions` 五条路由、Bearer 认证、错误映射与 SSE 编码。
//! - [`domain`]：会话、输入、item、游标、事件与错误等纯类型。
//! - [`store`]：`SwarmStore` 写入/读取端口与内存 fake。
//! - [`service`]：`SwarmService` 端口与内存 fake；后续阶段由 Rivet Actor 实现同一端口。
//!
//! 证据边界：内存 fake 只证明协议与顺序契约，不代表真实 Peri 执行、Loom 持久化或
//! Rivet 路由行为。真实接线以设计文档 `.peri/plans/peri-swarm-impl.md` 的 S4-S9 为准。

pub mod config;
pub mod domain;
pub mod http;
pub mod service;
pub mod store;

pub use config::{ConfigError, SwarmConfig};
pub use domain::{ErrorCode, ItemPage, ItemView, LogicalId, SessionView, StreamEvent, SwarmError};
pub use http::{build_router, AppState};
pub use service::{InMemorySwarmService, SwarmService};
pub use store::{MemorySwarmStore, SwarmStore};
