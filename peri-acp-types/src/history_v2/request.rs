//! History V2 请求契约（设计 §6.1、§7）。
//!
//! 根对象以 `action` 判别（V1 使用 `kind`）。[`HistoryRequest::decode`] 先取出并移除
//! `action`，再按变体结构反序列化；每个变体都启用 `deny_unknown_fields`，未知 action、
//! 未知字段与旧顶层私有字段都显式失败，不会降级为初始 load。

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::error::HistoryContractError;
use super::integer::DecimalU64;
use super::negotiation::HistoryLimits;
use super::{
    require_around_items, require_identity, require_item_limit, require_latest_items,
    require_request_key, require_target_bytes, require_token, require_version,
};

/// 分页方向。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryDirection {
    Before,
    After,
}

/// 继承来源必须绑定冻结快照与直接祖先会话。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryInheritedSource {
    pub snapshot_id: String,
    pub ancestor_session_id: String,
}

/// 条目来源。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum HistorySource {
    Own,
    Inherited(HistoryInheritedSource),
}

impl HistorySource {
    /// inherited 的两个身份都必须存在且有界。
    pub fn validate(&self) -> Result<(), HistoryContractError> {
        if let Self::Inherited(source) = self {
            require_identity("snapshotId", &source.snapshot_id)?;
            require_identity("ancestorSessionId", &source.ancestor_session_id)?;
        }
        Ok(())
    }
}

/// `entry` 锚点字段（wire 上由 `kind:"entry"` 标记）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryEntryAnchor {
    pub entry_id: String,
    pub source: HistorySource,
}

impl HistoryEntryAnchor {
    /// 校验稳定条目身份与来源。
    pub fn validate(&self) -> Result<(), HistoryContractError> {
        require_identity("entryId", &self.entry_id)?;
        self.source.validate()
    }
}

/// `open`/`seek` 锚点。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum HistoryAnchor {
    Latest,
    Entry(HistoryEntryAnchor),
}

/// `_peri/history` 请求判别联合。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryRequest {
    Open(HistoryOpenRequest),
    Page(HistoryPageRequest),
    Seek(HistorySeekRequest),
    Body(HistoryBodyRequest),
    Close(HistoryCloseRequest),
}

impl HistoryRequest {
    /// 解码严格 V2 请求。`action` 不进入变体结构。
    pub fn decode(value: &Value) -> Result<Self, HistoryContractError> {
        let object = value.as_object().ok_or_else(|| {
            HistoryContractError::invalid_request("history request must be an object")
        })?;
        let action = object
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                HistoryContractError::invalid_request("history request.action is required")
            })?;
        let mut fields = object.clone();
        fields.remove("action");
        let fields = Value::Object(fields);
        match action {
            "open" => decode_fields("open", fields).map(Self::Open),
            "page" => decode_fields("page", fields).map(Self::Page),
            "seek" => decode_fields("seek", fields).map(Self::Seek),
            "body" => decode_fields("body", fields).map(Self::Body),
            "close" => decode_fields("close", fields).map(Self::Close),
            _ => Err(HistoryContractError::invalid_request(
                "unknown history request.action",
            )),
        }
    }

    /// wire 上的 action 名称。
    pub fn action(&self) -> &'static str {
        match self {
            Self::Open(_) => "open",
            Self::Page(_) => "page",
            Self::Seek(_) => "seek",
            Self::Body(_) => "body",
            Self::Close(_) => "close",
        }
    }

    /// 按协商预算校验请求。
    pub fn validate(&self, limits: &HistoryLimits) -> Result<(), HistoryContractError> {
        match self {
            Self::Open(request) => request.validate(limits),
            Self::Page(request) => request.validate(limits),
            Self::Seek(request) => request.validate(limits),
            Self::Body(request) => request.validate(limits),
            Self::Close(request) => request.validate(limits),
        }
    }
}

fn decode_fields<T: DeserializeOwned>(
    action: &'static str,
    value: Value,
) -> Result<T, HistoryContractError> {
    serde_json::from_value(value).map_err(|error| {
        // 错误文本可能回显 readTicket/cursor；诊断只记录位置与 action。
        tracing::debug!(
            action,
            line = error.line(),
            column = error.column(),
            "history v2 request rejected"
        );
        HistoryContractError::invalid_request(format!("history {action} fields are invalid"))
    })
}

fn validate_envelope(
    version: u32,
    request_key: DecimalU64,
    session_id: &str,
) -> Result<(), HistoryContractError> {
    require_version(version)?;
    require_request_key(request_key)?;
    require_identity("sessionId", session_id)
}

fn validate_window(
    session_id: &str,
    window_id: &str,
    snapshot_token: &str,
    read_ticket: &str,
) -> Result<(), HistoryContractError> {
    require_identity("sessionId", session_id)?;
    require_identity("windowId", window_id)?;
    require_token("snapshotToken", snapshot_token)?;
    require_token("readTicket", read_ticket)
}

/// `open`：创建独立浏览快照。
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryOpenRequest {
    pub version: u32,
    pub session_id: String,
    pub request_key: DecimalU64,
    pub anchor: HistoryAnchor,
    pub before: u32,
    pub after: u32,
    pub target_bytes: u64,
}

impl HistoryOpenRequest {
    /// 校验版本、身份、锚点方向与预算。
    pub fn validate(&self, limits: &HistoryLimits) -> Result<(), HistoryContractError> {
        validate_envelope(self.version, self.request_key, &self.session_id)?;
        require_target_bytes(self.target_bytes, limits)?;
        match &self.anchor {
            HistoryAnchor::Latest => {
                if self.after != 0 {
                    return Err(HistoryContractError::invalid_request(
                        "open latest requires after = 0",
                    ));
                }
                require_latest_items("before", self.before, limits)
            }
            HistoryAnchor::Entry(anchor) => {
                anchor.validate()?;
                require_around_items("open", self.before, self.after, limits)
            }
        }
    }
}

/// `page`：沿游标读取固定快照。
///
/// `cursor` 缺省表示从窗口该方向的 home 边界开始；服务层会选择 home cursor，
/// 没有 home 时返回 `invalid_cursor`。
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryPageRequest {
    pub version: u32,
    pub session_id: String,
    pub request_key: DecimalU64,
    pub window_id: String,
    pub snapshot_token: String,
    pub read_ticket: String,
    /// `None` 表示从窗口该方向的 home 边界开始读取；服务层据此选择 home cursor。
    #[serde(default)]
    pub cursor: Option<String>,
    pub direction: HistoryDirection,
    pub limit: u32,
}

impl HistoryPageRequest {
    pub fn validate(&self, limits: &HistoryLimits) -> Result<(), HistoryContractError> {
        require_version(self.version)?;
        require_request_key(self.request_key)?;
        validate_window(
            &self.session_id,
            &self.window_id,
            &self.snapshot_token,
            &self.read_ticket,
        )?;
        if let Some(cursor) = &self.cursor {
            require_token("cursor", cursor)?;
        }
        require_item_limit("limit", self.limit, limits)
    }
}

/// `seek`：在固定快照内按稳定条目身份定位。
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistorySeekRequest {
    pub version: u32,
    pub session_id: String,
    pub request_key: DecimalU64,
    pub window_id: String,
    pub snapshot_token: String,
    pub read_ticket: String,
    pub anchor: HistoryAnchor,
    pub before: u32,
    pub after: u32,
}

impl HistorySeekRequest {
    pub fn validate(&self, limits: &HistoryLimits) -> Result<(), HistoryContractError> {
        require_version(self.version)?;
        require_request_key(self.request_key)?;
        validate_window(
            &self.session_id,
            &self.window_id,
            &self.snapshot_token,
            &self.read_ticket,
        )?;
        let HistoryAnchor::Entry(anchor) = &self.anchor else {
            return Err(HistoryContractError::invalid_request(
                "seek anchor must be an entry",
            ));
        };
        anchor.validate()?;
        require_around_items("seek", self.before, self.after, limits)
    }
}

/// `body`：读取已交付描述符的有界正文块。
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryBodyRequest {
    pub version: u32,
    pub session_id: String,
    pub request_key: DecimalU64,
    pub window_id: String,
    pub snapshot_token: String,
    pub read_ticket: String,
    pub body_ref: String,
    pub offset: DecimalU64,
    pub max_bytes: u64,
}

impl HistoryBodyRequest {
    pub fn validate(&self, limits: &HistoryLimits) -> Result<(), HistoryContractError> {
        require_version(self.version)?;
        require_request_key(self.request_key)?;
        validate_window(
            &self.session_id,
            &self.window_id,
            &self.snapshot_token,
            &self.read_ticket,
        )?;
        require_token("bodyRef", &self.body_ref)?;
        let max = limits.max_body_chunk_bytes.max(1);
        if self.max_bytes == 0 || self.max_bytes > u64::from(max) {
            return Err(HistoryContractError::limits(format!(
                "maxBytes must be within 1..={max}"
            )));
        }
        Ok(())
    }
}

/// `close`：只关闭指定窗口，幂等。
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryCloseRequest {
    pub version: u32,
    pub session_id: String,
    pub request_key: DecimalU64,
    pub window_id: String,
    pub snapshot_token: String,
    pub read_ticket: String,
}

impl HistoryCloseRequest {
    pub fn validate(&self, _limits: &HistoryLimits) -> Result<(), HistoryContractError> {
        require_version(self.version)?;
        require_request_key(self.request_key)?;
        validate_window(
            &self.session_id,
            &self.window_id,
            &self.snapshot_token,
            &self.read_ticket,
        )
    }
}
