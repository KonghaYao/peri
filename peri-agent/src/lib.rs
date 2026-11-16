//! # peri-agent
//!
//! Rust Agent framework with middleware system.
//! Aligned with `@langgraph-js/standard-agent` (TypeScript).
//!
//! ## API Stability（P1-8）
//!
//! peri-agent 公开 70+ 类型，目前无显式 stability 分层。以下为非正式约定：
//!
//! | 层级 | 含义 | 示例 |
//! |------|------|------|
//! | **stable** | 向后兼容保证，跨 minor 版本不变 | `BaseMessage`, `MessageContent`, `AgentError`, `Middleware` trait |
//! | **unstable** | 内部实现细节，可能在任何版本改变 | `StageContext`, `AgentContext`, `LoopState`, `compact_v2` 内部函数 |
//! | **internal** | 仅供 peri-middlewares/peri-acp 桥接使用 | `GoalController`, `GoalStateView`, `AgentEventBus` |
//!
//! **使用约定**：
//! - 外部消费者（`peri-tui` 应通过 ACP transport 通信）：仅依赖 `stable`
//! - `peri-middlewares`：可依赖 `stable` + `internal`
//! - `peri-acp`：可依赖所有层级
//!
//! 已部分实施（`#[doc(hidden)]`），后续计划：引入 feature gates 做进一步的编译期 enforcement。

pub mod agent;
pub mod context_archive_reader;
pub use context_archive_reader as context_archive;
pub mod error;
pub mod goal;
pub mod hitl;
pub mod interaction;
pub mod messages;
pub mod metrics;
pub mod middleware;
#[cfg(not(target_os = "emscripten"))]
pub mod resources;
pub mod session;
pub mod telemetry;
pub mod thread;
pub mod tools;

/// Prelude - 常用类型一次性导入
pub mod prelude {
    pub use crate::agent::events::AgentEventHandler;
    pub use crate::agent::events::ExecutorEvent;
    pub use crate::agent::events::FnEventHandler;
    pub use crate::agent::events_v2::Event;
    pub use crate::agent::events_v2::EventBus;
    pub use crate::agent::events_v2::EventBusConfig;
    pub use crate::agent::events_v2::EventHandles;
    pub use crate::agent::events_v2::ObserveEvent;
    pub use crate::agent::events_v2::RenderEvent;
    pub use crate::agent::events_v2::StateEvent;
    pub use crate::agent::events_v2::TurnErrorReason;
    pub use crate::agent::react::AgentInput;
    pub use crate::agent::react::AgentOutput;
    pub use crate::agent::react::ReactLLM;
    pub use crate::agent::react::Reasoning;
    pub use crate::agent::react::ToolCall;
    pub use crate::agent::react::ToolResult;
    pub use crate::agent::state::AgentState;
    pub use crate::agent::token::ContextBudget;
    pub use crate::agent::token::TokenTracker;
    pub use crate::agent::AgentCancellationToken;
    pub use crate::error::AgentError;
    pub use crate::error::AgentResult;
    pub use crate::hitl::BatchItem;
    pub use crate::hitl::HitlDecision;
    pub use crate::messages::BaseMessage;
    pub use crate::messages::ContentBlock;
    pub use crate::messages::DocumentSource;
    pub use crate::messages::ImageSource;
    pub use crate::messages::MessageContent;
    pub use crate::messages::ToolCallRequest;
    pub use crate::middleware::capabilities;
    pub use crate::middleware::r#trait::Middleware;
    pub use crate::middleware::state::MiddlewareState;
    pub use crate::middleware::LoggingMiddleware;
    pub use crate::middleware::MetricsMiddleware;
    pub use crate::middleware::MiddlewareChain;
    pub use crate::middleware::NoopMiddleware;
    pub use crate::session::FrozenContext;
    pub use crate::session::FrozenContextBuilder;
    pub use crate::session::MessageKind;
    pub use crate::session::MessageQueue;
    pub use crate::session::MessageSource;
    pub use crate::session::MessageTranscript;
    pub use crate::session::PermissionMode;
    pub use crate::session::QueuedMessage;
    pub use crate::session::Session;
    pub use crate::session::SessionConfig;
    pub use crate::session::SessionId;
    pub use crate::session::SessionStore;
    pub use crate::session::ThinkingConfig;
    pub use crate::session::TurnContext;
    pub use crate::session::TurnId;
    pub use crate::tools::BaseTool;
    pub use crate::tools::ToolDefinition;
}
