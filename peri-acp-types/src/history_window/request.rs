//! `peri.historyWindowV1` 的 open/page/seek 请求 schema 与纯校验。
//!
//! `kind` 先被 [`HistoryWindowRequest::decode`] 取出，其余字段按各自 `deny_unknown_fields`
//! 的结构严格反序列化：未知字段、未知 `kind`、互斥字段冲突都显式失败，绝不降级成
//! 「没有 history」的初始 load。

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::{
    HistoryWindowAnchor, HistoryWindowDirection, HistoryWindowError,
    HISTORY_WINDOW_DEFAULT_BYTE_BUDGET, HISTORY_WINDOW_DEFAULT_LIMIT,
    HISTORY_WINDOW_MAX_BYTE_BUDGET, HISTORY_WINDOW_MAX_LIMIT,
};

/// 稳定条目身份在 `seek` 请求里的锚点（wire 形状没有 `kind`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryWindowSeekAnchor {
    pub entry_id: String,
    pub source: super::HistorySource,
}

/// `session/load` 内 `history` 的判别联合。
#[derive(Clone, Debug)]
pub enum HistoryWindowRequest {
    Open(HistoryWindowOpenRequest),
    Page(HistoryWindowPageRequest),
    Seek(HistoryWindowSeekRequest),
}

impl HistoryWindowRequest {
    pub fn decode(value: &serde_json::Value) -> Result<Self, HistoryWindowError> {
        let object = value
            .as_object()
            .ok_or_else(|| HistoryWindowError::invalid_request("history must be an object"))?;
        let kind = object
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| HistoryWindowError::invalid_request("history.kind is required"))?;
        let mut fields = object.clone();
        fields.remove("kind");
        let fields = serde_json::Value::Object(fields);
        match kind {
            "open" => decode_fields("open", fields).map(Self::Open),
            "page" => decode_fields("page", fields).map(Self::Page),
            "seek" => decode_fields("seek", fields).map(Self::Seek),
            _ => Err(HistoryWindowError::invalid_request("unknown history.kind")),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Open(_) => "open",
            Self::Page(_) => "page",
            Self::Seek(_) => "seek",
        }
    }
}

fn decode_fields<T: DeserializeOwned>(
    kind: &'static str,
    value: serde_json::Value,
) -> Result<T, HistoryWindowError> {
    serde_json::from_value(value).map_err(|error| {
        // 错误文本可能回显字段值（包括 readTicket）；诊断只记录位置与 kind。
        tracing::debug!(
            kind,
            line = error.line(),
            column = error.column(),
            "history window request rejected"
        );
        HistoryWindowError::invalid_request(format!("history {kind} fields are invalid"))
    })
}

/// `open` 请求。`limit` 是双向缺省；显式 `beforeLimit`/`afterLimit` 优先。
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryWindowOpenRequest {
    #[serde(default)]
    pub anchor: Option<HistoryWindowAnchor>,
    pub direction: HistoryWindowDirection,
    #[serde(default)]
    pub limit: Option<u16>,
    #[serde(default)]
    pub before_limit: Option<u16>,
    #[serde(default)]
    pub after_limit: Option<u16>,
    #[serde(default)]
    pub byte_budget: Option<u32>,
}

/// `open` 的校验后计划。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryWindowOpenPlan {
    pub anchor: HistoryWindowAnchor,
    pub direction: HistoryWindowDirection,
    pub before_limit: usize,
    pub after_limit: usize,
    pub byte_budget: usize,
}

impl HistoryWindowOpenRequest {
    pub fn validate(self) -> Result<HistoryWindowOpenPlan, HistoryWindowError> {
        let limit = validate_limit(self.limit.unwrap_or(HISTORY_WINDOW_DEFAULT_LIMIT))?;
        let before = self
            .before_limit
            .map(validate_limit)
            .transpose()?
            .unwrap_or(limit);
        let after = self
            .after_limit
            .map(validate_limit)
            .transpose()?
            .unwrap_or(limit);
        let byte_budget = validate_byte_budget(
            self.byte_budget
                .unwrap_or(HISTORY_WINDOW_DEFAULT_BYTE_BUDGET),
        )?;
        let (before_limit, after_limit) = match self.direction {
            HistoryWindowDirection::Around => (before, after),
            HistoryWindowDirection::Before => (before, 0),
            HistoryWindowDirection::After => (0, after),
        };
        Ok(HistoryWindowOpenPlan {
            anchor: self.anchor.unwrap_or_default(),
            direction: self.direction,
            before_limit,
            after_limit,
            byte_budget,
        })
    }
}

/// `page` 请求。`beforeCursor` 与 `afterCursor` 必须恰好一个非空。
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryWindowPageRequest {
    pub snapshot_token: String,
    pub read_ticket: String,
    #[serde(default)]
    pub before_cursor: Option<String>,
    #[serde(default)]
    pub after_cursor: Option<String>,
    pub limit: u16,
}

impl HistoryWindowPageRequest {
    /// 返回读取方向（`Before` 或 `After`）。
    pub fn validate(&self) -> Result<HistoryWindowDirection, HistoryWindowError> {
        validate_token("snapshotToken", &self.snapshot_token)?;
        validate_token("readTicket", &self.read_ticket)?;
        validate_limit(self.limit)?;
        match (
            non_empty(self.before_cursor.as_deref()),
            non_empty(self.after_cursor.as_deref()),
        ) {
            (Some(_), None) => Ok(HistoryWindowDirection::Before),
            (None, Some(_)) => Ok(HistoryWindowDirection::After),
            (Some(_), Some(_)) => Err(HistoryWindowError::invalid_request(
                "beforeCursor and afterCursor are mutually exclusive",
            )),
            (None, None) => Err(HistoryWindowError::invalid_request(
                "exactly one of beforeCursor/afterCursor is required",
            )),
        }
    }
}

/// `seek` 请求。
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryWindowSeekRequest {
    pub snapshot_token: String,
    pub read_ticket: String,
    pub anchor: HistoryWindowSeekAnchor,
    #[serde(default)]
    pub before_limit: Option<u16>,
    #[serde(default)]
    pub after_limit: Option<u16>,
}

impl HistoryWindowSeekRequest {
    /// 返回 `(before_limit, after_limit)`。
    pub fn validate(&self) -> Result<(usize, usize), HistoryWindowError> {
        validate_token("snapshotToken", &self.snapshot_token)?;
        validate_token("readTicket", &self.read_ticket)?;
        if self.anchor.entry_id.is_empty() {
            return Err(HistoryWindowError::invalid_request(
                "anchor.entryId is required",
            ));
        }
        let default = HISTORY_WINDOW_DEFAULT_LIMIT as usize;
        let before = self
            .before_limit
            .map(validate_limit)
            .transpose()?
            .unwrap_or(default);
        let after = self
            .after_limit
            .map(validate_limit)
            .transpose()?
            .unwrap_or(default);
        Ok((before, after))
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

fn validate_token(name: &str, value: &str) -> Result<(), HistoryWindowError> {
    if value.is_empty() {
        return Err(HistoryWindowError::invalid_request(format!(
            "{name} must not be empty"
        )));
    }
    Ok(())
}

fn validate_limit(limit: u16) -> Result<usize, HistoryWindowError> {
    if !(1..=HISTORY_WINDOW_MAX_LIMIT).contains(&limit) {
        return Err(HistoryWindowError::invalid_request(format!(
            "limit must be in 1..={HISTORY_WINDOW_MAX_LIMIT}"
        )));
    }
    Ok(limit as usize)
}

fn validate_byte_budget(budget: u32) -> Result<usize, HistoryWindowError> {
    if !(1..=HISTORY_WINDOW_MAX_BYTE_BUDGET).contains(&budget) {
        return Err(HistoryWindowError::invalid_request(format!(
            "byteBudget must be in 1..={HISTORY_WINDOW_MAX_BYTE_BUDGET}"
        )));
    }
    Ok(budget as usize)
}
