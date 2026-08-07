//! # peri-resources
//!
//! Resources 层（§0：外部系统数据，访问通道归 Resources，持有按生命周期）。
//! 以 context 形式提供给 Agent / Middleware / Controller。
//!
//! - `config` — peri-config：直操配置文件（settings.json 等）
//! - `sessions` — peri-sessions：直操 sqlite（`SqliteThreadStore` 实现迁入）
//! - `lsp` — peri-lsp 资源实现门面（类型/能力出口，消费方不直接依赖 peri-lsp）
//! - `workflow` — peri-workflow 资源实现门面（类型/能力出口，消费方不直接依赖 peri-workflow）
//! - `context` — Resources 门面：唯一实例化入口

pub mod config;
pub mod context;
pub mod lsp;
pub mod sessions;
pub mod workflow;

pub use context::Resources;
