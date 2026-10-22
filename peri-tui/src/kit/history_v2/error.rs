//! History V2 wire 失败的客户端分类（设计 §12.1、§12.2）。
//!
//! 服务端历史领域失败统一 `-32000` + `data.domain/reason/recovery`。这里只保留
//! 可判定恢复动作的字段，日志与展示不得复用 wire 原文中的票据/cursor/正文。

use peri_acp::transport::types::AcpError;
use peri_acp_types::history_v2::{HistoryErrorData, HistoryErrorReason, HistoryRecovery};

/// 一次失败响应。缺少领域数据时按普通失败处理，不猜测服务端状态。
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryV2WireFailure {
    pub code: i64,
    pub message: String,
    pub reason: Option<HistoryErrorReason>,
    pub recovery: Option<HistoryRecovery>,
}

impl HistoryV2WireFailure {
    pub fn from_error(error: &AcpError) -> Self {
        let data = error
            .data
            .as_ref()
            .and_then(|value| serde_json::from_value::<HistoryErrorData>(value.clone()).ok());
        Self {
            code: error.code,
            message: error.message.clone(),
            reason: data.as_ref().map(|data| data.reason),
            recovery: data.map(|data| data.recovery),
        }
    }

    /// 仅表达类别；不把 message 写进日志（错误文本来自传输层，可能含实现细节）。
    pub fn label(&self) -> &'static str {
        match self.reason {
            Some(reason) => reason_label(reason),
            None => "history_error",
        }
    }

    /// snapshot/window/ticket 已失效：当前窗口不能继续请求，必须重新 open。
    pub fn requires_reopen(&self) -> bool {
        matches!(
            self.reason,
            Some(
                HistoryErrorReason::SnapshotInvalidated
                    | HistoryErrorReason::WindowExpired
                    | HistoryErrorReason::WindowClosed
                    | HistoryErrorReason::TicketInvalid
            )
        ) || self.recovery == Some(HistoryRecovery::Reopen)
    }

    /// 原操作仍在服务端执行：不得重复分配请求/预留，等待原结果。
    pub fn operation_in_progress(&self) -> bool {
        self.reason == Some(HistoryErrorReason::OperationInProgress)
    }

    /// 请求本身可安全丢弃：页面保持不变，换新 key 后可再试。
    pub fn discard_only(&self) -> bool {
        matches!(
            self.reason,
            Some(
                HistoryErrorReason::InvalidCursor
                    | HistoryErrorReason::CrossWindowCursor
                    | HistoryErrorReason::AnchorUnavailable
                    | HistoryErrorReason::RequestConflict
                    | HistoryErrorReason::OperationExpired
            )
        )
    }

    /// 协议/协商失败：不能静默全量回放。
    pub fn negotiation_failure(&self) -> bool {
        matches!(
            self.reason,
            Some(
                HistoryErrorReason::ProtocolNotNegotiated | HistoryErrorReason::UnsupportedVersion
            )
        )
    }
}

fn reason_label(reason: HistoryErrorReason) -> &'static str {
    match reason {
        HistoryErrorReason::ProtocolNotNegotiated => "protocol_not_negotiated",
        HistoryErrorReason::UnsupportedVersion => "unsupported_version",
        HistoryErrorReason::InvalidCursor => "invalid_cursor",
        HistoryErrorReason::CrossWindowCursor => "cross_window_cursor",
        HistoryErrorReason::SnapshotInvalidated => "snapshot_invalidated",
        HistoryErrorReason::WindowExpired => "window_expired",
        HistoryErrorReason::WindowClosed => "window_closed",
        HistoryErrorReason::TicketInvalid => "ticket_invalid",
        HistoryErrorReason::AnchorUnavailable => "anchor_unavailable",
        HistoryErrorReason::PermissionDenied => "permission_denied",
        HistoryErrorReason::NotFound => "not_found",
        HistoryErrorReason::IndexNotReady => "index_not_ready",
        HistoryErrorReason::SourceUnavailable => "source_unavailable",
        HistoryErrorReason::CapacityExceeded => "capacity_exceeded",
        HistoryErrorReason::BudgetExceeded => "budget_exceeded",
        HistoryErrorReason::BackendUnavailable => "backend_unavailable",
        HistoryErrorReason::Timeout => "timeout",
        HistoryErrorReason::RequestConflict => "request_conflict",
        HistoryErrorReason::OperationExpired => "operation_expired",
        HistoryErrorReason::OperationInProgress => "operation_in_progress",
        HistoryErrorReason::DeliveryExpired => "delivery_expired",
        HistoryErrorReason::DeliveryGap => "delivery_gap",
        HistoryErrorReason::Busy => "busy",
        HistoryErrorReason::InvalidRequest => "invalid_request",
    }
}

#[cfg(test)]
#[path = "error_test.rs"]
mod error_test;
