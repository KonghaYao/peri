//! Perihelion Workflow 编排系统 —— 接入 claude-code workflow-engine。
//!
//! 通过 npx / bunx 自动下载 @peri-code/workflow 并 spawn 子进程（优先 bunx），
//! stdio JSON-RPC 双向通信，agent 回调复用 v2 `run_react_loop`（`peri-agent::agent::stages`）。

pub mod error;
pub mod journal;
pub mod progress;
pub mod protocol;
pub mod registry;
pub mod rpc;
pub mod runner;
pub mod tool;
