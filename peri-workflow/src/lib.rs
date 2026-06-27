//! Perihelion Workflow 编排系统 —— 接入 claude-code workflow-engine。
//!
//! 通过 spawn Node 子进程运行 @peri-code/workflow npm 包，
//! stdio JSON-RPC 双向通信，agent 回调复用 v2 `run_react_loop`（`peri-agent::agent::stages`）。

pub mod error;
pub mod journal;
pub mod progress;
pub mod protocol;
pub mod registry;
pub mod rpc;
pub mod runner;
pub mod tool;
