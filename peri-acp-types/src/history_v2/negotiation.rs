//! History V2 协商（设计 §4）：`_meta["peri.history"]` 的请求/回显与预算。
//!
//! 协商是连接级的：只有服务端明确回显 `version=2` 且 `selectedProfile="windowed"`，
//! 客户端才可以发送 initial 与 `_peri/history`。请求了 `peri.history` 但版本/模式不受
//! 支持时必须显式失败，不能回落到标准全量回放。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::error::HistoryContractError;
use super::{
    HISTORY_CHANGED_NOTIFICATION, HISTORY_MAX_BODY_CHUNK_BYTES, HISTORY_MAX_ITEMS,
    HISTORY_MAX_TARGET_PAGE_BYTES, HISTORY_META_KEY, HISTORY_METHOD, HISTORY_MIN_TARGET_PAGE_BYTES,
    HISTORY_PROFILE_WINDOWED, HISTORY_V2_VERSION,
};

/// 默认页条目数。
pub const HISTORY_DEFAULT_ITEMS: u32 = 50;
/// 默认目标页字节。
pub const HISTORY_DEFAULT_TARGET_PAGE_BYTES: u64 = 512 * 1024;
/// 默认完整响应字节上限。
pub const HISTORY_DEFAULT_MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
/// 默认正文字块字节。
pub const HISTORY_DEFAULT_BODY_CHUNK_BYTES: u32 = 64 * 1024;
/// 默认连接窗口数上限。
pub const HISTORY_DEFAULT_MAX_WINDOWS: u32 = 8;

/// 服务端在 `agentCapabilities._meta["peri.history"].limits` 中公布的预算。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryLimits {
    pub default_items: u32,
    pub max_items: u32,
    pub target_page_bytes: u64,
    pub max_response_bytes: u64,
    pub default_body_chunk_bytes: u32,
    pub max_body_chunk_bytes: u32,
    pub max_windows_per_connection: u32,
}

impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            default_items: HISTORY_DEFAULT_ITEMS,
            max_items: HISTORY_MAX_ITEMS,
            target_page_bytes: HISTORY_DEFAULT_TARGET_PAGE_BYTES,
            max_response_bytes: HISTORY_DEFAULT_MAX_RESPONSE_BYTES,
            default_body_chunk_bytes: HISTORY_DEFAULT_BODY_CHUNK_BYTES,
            max_body_chunk_bytes: HISTORY_MAX_BODY_CHUNK_BYTES,
            max_windows_per_connection: HISTORY_DEFAULT_MAX_WINDOWS,
        }
    }
}

impl HistoryLimits {
    /// 校验各预算的闭合范围；不合法时不得参与协商。
    pub fn validate(&self) -> Result<(), HistoryContractError> {
        if self.default_items == 0 || self.default_items > self.max_items {
            return Err(HistoryContractError::limits(
                "defaultItems must be within 1..=maxItems",
            ));
        }
        if self.max_items == 0 || self.max_items > HISTORY_MAX_ITEMS {
            return Err(HistoryContractError::limits(format!(
                "maxItems must be within 1..={HISTORY_MAX_ITEMS}"
            )));
        }
        if !(HISTORY_MIN_TARGET_PAGE_BYTES..=HISTORY_MAX_TARGET_PAGE_BYTES)
            .contains(&self.target_page_bytes)
        {
            return Err(HistoryContractError::limits(format!(
                "targetPageBytes must be within {HISTORY_MIN_TARGET_PAGE_BYTES}..={HISTORY_MAX_TARGET_PAGE_BYTES}"
            )));
        }
        if self.max_response_bytes < self.target_page_bytes {
            return Err(HistoryContractError::limits(
                "maxResponseBytes must not be smaller than targetPageBytes",
            ));
        }
        if self.default_body_chunk_bytes == 0
            || self.default_body_chunk_bytes > self.max_body_chunk_bytes
        {
            return Err(HistoryContractError::limits(
                "defaultBodyChunkBytes must be within 1..=maxBodyChunkBytes",
            ));
        }
        if self.max_body_chunk_bytes == 0
            || self.max_body_chunk_bytes > HISTORY_MAX_BODY_CHUNK_BYTES
        {
            return Err(HistoryContractError::limits(format!(
                "maxBodyChunkBytes must be within 1..={HISTORY_MAX_BODY_CHUNK_BYTES}"
            )));
        }
        if self.max_windows_per_connection == 0 {
            return Err(HistoryContractError::limits(
                "maxWindowsPerConnection must be at least 1",
            ));
        }
        Ok(())
    }
}

/// 客户端在 `initialize` 的 `clientCapabilities._meta["peri.history"]` 中声明。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryClientCapability {
    pub versions: Vec<u32>,
    pub requested_profile: String,
}

impl HistoryClientCapability {
    /// 从 capability map 解析；key 缺失返回 `Ok(None)`，存在但形状错误显式失败。
    pub fn parse_from_meta(
        meta: &Map<String, Value>,
    ) -> Result<Option<Self>, HistoryContractError> {
        let Some(value) = meta.get(HISTORY_META_KEY) else {
            return Ok(None);
        };
        serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|_| {
                HistoryContractError::invalid_request(
                    "clientCapabilities._meta[\"peri.history\"] has an invalid shape",
                )
            })
    }

    /// 只接受 V2 + windowed。成功时返回服务端回显。
    pub fn select(
        &self,
        limits: &HistoryLimits,
    ) -> Result<HistoryServerCapability, HistoryContractError> {
        if !self.versions.contains(&HISTORY_V2_VERSION) {
            return Err(HistoryContractError::UnsupportedVersion);
        }
        if self.requested_profile != HISTORY_PROFILE_WINDOWED {
            return Err(HistoryContractError::UnsupportedVersion);
        }
        limits.validate()?;
        Ok(HistoryServerCapability {
            version: HISTORY_V2_VERSION,
            selected_profile: HISTORY_PROFILE_WINDOWED.to_owned(),
            method: HISTORY_METHOD.to_owned(),
            notification: HISTORY_CHANGED_NOTIFICATION.to_owned(),
            limits: limits.clone(),
        })
    }
}

/// 服务端在 `agentCapabilities._meta["peri.history"]` 中回显。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryServerCapability {
    pub version: u32,
    pub selected_profile: String,
    pub method: String,
    pub notification: String,
    pub limits: HistoryLimits,
}

impl HistoryServerCapability {
    /// 写入 `_meta["peri.history"]`。
    pub fn to_meta(&self) -> Result<Map<String, Value>, HistoryContractError> {
        let value = serde_json::to_value(self).map_err(|_| {
            HistoryContractError::shape("server history capability could not be serialized")
        })?;
        let mut meta = Map::new();
        meta.insert(HISTORY_META_KEY.to_owned(), value);
        Ok(meta)
    }

    /// 从 capability map 解析；key 缺失返回 `Ok(None)`。
    ///
    /// 版本、模式、方法名或通知名不匹配都显式失败，避免把半截能力当成 V2。
    pub fn parse_from_meta(
        meta: &Map<String, Value>,
    ) -> Result<Option<Self>, HistoryContractError> {
        let Some(value) = meta.get(HISTORY_META_KEY) else {
            return Ok(None);
        };
        let capability: Self = serde_json::from_value(value.clone()).map_err(|_| {
            HistoryContractError::invalid_request(
                "agentCapabilities._meta[\"peri.history\"] has an invalid shape",
            )
        })?;
        if capability.version != HISTORY_V2_VERSION
            || capability.selected_profile != HISTORY_PROFILE_WINDOWED
        {
            return Err(HistoryContractError::UnsupportedVersion);
        }
        if capability.method != HISTORY_METHOD
            || capability.notification != HISTORY_CHANGED_NOTIFICATION
        {
            return Err(HistoryContractError::invalid_request(
                "server history capability names do not match the V2 contract",
            ));
        }
        capability.limits.validate()?;
        Ok(Some(capability))
    }
}
