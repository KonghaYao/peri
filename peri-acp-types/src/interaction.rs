//! Interaction DTOs -- 取代 peri_agent::interaction::{InteractionContext, ...}
//!
//! 注意：InteractionContext / InteractionResponse 在 acp_server 边界仍使用
//! 原始类型（因为包含 oneshot::Sender 等 runtime channel）。
//! 本 DTO 仅供 TUI 层非 bridge 文件的数据展示使用。

use serde::{Deserialize, Serialize};

/// InteractionContext 的 DTO 形式，用于 TUI 展示层。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InteractionContextDto {
    pub session_id: String,
    pub prompt_id: String,
    pub channel_state: ChannelStateDto,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ChannelStateDto {
    Ready,
    WaitingForResponse { prompt: String },
    Closed,
}

/// InteractionResponse 的 DTO 形式。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum InteractionResponseDto {
    Text { text: String },
    Cancelled,
    Error { message: String },
}
