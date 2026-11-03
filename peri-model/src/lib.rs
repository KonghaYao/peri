//! 与模型提供商无关的协议 DTO 和流式优先模型接口。

pub mod anthropic;
mod auth;
pub mod openai_compatible;
pub mod openai_responses;
#[cfg(test)]
mod prepared_stream_test;
pub mod prompt_cache;
pub mod protocol;
pub mod runtime;
mod transport;

pub use anthropic::{AnthropicConfig, AnthropicModel};
pub use auth::{RequestAuth, RequestCredential};
pub use openai_compatible::{OpenAiConfig, OpenAiModel};
pub use openai_responses::{ResponsesConfig, ResponsesModel, ResponsesProfile};
pub use protocol::{
    ContentBlock, DocumentSource, ImageSource, JsonObject, MediaType, Model, ModelCapabilities,
    ModelMessage, ModelRequest, ModelResponse, ModelStream, ModelStreamEvent, PreparedModelCall,
    ProviderProtocol, StopReason, TokenUsage, ToolCall, ToolDefinition, ToolResult,
};
pub use runtime::{
    ModelError, ModelErrorCategory, ModelErrorDiagnostic, ModelErrorDiagnosticParts, ModelResult,
    ModelRuntimeConfig, ObservedProviderBody, PreparedModelRequest, ProtocolError,
    ProtocolErrorKind, RetryConfig, RetryErrorKind, RetryObservation, RetryObserver,
    RetryableErrorClasses, TransportErrorKind,
};
