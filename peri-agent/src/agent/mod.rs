#[doc(hidden)]
pub mod agent_context;
/// async tasks manager（Agent 层，per-session 实例化）：
/// `BackgroundTaskRegistry` 定义与 bg shell 实际执行（进程 spawn/进程组/超时/
/// 输出收集）归此模块，经 `TaskManager` 聚合（L1 迁移点）。
pub mod async_tasks;
#[doc(hidden)]
pub mod compact_v2;
pub mod events;
#[doc(hidden)]
pub mod events_v2;
#[doc(hidden)]
pub mod langfuse_bridge;
pub mod model_bridge;
pub mod react;
pub mod session;
pub mod stages;
pub mod state;
#[doc(hidden)]
pub mod subagent_event_forwarder;
#[doc(hidden)]
pub mod token;
/// workflow agent 执行单元（p1-wa 归位：session 运行单元归 Agent 层；
/// 装配经 [`workflow::factory`] 端口注入，§0 边 8）。
pub mod workflow;

#[doc(hidden)]
pub use compact_v2::CompactConfig;
#[doc(hidden)]
pub use events::{AgentEventHandler, BackgroundTaskResult, ExecutorEvent, FnEventHandler};
#[doc(hidden)]
pub use langfuse_bridge::LangfuseBridgeLike;
// P5.5：v1 executor/ 已物理删除。AgentCancellationToken 保留为 tokio_util alias，
// 众多模块（ACP / SubAgent / Workflow）依赖此类型名。
pub use react::{AgentInput, AgentOutput, ReactLLM, Reasoning, ToolCall, ToolResult};
pub use state::AgentState;
#[doc(hidden)]
pub use token::{ContextBudget, TokenTracker};
pub use tokio_util::sync::CancellationToken as AgentCancellationToken;
