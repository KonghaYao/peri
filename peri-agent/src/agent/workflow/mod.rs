//! Workflow agent 执行单元（p1-wa 归位：自 `peri-acp::host::workflow_agent` 迁入）。
//!
//! §2（Agent 层）：workflow agent 是 session 运行单元（内部自建 session +
//! run_react_loop 驱动 + 结果收集），执行体随本模块归 Agent 层；装配需求
//! （中间件链 / 工具 / provider 投影 / 事件发射）经 [`factory`] 端口与注入
//! 闭包参数化，ACP 宿主装配面（`host/workflow_agent.rs` 薄壳）构造注入。

pub mod agent;
pub mod factory;

pub use agent::{
    create_default_executor, create_executor, WorkflowAgentContext, WorkflowAgentExecutor,
};
pub use factory::{
    WorkflowAgentDefinition, WorkflowAgentPromptBuilder, WorkflowMiddlewareFactory, WorkflowModel,
    WorkflowModelFactory, WorkflowPublishHook, WorkflowSystemPromptFallback,
};
