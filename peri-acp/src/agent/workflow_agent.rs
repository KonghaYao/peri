//! Workflow agent 执行器（3.0 批 2 归位：实现在
//! `crate::host::exec::workflow_agent`，装配注入面；本模块 re-export 保兼容）。

pub use crate::host::exec::workflow_agent::{
    create_default_executor, create_executor, WorkflowAgentContext, WorkflowAgentExecutor,
};
