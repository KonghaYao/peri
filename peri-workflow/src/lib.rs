//! Perihelion Workflow 编排系统 —— 接入 claude-code workflow-engine。
//!
//! 通过随二进制分发的 sidecar（首次使用发布到 `~/.peri/workflow/<version>/<内容身份>/`
//! 后离线复用）运行 @peri-code/workflow artifact：Rust 侧零 JS 嵌入，只按 sha256 摘要
//! pin 校验构建产出；stdio JSON-RPC 双向通信，agent 回调复用 v2 `run_react_loop`
//!（`peri-agent::agent::stages`）。

pub mod cli;
mod completion;
pub mod error;
pub mod journal;
mod preflight;
mod process;
pub mod progress;
pub mod protocol;
pub mod registry;
pub mod rpc;
pub mod runner;
pub mod runtime;
pub mod tool;
