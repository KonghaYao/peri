//! # peri-resources
//!
//! Resources 层（§0：外部系统数据，访问通道归 Resources，持有按生命周期）。
//! 以 context 形式提供给 Agent / Middleware / Controller。
//!
//! - `config` — peri-config：直操配置文件（settings.json 等）
//! - `sessions` — peri-sessions：本机 SQLite 或 Turso 远端数据 adapter 与工作区校验组合
//! - `workflow` — 原生目标的 peri-workflow 资源实现门面
//! - `context` — Resources 门面：唯一实例化入口

pub mod config;
pub mod context;
pub mod sessions;
#[cfg(not(target_os = "emscripten"))]
pub mod workflow;

pub use context::{classify_open_failure, Resources, SessionStoreShutdownOwner, StoreOpenFailure};
pub use sessions::RemoteWorkspaceEnvironment;
