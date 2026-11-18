//! `_peri/history` 消费者请求与 params 编码。

use peri_acp_types::history_v2::{
    DecimalU64, HISTORY_V2_VERSION, HistoryEntryAnchor, HistoryLimits,
};
use serde_json::{Value, json};

use super::super::request::{HistoryV2PageRequest, HistoryV2RequestIdentity, HistoryV2SeekRequest};

/// 消费者队列里的请求；`Open` 只用于失败恢复后的重建与 V2 新会话首窗。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryV2ConsumerRequest {
    Open {
        session_id: String,
        request_key: DecimalU64,
        anchor: Option<HistoryEntryAnchor>,
    },
    Page(HistoryV2PageRequest),
    Seek(HistoryV2SeekRequest),
    /// 窗口拆除（best-effort）：只发送关闭事实，不修改本地状态。
    Close {
        session_id: String,
        request_key: DecimalU64,
        window_id: String,
        snapshot_token: String,
        read_ticket: String,
    },
}

impl HistoryV2ConsumerRequest {
    pub(crate) fn session_id(&self) -> &str {
        match self {
            Self::Open { session_id, .. } => session_id,
            Self::Close { session_id, .. } => session_id,
            Self::Page(request) => &request.context.session_id,
            Self::Seek(request) => &request.context.session_id,
        }
    }

    pub(crate) fn request_key(&self) -> DecimalU64 {
        match self {
            Self::Open { request_key, .. } => *request_key,
            Self::Close { request_key, .. } => *request_key,
            Self::Page(request) => request.context.request_key,
            Self::Seek(request) => request.context.request_key,
        }
    }

    /// 失败结算身份：session + window（open/close 为 None）+ requestKey。
    pub(super) fn identity(&self) -> HistoryV2RequestIdentity<'_> {
        match self {
            Self::Open { .. } | Self::Close { .. } => HistoryV2RequestIdentity {
                session_id: self.session_id(),
                window_id: None,
                request_key: self.request_key(),
            },
            Self::Page(request) => request.context.identity(),
            Self::Seek(request) => request.context.identity(),
        }
    }

    /// `_peri/history` 的完整 params。open 不带 window/ticket，其余带。
    pub fn params(&self, limits: &HistoryLimits) -> Value {
        match self {
            Self::Open {
                session_id,
                request_key,
                anchor,
            } => {
                let anchor = match anchor {
                    None => json!({"kind": "latest"}),
                    Some(anchor) => json!({
                        "kind": "entry",
                        "entryId": anchor.entry_id,
                        "source": source_value(&anchor.source),
                    }),
                };
                json!({
                    "version": HISTORY_V2_VERSION,
                    "action": "open",
                    "sessionId": session_id,
                    "requestKey": request_key,
                    "anchor": anchor,
                    "before": limits.default_items,
                    "after": 0,
                    "targetBytes": limits.target_page_bytes,
                })
            }
            Self::Close {
                session_id,
                request_key,
                window_id,
                snapshot_token,
                read_ticket,
            } => json!({
                "version": HISTORY_V2_VERSION,
                "action": "close",
                "sessionId": session_id,
                "requestKey": request_key,
                "windowId": window_id,
                "snapshotToken": snapshot_token,
                "readTicket": read_ticket,
            }),
            Self::Page(request) => request.params(),
            Self::Seek(request) => request.params(),
        }
    }
}

fn source_value(source: &peri_acp_types::history_v2::HistorySource) -> Value {
    match source {
        peri_acp_types::history_v2::HistorySource::Own => json!({"kind": "own"}),
        peri_acp_types::history_v2::HistorySource::Inherited(origin) => json!({
            "kind": "inherited",
            "snapshotId": origin.snapshot_id,
            "ancestorSessionId": origin.ancestor_session_id,
        }),
    }
}
