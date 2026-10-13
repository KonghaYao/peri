//! `SessionAppsFacade` 的**结果投影**（准入判决 / 未执行 / 资源失败 → 响应形状）。
//!
//! 从 `apps_facade.rs` 拆出（`STD-SIZE-001`：facade 本体已近 1000 行）；本文件只做
//! 投影，不做判决、不持有状态，也不构造第二份权威值。

use peri_acp_types::mcp_apps::{NotExecutedCause, SessionInvocationOutcome};
use peri_acp_types::mcp_apps_facade::{
    AppsRequestOutcome, AppsRequestResponse, RequestNamespaceId, RequestSeq, RequestVerdict,
};

use super::super::apps_ledger::{not_executed_before_commit, AdmissionVerdict, RequestLedger};

pub(super) fn admission_verdict(verdict: &AdmissionVerdict) -> RequestVerdict {
    match verdict {
        AdmissionVerdict::Accepted { .. } => RequestVerdict::Accepted,
        AdmissionVerdict::JoinedPending { .. } => RequestVerdict::JoinedPending,
        AdmissionVerdict::Duplicate { .. } => RequestVerdict::Duplicate,
        AdmissionVerdict::Conflict => RequestVerdict::Conflict,
        AdmissionVerdict::ResultExpired => RequestVerdict::ResultExpired,
        AdmissionVerdict::SequenceGap { expected } => RequestVerdict::SequenceGap {
            expected: *expected,
        },
        AdmissionVerdict::CapacityRejected => RequestVerdict::CapacityRejected,
        AdmissionVerdict::UnknownNamespace => RequestVerdict::UnknownNamespace,
    }
}

pub(super) fn unexecuted_response(
    verdict: RequestVerdict,
    ledger: &RequestLedger,
    namespace: Option<&RequestNamespaceId>,
) -> AppsRequestResponse {
    AppsRequestResponse {
        verdict,
        admitted: verdict.is_admitted(),
        outcome: None,
        next_seq: namespace
            .map(|namespace| ledger.next_seq(namespace))
            .unwrap_or_else(|| RequestSeq::new(1)),
    }
}

pub(super) fn not_executed_outcome() -> AppsRequestOutcome {
    AppsRequestOutcome::ToolsCall(not_executed_before_commit())
}

/// 资源操作失败：显式未执行（4a 没有资源级的 `NotExecutedCause` 变体，也不新增；
/// 失败发生在任何 wire 调用之前，绝不呈现为成功）。
pub(super) fn resource_failure(
    kind: peri_acp_types::mcp_apps::McpAppsErrorKind,
) -> AppsRequestOutcome {
    tracing::debug!(
        kind = kind.as_str(),
        "MCP Apps resource operation failed before any side effect"
    );
    AppsRequestOutcome::ToolsCall(SessionInvocationOutcome::NotExecuted {
        cause: NotExecutedCause::TargetRevoked,
    })
}
