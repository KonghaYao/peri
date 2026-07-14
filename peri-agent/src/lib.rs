//! # peri-agent
//!
//! Rust Agent framework with middleware system.
//! Aligned with `@langgraph-js/standard-agent` (TypeScript).

pub mod agent;
pub mod ask_user;
pub mod error;
pub mod error_suggest;
pub mod goal;
pub mod group;
pub mod hitl;
pub mod interaction;
pub mod llm;
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
        ask_user::{AskUserBatchRequest, AskUserOption, AskUserQuestionData},
        error::{AgentError, AgentResult},
        group::{AgentGroup, CancelPolicy},
        hitl::{BatchItem, HitlDecision},
        llm::{BaseModel, BaseModelReactLLM, ChatAnthropic, ChatOpenAI, MockLLM},
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
