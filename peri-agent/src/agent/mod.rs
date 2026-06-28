pub mod compact;
pub mod compact_v2;
pub mod events;
pub mod events_v2;
pub mod events_v2_mapper;
pub mod react;
pub mod session;
pub mod stages;
pub mod state;
pub mod subagent_event_forwarder;
pub mod token;

pub use compact::CompactConfig;
pub use events::{
    inject_source_agent_id, AgentEvent, AgentEventHandler, BackgroundTaskResult, ExecutorEvent,
    FnEventHandler,
};
// P5.5：v1 executor/ 已物理删除。AgentCancellationToken 保留为 tokio_util alias，
// 众多模块（ACP / SubAgent / Workflow）依赖此类型名。
pub use react::{AgentInput, AgentOutput, ReactLLM, Reasoning, ToolCall, ToolResult};
pub use state::AgentState;
pub use token::{ContextBudget, TokenTracker};
pub use tokio_util::sync::CancellationToken as AgentCancellationToken;
