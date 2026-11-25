//! 单视图生命周期：attach → resources/read → 建网关会话 → SSE 重连。
//!
//! 这里只关心一个 view 的状态流转；App 回调中继在同级 [`relay`] 模块，
//! 发现/打开/关闭由父模块的 runtime manager 承担。

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use peri_acp_types::mcp_apps::MCP_APPS_HTML_MIME;
use peri_acp_types::mcp_apps_facade::{
    AppsAttachRequest, AppsDetachRequest, AppsOperation, AppsRequestOutcome, AppsRequestRequest,
    AttachmentBinding, ClientViewId, RequestSeq, RequestVerdict,
};

use super::super::request_queue::PendingAppRequest;
use super::super::state::{self, AppEntry, ViewPhase};
use super::super::view_client::{
    CreatedViewSession, MAX_VIEW_HTML_BYTES, ViewEvent, ViewSessionApi,
};
use super::frame_decode::{FrameDecodeFn, FrameDecoder};
use super::relay::{
    RELAY_BUDGET, RELAY_RETRY_INTERVAL, RelayContext, dispatch_app_request, start_relay,
};
use super::{AppsRpc, CLEANUP_TIMEOUT, StreamPolicy};

/// `resources/read` 的 `Pending` 轮询上限（与 facade 的 30s 等待预算对齐）。
const READ_POLL_BUDGET: Duration = Duration::from_secs(30);
const READ_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub(super) struct ViewContext {
    pub(super) rpc: Arc<dyn AppsRpc>,
    pub(super) api: Arc<dyn ViewSessionApi>,
    pub(super) session_id: String,
    pub(super) entry: AppEntry,
    pub(super) view_id: String,
    pub(super) cancel: CancellationToken,
    pub(super) policy: StreamPolicy,
    /// 帧解码实现（生产是 JPEG 解码；测试注入可控时序）。
    pub(super) decode: FrameDecodeFn,
}

pub(super) async fn run_view(context: ViewContext) {
    let ViewContext {
        rpc,
        api,
        session_id,
        entry,
        view_id,
        cancel,
        policy,
        decode,
    } = context;

    // 1. attach：领取 record + 请求域身份（不调用业务工具）。
    let attach = AppsAttachRequest {
        session_id: session_id.clone(),
        app_id: entry.app_id.clone(),
        client_view_id: ClientViewId::new(format!("tui-{view_id}")),
        reattach: None,
    };
    let attached = match rpc.attach(attach).await {
        Ok(attached) => attached,
        Err(error) => {
            fail_view(&view_id, error.code());
            return;
        }
    };
    let binding = attached.attachment.clone();
    let snapshot = attached.snapshot;
    if !snapshot.ui_resource.uri.starts_with("ui://")
        || snapshot.ui_resource.mime_type != MCP_APPS_HTML_MIME
    {
        fail_view(&view_id, "apps_ui_resource_invalid");
        return;
    }
    if !snapshot.features.resources_read {
        fail_view(&view_id, "apps_resources_read_unsupported");
        return;
    }
    state::update_view(&view_id, |view| {
        view.attachment_id = Some(binding.attachment_id.to_string());
        view.epoch = Some(binding.connection_epoch.get());
        view.ui_resource_uri = Some(snapshot.ui_resource.uri.clone());
        view.ui_resource_mime = Some(snapshot.ui_resource.mime_type.clone());
        view.next_seq = binding.next_seq.get();
        true
    });

    // 2. resources/read：HTML 正文（校验 MIME / 5 MiB / UTF-8）。
    //    返回的 `next_seq` 是该 attachment 账本在当前视图上的下一个序号，
    //    后续 App 回调（tools/call）从这里继续，不能跨视图复用。
    let (html, ledger_next) = match read_ui_resource(
        &rpc,
        &session_id,
        &binding,
        &snapshot.ui_resource.uri,
        &cancel,
        &view_id,
    )
    .await
    {
        Ok(read) => read,
        Err(code) => {
            fail_view(&view_id, &code);
            detach_best_effort(&rpc, &session_id, &binding).await;
            return;
        }
    };

    // 3. 建网关 view session（HTML 只作为请求 body）。
    let created = match api.create_session(html).await {
        Ok(created) => created,
        Err(error) => {
            fail_view(&view_id, error.code());
            detach_best_effort(&rpc, &session_id, &binding).await;
            return;
        }
    };
    let view_session_id = created.session_id.clone();
    state::update_view(&view_id, |view| {
        view.view_session_id = Some(view_session_id.clone());
        view.events_url = Some(created.events_url.clone());
        true
    });

    // 4. App 回调中继：每视图独立的序号账本与队列（容量 32）。
    let relay_cancel = cancel.child_token();
    let (relay_tx, relay_join) = start_relay(
        RelayContext {
            rpc: Arc::clone(&rpc),
            api: Arc::clone(&api),
            session_id: session_id.clone(),
            attachment_id: binding.attachment_id.clone(),
            epoch: binding.connection_epoch,
            view_id: view_id.clone(),
            view_session_id: created.session_id.clone(),
            cancel: relay_cancel.clone(),
            budget: RELAY_BUDGET,
            retry_interval: RELAY_RETRY_INTERVAL,
        },
        ledger_next,
    );

    // 5. 帧解码：单槽最新帧 + 独立任务；事件循环不等待解码。
    let mut frames = FrameDecoder::start(view_id.clone(), cancel.clone(), decode);

    // 6. SSE：断线重连 + 帧/状态/closed 解析；app-request 进入中继队列。
    let outcome = stream_view(
        &api,
        &view_id,
        &created,
        &cancel,
        policy,
        &relay_tx,
        &mut frames,
    )
    .await;
    // 视图结束：解完最后一帧后停止解码任务（超时 abort，不拖住收尾）。
    frames.finish(CLEANUP_TIMEOUT).await;
    match outcome {
        StreamOutcome::Cancelled | StreamOutcome::RemoteClosed => {}
        StreamOutcome::Unavailable(code) => fail_view(&view_id, &code),
    }
    // 视图结束：停止中继（在途 ACP 调用被取消；网关会话随后 DELETE）。
    relay_cancel.cancel();
    drop(relay_tx);
    let _ = tokio::time::timeout(CLEANUP_TIMEOUT, relay_join).await;

    // 7. 关闭路径：DELETE 网关会话（幂等）；detach 尽力而为。
    let _ = tokio::time::timeout(CLEANUP_TIMEOUT, api.delete_session(&created.session_id)).await;
    detach_best_effort(&rpc, &session_id, &binding).await;
}

fn fail_view(view_id: &str, code: &str) {
    state::update_view(view_id, |view| {
        // 终态（远端 closed/failed）不被收尾错误覆盖。
        if view.phase.is_terminal() {
            return false;
        }
        view.phase = ViewPhase::LocalError {
            code: code.to_string(),
        };
        view.last_error = Some(code.to_string());
        true
    });
    tracing::warn!(view = %view_id, code = %code, "MCP App view failed");
}

async fn detach_best_effort(rpc: &Arc<dyn AppsRpc>, session_id: &str, binding: &AttachmentBinding) {
    let request = AppsDetachRequest {
        session_id: session_id.to_string(),
        attachment_id: binding.attachment_id.clone(),
        epoch: binding.connection_epoch,
    };
    let _ = tokio::time::timeout(CLEANUP_TIMEOUT, rpc.detach(request)).await;
}

/// `resources/read`：`Pending` 用同一 `{seq}` 有界轮询；`SequenceGap` 改用期望值。
async fn read_ui_resource(
    rpc: &Arc<dyn AppsRpc>,
    session_id: &str,
    binding: &AttachmentBinding,
    uri: &str,
    cancel: &CancellationToken,
    view_id: &str,
) -> Result<(String, u64), String> {
    let deadline = tokio::time::Instant::now() + READ_POLL_BUDGET;
    let mut seq = binding.next_seq.get();
    loop {
        if cancel.is_cancelled() {
            return Err("apps_cancelled".to_string());
        }
        if tokio::time::Instant::now() > deadline {
            return Err("apps_read_timeout".to_string());
        }
        let request = AppsRequestRequest {
            session_id: session_id.to_string(),
            attachment_id: binding.attachment_id.clone(),
            epoch: binding.connection_epoch,
            seq: RequestSeq::new(seq),
            operation: AppsOperation::ResourcesRead {
                uri: uri.to_string(),
            },
        };
        let response = rpc
            .request(request)
            .await
            .map_err(|error| error.code().to_string())?;
        let next_seq = response.next_seq.get();
        state::update_view(view_id, |view| {
            let changed = view.next_seq != next_seq;
            view.next_seq = next_seq;
            changed
        });
        match response.verdict {
            RequestVerdict::SequenceGap { expected } => {
                seq = expected.get();
                continue;
            }
            RequestVerdict::Accepted
            | RequestVerdict::JoinedPending
            | RequestVerdict::Duplicate => {
                match response.outcome {
                    Some(AppsRequestOutcome::ResourcesRead { resources }) => {
                        return html_from_resources(&resources, uri).map(|html| (html, next_seq));
                    }
                    Some(AppsRequestOutcome::Pending) | None => {
                        tokio::select! {
                            biased;
                            _ = cancel.cancelled() => return Err("apps_cancelled".to_string()),
                            _ = peri_time::sleep(READ_POLL_INTERVAL) => {}
                        }
                        // 同一 `{attachment, seq}` 重试：ledger 以 key 去重。
                        continue;
                    }
                    Some(_) => return Err("apps_read_unexpected_outcome".to_string()),
                }
            }
            RequestVerdict::Conflict => return Err("apps_read_conflict".to_string()),
            RequestVerdict::ResultExpired => return Err("apps_read_result_expired".to_string()),
            RequestVerdict::CapacityRejected => return Err("apps_read_capacity".to_string()),
            RequestVerdict::UnknownNamespace => {
                return Err("apps_read_unknown_namespace".to_string());
            }
        }
    }
}

/// 从 `resources/read` 的 contents 中取 UI 文档正文。
fn html_from_resources(
    resources: &[peri_acp_types::mcp_apps::RawResource],
    uri: &str,
) -> Result<String, String> {
    let resource = resources
        .iter()
        .find(|resource| resource.is_valid_app_resource(uri))
        .ok_or_else(|| "apps_ui_resource_missing".to_string())?;
    if let Some(text) = resource.text.as_ref() {
        if text.len() > MAX_VIEW_HTML_BYTES {
            return Err("html_too_large".to_string());
        }
        return Ok(text.clone());
    }
    let Some(blob) = resource.blob.as_ref() else {
        return Err("apps_ui_resource_invalid".to_string());
    };
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(blob.as_bytes())
        .map_err(|_| "apps_ui_resource_bad_base64".to_string())?;
    if bytes.len() > MAX_VIEW_HTML_BYTES {
        return Err("html_too_large".to_string());
    }
    String::from_utf8(bytes).map_err(|_| "html_not_utf8".to_string())
}

/// app-request 去重游标（网关 seq 单调递增；重连会重放未回答项）。
#[derive(Debug, Default)]
pub(super) struct AppRequestCursor {
    last: u64,
}

impl AppRequestCursor {
    /// `true` = 首次见到该 seq；`false` = 重连重放/乱序旧值。
    pub(super) fn accept(&mut self, seq: u64) -> bool {
        if seq <= self.last {
            return false;
        }
        self.last = seq;
        true
    }
}

enum StreamOutcome {
    Cancelled,
    RemoteClosed,
    Unavailable(String),
}

async fn stream_view(
    api: &Arc<dyn ViewSessionApi>,
    view_id: &str,
    created: &CreatedViewSession,
    cancel: &CancellationToken,
    policy: StreamPolicy,
    relay_tx: &mpsc::Sender<PendingAppRequest>,
    frames: &mut FrameDecoder,
) -> StreamOutcome {
    let mut attempt = 0u32;
    // SSE 重连时网关会重放尚未回答的 app-request（新连接 cursor 从 0 起）：
    // 按网关 seq 去重，避免同一个待回填请求被执行两次。
    let mut app_requests = AppRequestCursor::default();
    loop {
        if cancel.is_cancelled() {
            return StreamOutcome::Cancelled;
        }
        let connected = api
            .connect_events(&created.session_id, Some(&created.events_url))
            .await;
        let mut source = match connected {
            Ok(source) => source,
            Err(error) => {
                attempt = attempt.saturating_add(1);
                if attempt > policy.max_attempts {
                    return StreamOutcome::Unavailable(error.code().to_string());
                }
                mark_reconnecting(view_id, attempt);
                if wait_backoff(cancel, policy.backoff(attempt)).await {
                    return StreamOutcome::Cancelled;
                }
                continue;
            }
        };
        let mut received_event = false;
        loop {
            let next = tokio::select! {
                biased;
                _ = cancel.cancelled() => return StreamOutcome::Cancelled,
                next = source.next_event() => next,
            };
            match next {
                Ok(Some(ViewEvent::Frame(frame))) => {
                    received_event = true;
                    // 只投递不解码：帧落后时可合并/丢弃，事件流继续。
                    frames.offer(Arc::new(frame));
                }
                Ok(Some(ViewEvent::AppRequest(request))) => {
                    received_event = true;
                    if !app_requests.accept(request.seq) {
                        // 重连重放的同一请求（已入队/已回填）。
                        continue;
                    }
                    state::update_view(view_id, |view| {
                        view.pending_requests = view.pending_requests.saturating_add(1);
                        true
                    });
                    dispatch_app_request(relay_tx, api, &created.session_id, view_id, request)
                        .await;
                }
                Ok(Some(ViewEvent::Closed { reason })) => {
                    // 终态不再接收帧：先把已收到的最后一帧解完再落地 closed，
                    // 保证「关闭前最后一帧」仍会显示（只等一次解码）。
                    frames.finish(CLEANUP_TIMEOUT).await;
                    state::update_view(view_id, |view| {
                        // 失败/本地错误是终态：远端随后发的 closed 只是清理事实，
                        // 不能覆盖失败码（A5：浏览器退出要留下明确原因）。
                        if view.phase.is_terminal() {
                            return false;
                        }
                        view.phase = ViewPhase::Closed { reason };
                        true
                    });
                    return StreamOutcome::RemoteClosed;
                }
                Ok(Some(event)) => {
                    received_event = true;
                    state::update_view(view_id, |view| view.apply_event(&event));
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
        if received_event {
            attempt = 0;
        }
        attempt = attempt.saturating_add(1);
        if attempt > policy.max_attempts {
            return StreamOutcome::Unavailable("sse_unavailable".to_string());
        }
        // 终止态（failed/closed）不再重连。
        if state::APPS_STATE
            .state()
            .read()
            .view(view_id)
            .is_some_and(|view| {
                matches!(
                    view.phase,
                    ViewPhase::Closed { .. } | ViewPhase::Failed { .. }
                )
            })
        {
            return StreamOutcome::RemoteClosed;
        }
        mark_reconnecting(view_id, attempt);
        if wait_backoff(cancel, policy.backoff(attempt)).await {
            return StreamOutcome::Cancelled;
        }
    }
}

fn mark_reconnecting(view_id: &str, attempt: u32) {
    state::update_view(view_id, |view| {
        if view.phase.is_terminal() {
            return false;
        }
        let next = ViewPhase::Reconnecting { attempt };
        if view.phase == next {
            return false;
        }
        view.phase = next;
        true
    });
}

/// `true` = 被取消（调用方应结束）。
async fn wait_backoff(cancel: &CancellationToken, delay: Duration) -> bool {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => true,
        _ = peri_time::sleep(delay) => false,
    }
}
