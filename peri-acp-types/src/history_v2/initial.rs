//! History V2 `session/load` 首屏契约（设计 §6.1、§6.2）。
//!
//! 扩展请求与结果都放在 `_meta["peri.history"]`。initial 只允许最新窗口；旧顶层
//! `cursor`/`limit`/`loadGeneration`/`history` 混入时必须显式失败，不能回落到全量回放。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::error::HistoryContractError;
use super::integer::DecimalU64;
use super::negotiation::HistoryLimits;
use super::page::HistoryWindowPage;
use super::{
    require_item_limit, require_request_key, require_target_bytes, require_version,
    HISTORY_V2_VERSION,
};

/// `_meta["peri.history"]` 的首屏请求。
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryInitialRequest {
    pub version: u32,
    pub request_key: DecimalU64,
    pub limit: u32,
    pub target_bytes: u64,
}

impl HistoryInitialRequest {
    /// 解码严格 initial 请求；`action` 必须为 `initial` 且不进入结构。
    pub fn decode(value: &Value) -> Result<Self, HistoryContractError> {
        let object = value.as_object().ok_or_else(|| {
            HistoryContractError::invalid_request("history initial request must be an object")
        })?;
        let action = object
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                HistoryContractError::invalid_request("history initial request.action is required")
            })?;
        if action != "initial" {
            return Err(HistoryContractError::invalid_request(
                "history initial request.action must be \"initial\"",
            ));
        }
        let mut fields = object.clone();
        fields.remove("action");
        serde_json::from_value(Value::Object(fields)).map_err(|error| {
            tracing::debug!(
                line = error.line(),
                column = error.column(),
                "history v2 initial request rejected"
            );
            HistoryContractError::invalid_request("history initial fields are invalid")
        })
    }

    /// 校验版本、操作号、条数与字节预算。
    pub fn validate(&self, limits: &HistoryLimits) -> Result<(), HistoryContractError> {
        require_version(self.version)?;
        require_request_key(self.request_key)?;
        require_item_limit("limit", self.limit, limits)?;
        require_target_bytes(self.target_bytes, limits)
    }
}

/// `kind:"initial"`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HistoryInitialKind {
    Initial,
}

/// 首屏结果；与标准 ACP 配置字段并列放在 `_meta`。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryInitialResult {
    pub version: u32,
    pub kind: HistoryInitialKind,
    pub request_key: DecimalU64,
    pub execution_ready: bool,
    pub runtime_generation: String,
    pub delivery_id: String,
    pub live_boundary: HistoryLiveBoundary,
    pub page: HistoryWindowPage,
}

/// 首屏交付边界：此后只应用 `afterSeq` 之后的实时事件。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryLiveBoundary {
    pub stream_id: String,
    pub after_seq: DecimalU64,
}

impl HistoryInitialResult {
    /// 校验结果版本与页面/交付身份的下界。
    pub fn validate(&self) -> Result<(), HistoryContractError> {
        if self.version != HISTORY_V2_VERSION {
            return Err(HistoryContractError::UnsupportedVersion);
        }
        if self.runtime_generation.is_empty() {
            return Err(HistoryContractError::shape(
                "initial result requires runtimeGeneration",
            ));
        }
        if self.delivery_id.is_empty() {
            return Err(HistoryContractError::shape(
                "initial result requires deliveryId",
            ));
        }
        if self.live_boundary.stream_id.is_empty() {
            return Err(HistoryContractError::shape(
                "initial result requires liveBoundary.streamId",
            ));
        }
        Ok(())
    }
}
