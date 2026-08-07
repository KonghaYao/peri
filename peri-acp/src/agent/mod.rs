//! Agent construction and lifecycle.
//!
//! 3.0 批 2 + L5 归位：`build_agent` / `build_stage_context`（AgentComponents 装配）
//! 装配桥在 `crate::host::stage_builder`（装配注入面，装配本体在
//! peri-agent session 工厂）；workflow agent 执行器已随 p1-wa 迁入
//! `peri_agent::agent::workflow`（session 运行单元归 Agent 层，§2），
//! ACP 侧保留装配面薄壳 `host/workflow_agent.rs`（注入面构造 +
//! session 级 WorkflowMiddleware 装配编排，装配经
//! `WorkflowMiddlewareFactory` 端口——peri-middlewares 实现）。
