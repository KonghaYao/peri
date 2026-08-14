//! y-sync 帧 envelope（§4.2 / §5.6）。
//!
//! `ysync.*` 帧体为 y-sync 协议消息（`Y.encodeStateAsUpdate` / update diff），
//! **base64 嵌入文本帧**（与 chat `broadcaster.ts` 的
//! `Buffer.toString("base64")` 一致，§4.1）；固定 update 编码版本 v1
//! （[`crate::version::Y_UPDATE_ENCODING_VERSION`]）。

use serde::{Deserialize, Serialize};

use crate::conn::DocId;

/// Browser/server capability required for the conservative prompt delivery
/// pipeline. Capabilities are negotiated on the first subscription and bound
/// to that connection; actions cannot self-assert them.
pub const CAP_PROMPT_DELIVERY_V2: &str = "prompt-delivery-v2";

/// `ysync.subscribe`：订阅指定 Doc 的更新（多 session 视图必需，§4.2）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YsyncSubscribe {
    /// `["chat:{sid}", ...]` 形态的 doc 名列表。
    pub docs: Vec<DocId>,
    /// 客户端支持的可选协议能力。缺失等价于空集合，保持旧客户端兼容；空集合
    /// 不上行，避免改变既有 JSON 形态。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub client_capabilities: Vec<String>,
}

/// `ysync.unsubscribe`：退订（§4.2）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YsyncUnsubscribe {
    pub docs: Vec<DocId>,
}

/// `ysync.update`：S→C **单向**增量/快照广播（§4.2 / §5.6）。
///
/// server 是唯一写入者，客户端无写权限、不持有写租约——客户端上行一律拒绝。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YsyncUpdate {
    pub doc: DocId,
    /// base64（`Y.encodeStateAsUpdate` 输出，编码版本 v1）。
    pub update: String,
    /// **快照必带**（§4.6 步骤 3：全量快照携带各 Doc 的 `projection_version`，
    /// 远端据此判断是否需要校准显示）；增量**不携带**该字段（§4.6 原文语义，
    /// 序列化时 `None` 跳过，不输出 `null`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projection_version: Option<u32>,
}

/// `ysync.sync`：y-sync Step 1/2 消息（§5.6 不采用双向增量握手，保留定义）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YsyncSync {
    /// base64 编码的 y-sync 消息。
    pub msg: String,
}

/// `ysync.awareness`：y-protocol awareness（M3 启用，保留定义）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YsyncAwareness {
    /// base64 编码的 awareness 消息。
    pub msg: String,
}
