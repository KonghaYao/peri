//! App 回调中继：`app-request` → `apps/request`（tools/call）→ `/app-results`。
//!
//! 每个视图一条有界队列（容量 32）与独立序号账本；`Pending` 复用同一
//! `{attachment, epoch, seq}` 重试，终态把**原始** outcome 回填给 App。
//! 不产生 LLM turn，也不直接调用业务工具（只经 facade）。

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use peri_acp_types::interaction::{AttachmentId, ConnectionEpoch};
use peri_acp_types::mcp_apps::{NotExecutedCause, SessionInvocationOutcome};
use peri_acp_types::mcp_apps_facade::{
    AppsOperation, AppsRequestOutcome, AppsRequestRequest, RequestSeq, RequestVerdict,
};

use super::super::request_queue::{DEFAULT_APP_REQUEST_CAPACITY, PendingAppRequest};
use super::super::state;
use super::super::view_client::{ViewAppResult, ViewSessionApi};
use super::{AppsRpc, RELAY_RPC_TIMEOUT};

/// 每视图 App 回调队列容量（与 `AppRequestQueue` 同源）。
pub(super) const RELAY_QUEUE_CAPACITY: usize = DEFAULT_APP_REQUEST_CAPACITY;
/// 单条 App 回调的整体预算（含重试）；超时回填错误，不挂死。
pub(super) const RELAY_BUDGET: Duration = Duration::from_secs(35);
pub(super) const RELAY_RETRY_INTERVAL: Duration = Duration::from_millis(100);
/// 回填 `/app-results` 的 HTTP 预算。
const RESULT_POST_TIMEOUT: Duration = Duration::from_secs(10);
/// 单条 App 回调 `params` 的序列化上限（防恶意 App 用大参数占满队列内存）。
pub(super) const MAX_APP_REQUEST_BYTES: usize = 1024 * 1024;

// ── App 回调中继（tools/call） ──────────────────────────────────────────────

/// 中继上下文：序号账本属于**单个视图**，严禁跨视图复用。
pub(super) struct RelayContext {
    pub(super) rpc: Arc<dyn AppsRpc>,
    pub(super) api: Arc<dyn ViewSessionApi>,
    pub(super) session_id: String,
    pub(super) attachment_id: AttachmentId,
    pub(super) epoch: ConnectionEpoch,
    pub(super) view_id: String,
    pub(super) view_session_id: String,
    pub(super) cancel: CancellationToken,
    pub(super) budget: Duration,
    pub(super) retry_interval: Duration,
}

pub(super) fn start_relay(
    context: RelayContext,
    initial_seq: u64,
) -> (mpsc::Sender<PendingAppRequest>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(RELAY_QUEUE_CAPACITY);
    let join = tokio::spawn(run_relay_worker(rx, context, initial_seq));
    (tx, join)
}

async fn run_relay_worker(
    mut rx: mpsc::Receiver<PendingAppRequest>,
    context: RelayContext,
    initial_seq: u64,
) {
    let mut ledger_next = initial_seq;
    while let Some(request) = tokio::select! {
        biased;
        _ = context.cancel.cancelled() => return,
        request = rx.recv() => request,
    } {
        let seq = request.seq;
        let payload = tokio::select! {
            biased;
            _ = context.cancel.cancelled() => return,
            payload = relay_tools_call(&context, &request, &mut ledger_next) => payload,
        };
        if context.cancel.is_cancelled() {
            return;
        }
        match tokio::time::timeout(
            RESULT_POST_TIMEOUT,
            context
                .api
                .app_result(&context.view_session_id, seq, payload),
        )
        .await
        {
            Ok(Ok(())) => state::update_view(&context.view_id, |view| {
                let changed = view.last_error.is_some();
                view.last_error = None;
                changed
            }),
            Ok(Err(error)) => state::update_view(&context.view_id, |view| {
                view.last_error = Some(error.code().to_string());
                true
            }),
            Err(_) => state::update_view(&context.view_id, |view| {
                view.last_error = Some("app_result_timeout".to_string());
                true
            }),
        }
        // 已回填（成功/失败/超时都算处理完）：待回填计数回落。
        state::update_view(&context.view_id, |view| {
            view.pending_requests = view.pending_requests.saturating_sub(1);
            true
        });
    }
}

/// 队列满时立即回填错误（不静默丢弃）；队列关闭表示视图正在结束。
pub(super) async fn dispatch_app_request(
    relay_tx: &mpsc::Sender<PendingAppRequest>,
    api: &Arc<dyn ViewSessionApi>,
    view_session_id: &str,
    view_id: &str,
    request: PendingAppRequest,
) {
    match relay_tx.try_send(request) {
        Ok(()) => {}
        Err(TrySendError::Full(request)) => {
            tracing::warn!(
                view = %view_id,
                seq = request.seq,
                "MCP App request queue is full; returning an error to the app"
            );
            let _ = tokio::time::timeout(
                RESULT_POST_TIMEOUT,
                api.app_result(
                    view_session_id,
                    request.seq,
                    ViewAppResult::Error {
                        code: -32603,
                        message: "app_request_overflow".to_string(),
                    },
                ),
            )
            .await;
            state::update_view(view_id, |view| {
                view.last_error = Some("app_request_overflow".to_string());
                view.pending_requests = view.pending_requests.saturating_sub(1);
                true
            });
        }
        Err(TrySendError::Closed(_)) => {}
    }
}

/// 单条 `tools/call`：同 `{attachment, epoch, seq}` 有界重试，终态回填原始结果。
pub(super) async fn relay_tools_call(
    context: &RelayContext,
    request: &PendingAppRequest,
    ledger_next: &mut u64,
) -> ViewAppResult {
    if request.method != "tools/call" {
        return ViewAppResult::Error {
            code: -32601,
            message: "unsupported_method".to_string(),
        };
    }
    if serde_json::to_vec(&request.params).map_or(true, |bytes| bytes.len() > MAX_APP_REQUEST_BYTES)
    {
        return ViewAppResult::Error {
            code: -32603,
            message: "app_request_too_large".to_string(),
        };
    }
    let Some(name) = request
        .params
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
    else {
        return ViewAppResult::Error {
            code: -32602,
            message: "invalid_tools_call".to_string(),
        };
    };
    let arguments = match request.params.get("arguments") {
        None => serde_json::Map::new(),
        Some(Value::Object(map)) => map.clone(),
        Some(_) => {
            return ViewAppResult::Error {
                code: -32602,
                message: "invalid_arguments".to_string(),
            };
        }
    };

    let deadline = tokio::time::Instant::now() + context.budget;
    let mut request_seq = *ledger_next;
    loop {
        if context.cancel.is_cancelled() {
            return ViewAppResult::Error {
                code: -32603,
                message: "cancelled".to_string(),
            };
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return timeout_error();
        }
        let call_timeout = (deadline - now).min(RELAY_RPC_TIMEOUT);
        let apps_request = AppsRequestRequest {
            session_id: context.session_id.clone(),
            attachment_id: context.attachment_id.clone(),
            epoch: context.epoch,
            seq: RequestSeq::new(request_seq),
            operation: AppsOperation::ToolsCall {
                name: name.clone(),
                arguments: arguments.clone(),
            },
        };
        let response =
            match tokio::time::timeout(call_timeout, context.rpc.request(apps_request)).await {
                Err(_) => return timeout_error(),
                Ok(Err(error)) => {
                    return ViewAppResult::Error {
                        code: -32603,
                        message: sanitize_message(error.code()),
                    };
                }
                Ok(Ok(response)) => response,
            };
        *ledger_next = response.next_seq.get();
        state::update_view(&context.view_id, |view| {
            let changed = view.next_seq != *ledger_next;
            view.next_seq = *ledger_next;
            changed
        });

        match response.verdict {
            RequestVerdict::SequenceGap { expected } => {
                request_seq = expected.get();
                *ledger_next = request_seq;
                continue;
            }
            RequestVerdict::Accepted
            | RequestVerdict::JoinedPending
            | RequestVerdict::Duplicate => {
                match response.outcome {
                    Some(AppsRequestOutcome::ToolsCall(outcome)) => {
                        return outcome_payload(outcome);
                    }
                    Some(AppsRequestOutcome::Pending) | None => {
                        tokio::select! {
                            biased;
                            _ = context.cancel.cancelled() => {
                                return ViewAppResult::Error {
                                    code: -32603,
                                    message: "cancelled".to_string(),
                                };
                            }
                            _ = peri_time::sleep(context.retry_interval) => {}
                        }
                        // 同 key 重试：ledger 以 {attachment, epoch, seq} 去重。
                        continue;
                    }
                    Some(_) => {
                        return ViewAppResult::Error {
                            code: -32603,
                            message: "unexpected_outcome".to_string(),
                        };
                    }
                }
            }
            RequestVerdict::Conflict => return verdict_error("request_conflict"),
            RequestVerdict::ResultExpired => return verdict_error("result_expired"),
            RequestVerdict::CapacityRejected => return verdict_error("capacity_rejected"),
            RequestVerdict::UnknownNamespace => return verdict_error("unknown_namespace"),
        }
    }
}

fn timeout_error() -> ViewAppResult {
    ViewAppResult::Error {
        code: -32603,
        message: "apps_request_timeout".to_string(),
    }
}

fn verdict_error(message: &str) -> ViewAppResult {
    ViewAppResult::Error {
        code: -32603,
        message: message.to_string(),
    }
}

/// `tools/call` 的 outcome → App 可消费的原始结果/错误。
///
/// `Completed` 原样回填（含 `isError:true`，不折叠成错误）；其余只回填清洗后的
/// 短码，避免把服务端自由文本/任何载荷带进 App。
pub(super) fn outcome_payload(outcome: SessionInvocationOutcome) -> ViewAppResult {
    match outcome {
        SessionInvocationOutcome::Completed(raw) => ViewAppResult::Result(raw.into_value()),
        SessionInvocationOutcome::RemoteProtocolError(error) => ViewAppResult::Error {
            code: error.code,
            message: safe_message(&error.message),
        },
        SessionInvocationOutcome::TaskAccepted(_) => ViewAppResult::Error {
            code: -32603,
            message: "task_accepted".to_string(),
        },
        SessionInvocationOutcome::OutcomeUnknown { reason } => ViewAppResult::Error {
            code: -32603,
            message: format!("outcome_unknown: {}", safe_message(&reason)),
        },
        SessionInvocationOutcome::NotExecuted { cause } => ViewAppResult::Error {
            code: -32603,
            message: not_executed_message(cause).to_string(),
        },
    }
}

fn not_executed_message(cause: NotExecutedCause) -> &'static str {
    match cause {
        NotExecutedCause::TargetRevoked => "target_revoked",
        NotExecutedCause::TargetUnbound => "target_unbound",
        NotExecutedCause::UntrustedOwner => "untrusted_owner",
        NotExecutedCause::ApprovalUnavailable(_) => "approval_unavailable",
        NotExecutedCause::UserRejected => "user_rejected",
        NotExecutedCause::PolicyDenied => "policy_denied",
        NotExecutedCause::CommitConsumed => "commit_consumed",
        NotExecutedCause::CancelledBeforeCommit => "cancelled_before_commit",
    }
}

/// 上游错误文本的对外投影：去控制字符、限长，含 `<`/`>`（HTML）时退回稳定码。
fn safe_message(raw: &str) -> String {
    let sanitized = sanitize_message(raw);
    if sanitized.contains('<') || sanitized.contains('>') {
        "upstream_protocol_error".to_string()
    } else {
        sanitized
    }
}

/// 错误 message 清洗：去控制字符、限制 200 字，空值给稳定占位。
fn sanitize_message(raw: &str) -> String {
    let filtered = raw
        .chars()
        .filter(|ch| !ch.is_control())
        .take(200)
        .collect::<String>();
    let trimmed = filtered.trim();
    if trimmed.is_empty() {
        "error".to_string()
    } else {
        trimmed.to_string()
    }
}
