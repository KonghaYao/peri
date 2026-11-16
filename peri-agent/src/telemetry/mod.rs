//! Tracing 日志模块
//!
//! ## 控制开关
//!
//! 通过环境变量控制：
//!
//! | 环境变量 | 说明 |
//! |---|---|
//! | `RUST_LOG` | 日志级别，默认 `info` |
//! | `RUST_LOG_FORMAT=json` | 使用 JSON 格式输出 |
//!
//! ## 使用方式
//!
//! 调用一次 [`init_tracing`]，其余自动处理：
//!
//! ```rust,no_run
//! #[tokio::main]
//! async fn main() {
//!     let _guard = peri_agent::telemetry::init_tracing("my-agent");
//! }
//! ```

mod subscriber;

pub use subscriber::{init_tracing, TracingGuard};
