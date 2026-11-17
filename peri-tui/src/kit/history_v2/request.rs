//! History V2 只读请求的本地身份与 wire 参数（设计 §7.2–§7.4）。
//!
//! reducer 不持有 transport：这里生成带 `requestKey` 的完整参数与本地上下文，H5c
//! 负责发送并把同一上下文交回 `settle_*`。响应解码只接受严格 V2 形状，错误文本
//! 不包含 token/cursor/bodyRef/正文。

use peri_acp_types::history_v2::{
    DecimalU64, HISTORY_V2_VERSION, HistoryBodyChunk, HistoryDirection, HistoryEntryAnchor,
    HistorySource, HistoryWindowPage,
};
use serde_json::{Value, json};

/// 一次请求的本地关联身份；服务端响应不回传 requestKey，由调用方原样带回。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryV2RequestContext {
    pub request_key: DecimalU64,
    pub session_id: String,
    pub window_id: String,
    pub snapshot_token: String,
    pub read_ticket: String,
    /// 发出请求时的导航代次；seek 会推进它。
    pub view_epoch: u64,
}

/// 失败结算的关联身份：只允许作用于发出它的会话/窗口/请求。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HistoryV2RequestIdentity<'a> {
    pub(crate) session_id: &'a str,
    /// open 没有 windowId；page/seek 必须与当前窗口一致。
    pub(crate) window_id: Option<&'a str>,
    pub(crate) request_key: DecimalU64,
}

impl HistoryV2RequestContext {
    pub(crate) fn identity(&self) -> HistoryV2RequestIdentity<'_> {
        HistoryV2RequestIdentity {
            session_id: &self.session_id,
            window_id: Some(&self.window_id),
            request_key: self.request_key,
        }
    }
}

/// `page` 请求：游标只来自服务端返回的窗口边界。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryV2PageRequest {
    pub context: HistoryV2RequestContext,
    pub direction: HistoryDirection,
    /// 请求发出时窗口该方向的边界游标。
    pub cursor: String,
    pub limit: u32,
    pub reserved_bytes: usize,
}

impl HistoryV2PageRequest {
    /// `_peri/history` 的 params 对象。
    pub fn params(&self) -> Value {
        json!({
            "version": HISTORY_V2_VERSION,
            "action": "page",
            "sessionId": self.context.session_id,
            "requestKey": self.context.request_key,
            "windowId": self.context.window_id,
            "snapshotToken": self.context.snapshot_token,
            "readTicket": self.context.read_ticket,
            "cursor": self.cursor,
            "direction": direction_value(self.direction),
            "limit": self.limit,
        })
    }
}

/// `seek`：只在用户跳转时发出，锚点必须是稳定条目身份。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryV2SeekRequest {
    pub context: HistoryV2RequestContext,
    pub anchor: HistoryEntryAnchor,
    pub before: u32,
    pub after: u32,
    pub reserved_bytes: usize,
}

impl HistoryV2SeekRequest {
    pub fn params(&self) -> Value {
        json!({
            "version": HISTORY_V2_VERSION,
            "action": "seek",
            "sessionId": self.context.session_id,
            "requestKey": self.context.request_key,
            "windowId": self.context.window_id,
            "snapshotToken": self.context.snapshot_token,
            "readTicket": self.context.read_ticket,
            "anchor": {
                "kind": "entry",
                "entryId": self.anchor.entry_id,
                "source": source_value(&self.anchor.source),
            },
            "before": self.before,
            "after": self.after,
        })
    }
}

/// `body`：按已交付 descriptor 读取下一块。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryV2BodyRequest {
    pub context: HistoryV2RequestContext,
    /// reducer 内部的稳定正文身份；不上 wire。
    pub body_key: String,
    pub body_ref: String,
    pub offset: DecimalU64,
    pub max_bytes: u32,
}

impl HistoryV2BodyRequest {
    pub fn params(&self) -> Value {
        json!({
            "version": HISTORY_V2_VERSION,
            "action": "body",
            "sessionId": self.context.session_id,
            "requestKey": self.context.request_key,
            "windowId": self.context.window_id,
            "snapshotToken": self.context.snapshot_token,
            "readTicket": self.context.read_ticket,
            "bodyRef": self.body_ref,
            "offset": self.offset,
            "maxBytes": self.max_bytes,
        })
    }
}

/// 响应解码失败；只保留类别，避免把 wire 原文写入日志。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryV2DecodeError {
    Page,
    Body,
}

impl std::fmt::Display for HistoryV2DecodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Page => "history page result has an invalid shape",
            Self::Body => "history body result has an invalid shape",
        })
    }
}

/// 严格解码 `_peri/history page|seek` 的 result。
pub fn parse_page_result(value: &Value) -> Result<HistoryWindowPage, HistoryV2DecodeError> {
    serde_json::from_value(value.clone()).map_err(|_| HistoryV2DecodeError::Page)
}

/// 严格解码 `_peri/history body` 的 result。
pub fn parse_body_result(value: &Value) -> Result<HistoryBodyChunk, HistoryV2DecodeError> {
    serde_json::from_value(value.clone()).map_err(|_| HistoryV2DecodeError::Body)
}

fn direction_value(direction: HistoryDirection) -> &'static str {
    match direction {
        HistoryDirection::Before => "before",
        HistoryDirection::After => "after",
    }
}

fn source_value(source: &HistorySource) -> Value {
    match source {
        HistorySource::Own => json!({"kind": "own"}),
        HistorySource::Inherited(origin) => json!({
            "kind": "inherited",
            "snapshotId": origin.snapshot_id,
            "ancestorSessionId": origin.ancestor_session_id,
        }),
    }
}
