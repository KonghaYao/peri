//! Perihelion Workflow 编排系统 —— 接入 claude-code workflow-engine。
//!
//! 通过固定版本（registry 安装到 `~/.peri/workflow/<version>/`）的 @peri-code/workflow
//! artifact spawn Node 子进程，stdio JSON-RPC 双向通信，agent 回调复用 v2 `run_react_loop`
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
