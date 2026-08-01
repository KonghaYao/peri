#[doc(hidden)]
pub mod agent_context;
#[doc(hidden)]
pub mod compact_v2;
pub mod events;
#[doc(hidden)]
pub mod events_v2;
#[doc(hidden)]
pub mod events_v2_mapper;
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
