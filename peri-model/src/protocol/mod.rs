mod model;
mod responses_history;
mod types;

pub use crate::runtime::{ModelError, ModelResult};
pub use model::{Model, ModelStream, ModelStreamEvent};
pub use responses_history::{
    AssistantPhase, HistoryError, ResponsesHistoryItem, ResponsesHistoryItemKind,
    ResponsesHistoryV1, ResponsesSourceIdentity, RESPONSES_HISTORY_VERSION,
};
pub use types::{
    ContentBlock, DocumentSource, ImageSource, JsonObject, MediaType, ModelCapabilities,
    ModelMessage, ModelRequest, ModelResponse, ProviderProtocol, StopReason, TokenUsage, ToolCall,
    ToolDefinition, ToolResult,
};

#[cfg(test)]
#[path = "responses_history_test.rs"]
mod responses_history_test;

#[cfg(test)]
#[path = "types_test.rs"]
mod types_test;

#[cfg(test)]
#[path = "model_test.rs"]
mod model_test;
