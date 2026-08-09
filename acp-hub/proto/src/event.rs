//! `event` 帧：S→C 事件推送 envelope（§4.3.1）。
//!
//! 边界声明（架构 §6.1）：`frame` 为**不透明 JSON**——`NormalizedEvent`
//! （ACPChannel 产物）定义在 `server/src/protocol/acp-channel`；本模块只承载
//! envelope（`chat_id`/`seq`/`frame`）。

use serde::{Deserialize, Serialize};

/// `events/subscribe` 推送帧载荷（§4.3.1）。
///
/// `frame` 为规范化事件（hub 侧 chat_id 经 binding 翻译——不透传原始
/// acp_chat_id）。双流顺序契约：视图收敛以 yjs 为准，事件流尽力而为，
/// 双流之间无顺序契约。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventFrame {
    /// hub 侧 chat_id（经 binding 翻译，非原始 acp_chat_id）。
    pub chat_id: String,
    /// hub 侧单调序号。
    pub seq: u64,
    /// 规范化事件（不透明 JSON，结构定义在 server ACPChannel）。
    pub frame: serde_json::Value,
}
