//! Agent construction and lifecycle.
//!
//! 3.0 批 2 归位：`build_agent` / `build_stage_context`（AgentComponents 装配）
//! 随执行面迁至 `crate::host::exec::stage_builder`（装配注入面，L5 物理迁入
//! peri-agent session 工厂）；workflow agent 执行器迁至
//! `crate::host::exec::workflow_agent`（本模块保留 re-export 保调用方兼容）。

pub mod workflow_agent;
