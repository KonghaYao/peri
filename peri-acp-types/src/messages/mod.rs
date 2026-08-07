//! 消息契约类型（自 peri-agent/src/messages 下沉，接口契约归 peri-acp-types）。
//!
//! 纯数据 + 序列化类型，供 ThreadStore 契约（`crate::store`）与各层引用。
//! peri-agent::messages 保留 re-export 兼容路径。

pub mod content;
pub mod message;

pub use content::{
    strip_system_reminders, ContentBlock, DocumentSource, ImageSource, MessageContent,
};
pub use message::{BaseMessage, MessageId, ToolCallRequest};
