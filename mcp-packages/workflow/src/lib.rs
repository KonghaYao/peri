//! Workflow builtin MCP 实例：编排工具、资源与标准资源订阅。
//!
//! 设计依据：`docs/design/workflow-mcp.md` §4。本 crate 是 `WorkflowRuntime`
//! （`peri-workflow`）的 MCP adapter：工具 / 资源 / 订阅只做协议映射，业务语义
//! （启动参数校验、任务状态机、事件日志）全部归 runtime。
//!
//! 边界：不依赖 peri-middlewares / peri-acp / peri-tui（能力包只依赖契约与实现层）；
//! 会话装配把 `WorkflowRuntime` 经 [`WorkflowInstanceInput`] 注入 handler。

mod input;
mod resources;
mod server;

pub use input::{BindOutcome, WorkflowInstanceInput};
// 宿主消费者（`WorkflowTaskConsumer`）经本 crate 消费领域面：任务表 claim 结果、
// 任务记录与消费者令牌键——与 `WorkflowRuntime` 同一 re-export 面，宿主不直连
// `peri-workflow`（依赖方向：宿主经实例 crate 消费领域接口）。
pub use peri_workflow::runtime::{
    ClaimOutcome, WorkflowRuntime, WorkflowTaskRecord, WorkflowTaskState,
    WORKFLOW_CONSUMER_META_KEY,
};
pub use resources::{
    parse_uri, read_payload, update_uri, PageQuery, ResourceReadError, WorkflowResource, RUNS_URI,
    TASKS_URI, WORKFLOW_URI_PREFIX,
};
pub use server::WorkflowMcpServer;
