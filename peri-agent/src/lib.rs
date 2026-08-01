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
pub mod error;
pub mod error_suggest;
pub mod goal;
pub mod group;
pub mod hitl;
pub mod interaction;
pub mod messages;
pub mod metrics;
pub mod middleware;
pub mod session;
pub mod telemetry;
pub mod thread;
pub mod tools;

/// Prelude - 常用类型一次性导入
pub mod prelude {
    pub use crate::{
        agent::{
            events::{AgentEventHandler, ExecutorEvent, FnEventHandler},
            events_v2::{
                Event, EventBus, EventBusConfig, EventHandles, ObserveEvent, RenderEvent,
                StateEvent, TurnErrorReason,
            },
            react::{AgentInput, AgentOutput, ReactLLM, Reasoning, ToolCall, ToolResult},
            state::AgentState,
            token::{ContextBudget, TokenTracker},
            AgentCancellationToken,
        },
        error::{AgentError, AgentResult},
        group::AgentGroup,
        hitl::{BatchItem, HitlDecision},
        messages::{
            BaseMessage, ContentBlock, DocumentSource, ImageSource, MessageContent, ToolCallRequest,
        },
        middleware::{
            r#trait::Middleware, state::MiddlewareState, LoggingMiddleware, MetricsMiddleware,
            MiddlewareChain, NoopMiddleware,
        },
        session::{
            FrozenContext, FrozenContextBuilder, MessageKind, MessageQueue, MessageSource,
            MessageTranscript, PermissionMode, QueuedMessage, Session, SessionConfig, SessionId,
            SessionStore, ThinkingConfig, TurnContext, TurnId,
        },
        tools::{BaseTool, ToolDefinition},
    };
}
