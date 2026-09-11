//! 用户输入队列的 ACP 契约；执行与生命周期归 Agent mailbox。

use crate::messages::MessageContent;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInput {
    /// 与最终 Human 消息共用的稳定 UUID。
    pub input_id: String,
    pub content: MessageContent,
    /// 未经 trim 或输入准备改写的编辑器原文。
    pub original_draft: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserInputState {
    Queued,
    Dispatching,
    Claimed,
    Delivered,
    Withdrawn,
    /// 请求的输入身份不在本会话队列记录中。
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputQueueItem {
    pub input_id: String,
    pub content: MessageContent,
    pub original_draft: String,
    pub state: UserInputState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputQueueSnapshot {
    pub session_id: String,
    /// 内存 mailbox 实例身份，重建会话后更换。
    pub generation: String,
    pub revision: u64,
    /// 已绑定真实执行的队列 run；仅预留、尚未启动时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_request_id: Option<String>,
    pub items: Vec<UserInputQueueItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnqueueUserInputRequest {
    pub session_id: String,
    pub generation: String,
    pub command_id: String,
    pub input_id: String,
    pub content: MessageContent,
    pub original_draft: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DispatchUserInputsRequest {
    pub session_id: String,
    pub generation: String,
    pub command_id: String,
    /// 点击时的明确集合；重试不得重新从队列中全选。
    pub input_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TakeBackUserInputRequest {
    pub session_id: String,
    pub generation: String,
    pub command_id: String,
    pub input_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserInputQueueSnapshotRequest {
    pub session_id: String,
    #[serde(default)]
    pub generation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputItemResult {
    pub input_id: String,
    pub state: UserInputState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputQueueReceipt {
    pub snapshot: UserInputQueueSnapshot,
    /// 命令首次裁决的结果；重试时可与最新 snapshot 的状态不同。
    pub results: Vec<UserInputItemResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taken_back: Option<UserInput>,
}

#[cfg(test)]
#[path = "user_input_test.rs"]
mod tests;
