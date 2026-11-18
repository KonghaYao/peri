//! History V2 错误契约（设计 §12.1）。
//!
//! 领域失败统一使用 JSON-RPC `-32000`，由 `data.domain`/`data.reason` 区分；格式错误仍
//! 走 `-32602`。本模块只定义 wire 结构、类型层错误与二者之间的映射，不决定 JSON-RPC
//! code 的发送时机。

use serde::{Deserialize, Serialize};

use super::{HISTORY_DOMAIN, HISTORY_V2_VERSION};

/// 历史领域失败原因；`serde` 形状为 snake_case。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryErrorReason {
    ProtocolNotNegotiated,
    UnsupportedVersion,
    InvalidCursor,
    CrossWindowCursor,
    SnapshotInvalidated,
    WindowExpired,
    WindowClosed,
    TicketInvalid,
    AnchorUnavailable,
    PermissionDenied,
    NotFound,
    IndexNotReady,
    SourceUnavailable,
    CapacityExceeded,
    BudgetExceeded,
    BackendUnavailable,
    Timeout,
    RequestConflict,
    OperationExpired,
    OperationInProgress,
    DeliveryExpired,
    DeliveryGap,
    Busy,
    /// 仅用于类型层拒绝的格式错误；发送方应使用 JSON-RPC `-32602`。
    InvalidRequest,
}

/// 客户端恢复建议。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryRecovery {
    None,
    Retry,
    Reopen,
    Renegotiate,
    /// 缩小读取范围（页/正文预算超限）。
    ReduceScope,
}

/// 执行事实。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionEffect {
    NotReady,
    Ready,
    Unchanged,
}

/// 历史发布事实。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HistoryEffectState {
    NotPublished,
    Published,
    Unchanged,
}

/// 错误响应里的 `data.effect`。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryEffect {
    pub execution: ExecutionEffect,
    pub history: HistoryEffectState,
    /// 非 `ready` 时显式输出 `null`：effect 是闭合事实，缺省键会被误读成未知。
    #[serde(default)]
    pub runtime_generation: Option<String>,
}

impl HistoryEffect {
    /// 请求在发布前被拒绝：执行未变、历史未发布。
    pub fn unchanged() -> Self {
        Self {
            execution: ExecutionEffect::Unchanged,
            history: HistoryEffectState::Unchanged,
            runtime_generation: None,
        }
    }

    /// `execution=ready` 必须携带 `runtimeGeneration`。
    pub fn validate(&self) -> Result<(), HistoryContractError> {
        if self.execution == ExecutionEffect::Ready
            && self.runtime_generation.as_deref().is_none_or(str::is_empty)
        {
            return Err(HistoryContractError::shape(
                "execution=ready requires a non-empty runtimeGeneration",
            ));
        }
        Ok(())
    }
}

/// JSON-RPC `error.data` 的 History V2 形状。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryErrorData {
    pub domain: String,
    pub version: u32,
    pub reason: HistoryErrorReason,
    pub recovery: HistoryRecovery,
    pub effect: HistoryEffect,
}

impl HistoryErrorData {
    /// 以固定 domain/version 构造。
    pub fn new(
        reason: HistoryErrorReason,
        recovery: HistoryRecovery,
        effect: HistoryEffect,
    ) -> Self {
        Self {
            domain: HISTORY_DOMAIN.to_owned(),
            version: HISTORY_V2_VERSION,
            reason,
            recovery,
            effect,
        }
    }

    /// 校验 domain/version 与 effect 的闭合约束。
    pub fn validate(&self) -> Result<(), HistoryContractError> {
        if self.domain != HISTORY_DOMAIN {
            return Err(HistoryContractError::shape(
                "history error data.domain must be peri.history",
            ));
        }
        if self.version != HISTORY_V2_VERSION {
            return Err(HistoryContractError::UnsupportedVersion);
        }
        self.effect.validate()
    }
}

/// History V2 契约层的类型错误。
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HistoryContractError {
    #[error("invalid history request: {detail}")]
    InvalidRequest { detail: String },
    #[error("unsupported history protocol version")]
    UnsupportedVersion,
    #[error("history limits or budgets are invalid: {detail}")]
    Limits { detail: String },
    #[error("history identity field {field} exceeds its wire limit")]
    IdentityTooLong { field: &'static str },
    #[error("history payload shape is invalid: {detail}")]
    Shape { detail: String },
}

impl HistoryContractError {
    /// 构造格式错误。
    pub fn invalid_request(detail: impl Into<String>) -> Self {
        Self::InvalidRequest {
            detail: detail.into(),
        }
    }

    /// 构造预算/限额错误。
    pub fn limits(detail: impl Into<String>) -> Self {
        Self::Limits {
            detail: detail.into(),
        }
    }

    /// 构造 wire 形状错误。
    pub fn shape(detail: impl Into<String>) -> Self {
        Self::Shape {
            detail: detail.into(),
        }
    }

    /// 映射到领域 `reason`。格式错误使用仅类型层使用的 `invalid_request`。
    pub fn reason(&self) -> HistoryErrorReason {
        match self {
            Self::UnsupportedVersion => HistoryErrorReason::UnsupportedVersion,
            Self::Limits { .. } => HistoryErrorReason::BudgetExceeded,
            Self::InvalidRequest { .. } | Self::IdentityTooLong { .. } | Self::Shape { .. } => {
                HistoryErrorReason::InvalidRequest
            }
        }
    }

    /// 映射到恢复建议。
    pub fn recovery(&self) -> HistoryRecovery {
        match self {
            Self::UnsupportedVersion => HistoryRecovery::Renegotiate,
            Self::InvalidRequest { .. }
            | Self::Limits { .. }
            | Self::IdentityTooLong { .. }
            | Self::Shape { .. } => HistoryRecovery::None,
        }
    }

    /// 映射为可直接放入 JSON-RPC `error.data` 的结构。
    pub fn to_error_data(&self) -> HistoryErrorData {
        HistoryErrorData::new(self.reason(), self.recovery(), HistoryEffect::unchanged())
    }
}
