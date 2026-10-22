//! History V2 正文描述符、分块结果与 close 结果（设计 §7.4、§7.5、§8.3）。

use serde::{Deserialize, Serialize};

use super::error::HistoryContractError;
use super::integer::DecimalU64;
use super::negotiation::HistoryLimits;
use super::{deserialize_required_option, require_token, require_version};

/// 可见正文的表示方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HistoryRepresentation {
    DisplayText,
    DisplayBinary,
    DisplayJson,
}

/// 正文块编码；V2 固定 base64。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HistoryEncoding {
    Base64,
}

/// 外置正文的描述符；本身必须有界，不承载正文。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryBodyDescriptor {
    pub body_ref: String,
    pub representation: HistoryRepresentation,
    pub media_type: String,
    /// 该表示的已知总长度。
    ///
    /// V2 page 的 opaque 描述符只能给出 canonical payload 的预估长度；展示表示的真实
    /// 长度以 body 响应的 `totalBytes` 为准，客户端在首块到达后修正滚动范围。
    pub total_bytes: DecimalU64,
    pub chunk_bytes: u32,
}

impl HistoryBodyDescriptor {
    /// 校验描述符身份、媒体类型与块大小预算。
    pub fn validate(&self, limits: &HistoryLimits) -> Result<(), HistoryContractError> {
        require_token("bodyRef", &self.body_ref)?;
        if self.media_type.is_empty() {
            return Err(HistoryContractError::invalid_request(
                "mediaType must not be empty",
            ));
        }
        let max = limits.max_body_chunk_bytes.max(1);
        if self.chunk_bytes == 0 || self.chunk_bytes > max {
            return Err(HistoryContractError::limits(format!(
                "chunkBytes must be within 1..={max}"
            )));
        }
        Ok(())
    }
}

/// 有界正文块。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryBodyChunk {
    pub version: u32,
    pub kind: HistoryBodyKind,
    pub session_id: String,
    pub window_id: String,
    pub snapshot_token: String,
    pub body_ref: String,
    pub content_version: String,
    pub representation: HistoryRepresentation,
    pub encoding: HistoryEncoding,
    pub offset: DecimalU64,
    pub data: String,
    /// 必填，可 null；complete 时固定为 null。
    #[serde(deserialize_with = "deserialize_required_option")]
    pub next_offset: Option<DecimalU64>,
    pub complete: bool,
    /// 当前表示（投影后的展示字节）的总长度。
    ///
    /// 这是 body 响应的权威长度：页面描述符里的 `totalBytes` 只是 canonical payload
    /// 的预估，客户端在首块到达后用本字段修正滚动范围。每个 V2 body 响应都必须填写。
    pub total_bytes: DecimalU64,
}

/// `kind:"body"`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HistoryBodyKind {
    Body,
}

impl HistoryBodyChunk {
    /// 只校验 wire 形状（不校验 base64 或内容长度预算）。
    ///
    /// `complete=true` 时 `nextOffset` 必须为 null；未完成时必须是严格前进的
    /// `nextOffset`。EOF 允许空 `data`，因为类型层不解释字节内容。
    pub fn validate_shape(&self) -> Result<(), HistoryContractError> {
        require_version(self.version)?;
        if self.complete {
            if self.next_offset.is_some() {
                return Err(HistoryContractError::shape(
                    "complete body chunk must not carry nextOffset",
                ));
            }
            return Ok(());
        }
        let Some(next) = self.next_offset else {
            return Err(HistoryContractError::shape(
                "incomplete body chunk requires nextOffset",
            ));
        };
        if next <= self.offset {
            return Err(HistoryContractError::shape(
                "incomplete body chunk must advance nextOffset",
            ));
        }
        Ok(())
    }
}

/// `close` 结果。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryClosedResult {
    pub version: u32,
    pub kind: HistoryClosedKind,
    pub window_id: String,
}

/// `kind:"closed"`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HistoryClosedKind {
    Closed,
}
