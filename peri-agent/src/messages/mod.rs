//! 消息契约类型 re-export（定义已下沉 peri-acp-types，本模块保持兼容路径）。
//!
//! 适配器（Anthropic/OpenAI）保留在本层，依赖经 re-export 的类型。

pub mod adapters;

pub use peri_acp_types::messages::{
    BaseMessage, ContentBlock, DocumentSource, ImageSource, MessageContent, MessageId,
    ToolCallRequest,
};
