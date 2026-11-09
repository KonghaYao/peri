//! History V2 服务层的错误映射：领域 `-32000` + `HistoryErrorData`，格式错误 `-32602`。

use peri_acp_types::history_v2::{
    ExecutionEffect, HistoryBackendError, HistoryContractError, HistoryEffect, HistoryEffectState,
    HistoryErrorData, HistoryErrorReason, HistoryRecovery,
};

use crate::transport::types::AcpError;

/// 历史领域失败统一使用的 JSON-RPC server error code（设计 §12.1）。
pub(super) const HISTORY_DOMAIN_ERROR_CODE: i64 = -32000;
/// 格式/限额错误。
pub(super) const INVALID_PARAMS_CODE: i64 = -32602;

/// 服务层类型化失败；`message` 固定且不含票据、cursor 或正文。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ServiceFailure {
    pub(super) reason: HistoryErrorReason,
    pub(super) recovery: HistoryRecovery,
    pub(super) message: &'static str,
}

impl ServiceFailure {
    pub(super) fn new(
        reason: HistoryErrorReason,
        recovery: HistoryRecovery,
        message: &'static str,
    ) -> Self {
        Self {
            reason,
            recovery,
            message,
        }
    }

    pub(super) fn from_backend(error: HistoryBackendError) -> Self {
        Self::new(error.reason(), error.recovery(), error.message())
    }

    pub(super) fn request_conflict() -> Self {
        Self::new(
            HistoryErrorReason::RequestConflict,
            HistoryRecovery::None,
            "History request key was reused with different parameters",
        )
    }

    pub(super) fn operation_expired() -> Self {
        Self::new(
            HistoryErrorReason::OperationExpired,
            HistoryRecovery::None,
            "History operation is no longer retained",
        )
    }

    pub(super) fn operation_in_progress() -> Self {
        Self::new(
            HistoryErrorReason::OperationInProgress,
            HistoryRecovery::Retry,
            "History operation is still in progress",
        )
    }

    pub(super) fn delivery_expired() -> Self {
        Self::new(
            HistoryErrorReason::DeliveryExpired,
            HistoryRecovery::Retry,
            "History delivery is no longer available",
        )
    }

    pub(super) fn timeout() -> Self {
        Self::new(
            HistoryErrorReason::Timeout,
            HistoryRecovery::Retry,
            "History request timed out",
        )
    }

    pub(super) fn invalid_cursor() -> Self {
        Self::new(
            HistoryErrorReason::InvalidCursor,
            HistoryRecovery::None,
            "History cursor is invalid",
        )
    }

    pub(super) fn cross_window_cursor() -> Self {
        Self::new(
            HistoryErrorReason::CrossWindowCursor,
            HistoryRecovery::None,
            "History cursor belongs to another window",
        )
    }

    pub(super) fn snapshot_invalidated() -> Self {
        Self::new(
            HistoryErrorReason::SnapshotInvalidated,
            HistoryRecovery::Reopen,
            "History snapshot is no longer valid",
        )
    }

    pub(super) fn window_expired() -> Self {
        Self::new(
            HistoryErrorReason::WindowExpired,
            HistoryRecovery::Reopen,
            "History window expired",
        )
    }

    pub(super) fn window_closed() -> Self {
        Self::new(
            HistoryErrorReason::WindowClosed,
            HistoryRecovery::Reopen,
            "History window is closed",
        )
    }

    pub(super) fn ticket_invalid() -> Self {
        Self::new(
            HistoryErrorReason::TicketInvalid,
            HistoryRecovery::Reopen,
            "History read ticket is invalid",
        )
    }

    pub(super) fn permission_denied() -> Self {
        Self::new(
            HistoryErrorReason::PermissionDenied,
            HistoryRecovery::None,
            "History window does not belong to this session",
        )
    }

    pub(super) fn capacity_exceeded() -> Self {
        Self::new(
            HistoryErrorReason::CapacityExceeded,
            HistoryRecovery::Retry,
            "History window capacity was exceeded",
        )
    }

    /// 在途读配额饱和（FIX-04）：准入已完成但未取得执行槽位。
    ///
    /// 该失败可立即用同一 requestKey 重试；服务端不会排队等待，避免接收循环被
    /// 饱和的历史读取拖住。
    pub(super) fn busy() -> Self {
        Self::new(
            HistoryErrorReason::CapacityExceeded,
            HistoryRecovery::Retry,
            "History read capacity is saturated; retry the request",
        )
    }

    pub(super) fn budget_exceeded() -> Self {
        Self::new(
            HistoryErrorReason::BudgetExceeded,
            HistoryRecovery::ReduceScope,
            "History response exceeded its byte budget",
        )
    }

    pub(super) fn source_unavailable() -> Self {
        Self::new(
            HistoryErrorReason::SourceUnavailable,
            HistoryRecovery::Retry,
            "History source is unavailable",
        )
    }
}

pub(super) fn default_error(failure: ServiceFailure) -> AcpError {
    domain_error(failure, ExecutionEffect::Unchanged)
}

/// initial 失败：执行事实取自调用方显式传入的恢复状态；历史页未发布。
pub(crate) fn initial_error(
    failure: ServiceFailure,
    execution_ready: bool,
    runtime_generation: &str,
) -> AcpError {
    let effect = if execution_ready {
        HistoryEffect {
            execution: ExecutionEffect::Ready,
            history: HistoryEffectState::NotPublished,
            runtime_generation: Some(runtime_generation.to_owned()),
        }
    } else {
        HistoryEffect {
            execution: ExecutionEffect::NotReady,
            history: HistoryEffectState::NotPublished,
            runtime_generation: None,
        }
    };
    domain_error_with(failure, effect)
}

/// `session/load` initial 的恢复期限耗尽：执行/交付事实未改变，历史未发布。
pub(crate) fn initial_timeout_error(execution_ready: bool, runtime_generation: &str) -> AcpError {
    initial_error(
        ServiceFailure::timeout(),
        execution_ready,
        runtime_generation,
    )
}

pub(crate) fn protocol_not_negotiated_error() -> AcpError {
    domain_error(
        ServiceFailure::new(
            HistoryErrorReason::ProtocolNotNegotiated,
            HistoryRecovery::Renegotiate,
            "History V2 was not negotiated on this connection",
        ),
        ExecutionEffect::NotReady,
    )
}

pub(crate) fn backend_unavailable_error() -> AcpError {
    default_error(ServiceFailure::new(
        HistoryErrorReason::BackendUnavailable,
        HistoryRecovery::Retry,
        "History backend is unavailable",
    ))
}

/// 读请求总期限耗尽：执行事实不变、历史未发布，客户端可同键退避重试。
pub(crate) fn timeout_error() -> AcpError {
    default_error(ServiceFailure::timeout())
}

fn domain_error(failure: ServiceFailure, execution: ExecutionEffect) -> AcpError {
    domain_error_with(
        failure,
        HistoryEffect {
            execution,
            history: HistoryEffectState::NotPublished,
            runtime_generation: None,
        },
    )
}

fn domain_error_with(failure: ServiceFailure, effect: HistoryEffect) -> AcpError {
    let data = HistoryErrorData::new(failure.reason, failure.recovery, effect);
    debug_assert!(data.validate().is_ok(), "history error data must be valid");
    AcpError::new(HISTORY_DOMAIN_ERROR_CODE, failure.message)
        .with_data(serde_json::to_value(&data).expect("HistoryErrorData 序列化不应失败"))
}

pub(super) fn invalid_params(error: HistoryContractError) -> AcpError {
    // 契约错误文本只含字段名/规则，不回显 cursor、ticket 或正文。
    AcpError::new(INVALID_PARAMS_CODE, error.to_string())
}

pub(super) fn invalid_params_message(message: &'static str) -> AcpError {
    AcpError::new(INVALID_PARAMS_CODE, message)
}
