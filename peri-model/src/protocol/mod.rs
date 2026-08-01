mod model;
mod types;

pub use crate::runtime::{ModelError, ModelResult};
pub use model::{Model, ModelStream, ModelStreamEvent};
pub use types::{
    ContentBlock, DocumentSource, ImageSource, JsonObject, MediaType, ModelCapabilities,
    ModelMessage, ModelRequest, ModelResponse, ProviderProtocol, StopReason, TokenUsage, ToolCall,
    ToolDefinition, ToolResult,
};

#[cfg(test)]
#[path = "types_test.rs"]
mod types_test;

#[cfg(test)]
#[path = "model_test.rs"]
mod model_test;
