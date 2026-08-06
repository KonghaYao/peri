//! Agent construction and lifecycle.
//!
//! 3.0 批 2 + L5 归位：`build_agent` / `build_stage_context`（AgentComponents 装配）
//! 装配桥在 `crate::host::stage_builder`（装配注入面，装配本体在
//! peri-agent session 工厂）；workflow agent 执行器在 `crate::host::workflow_agent`
//! （ACP 装配面宿主——深绑 ACP provider/prompt/AgentPool 与
//! peri-middlewares / peri-workflow 装配面，§0 边 8 禁止迁入 peri-agent，
//! 见 `host/workflow_agent.rs` 归位裁定）。
