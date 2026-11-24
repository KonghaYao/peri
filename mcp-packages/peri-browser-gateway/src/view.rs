//! 外部宿主（Peri TUI）驱动的 view session：宿主给 App HTML，网关把它作为顶层
//! 文档渲染，宿主收 screencast 帧、发真实 CDP 输入，并中继 App 的 `tools/call`。
//!
//! 控制面与 `/mcp` 共用主 listener（默认 `127.0.0.1:7391`）：会话创建、输入与
//! 中继都需要 token 鉴权和生命周期管理，放在唯一有业务路由的 listener 上；文档
//! 源仍在独立的 sandbox listener（`127.0.0.1:7392`），只按 session id 提供 HTML，
//! 不持有会话、也不接受宿主命令。
//!
//! App 侧桥接见 [`crate::apps`]：session 文档的 bridge 进入 relay 模式，在 App
//! transport 出站前拦截 JSON-RPC 请求，使 App 自己的 MCP client 收不到自己的
//! 请求；`tools/call` 进入页面队列，由本模块经 CDP `Runtime.evaluate` 取走、
//! 经 SSE 交给宿主，再把宿主的结果投回页面。页面与网关之间没有网络通道
//! （CSP `connect-src 'none'`），console 事件只是唤醒信号。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::StatusCode;
use bytes::Bytes;
use serde_json::{json, Value};
use tokio::sync::{watch, Notify};
use tokio_util::sync::CancellationToken;

use crate::apps::{AppDocuments, MAX_APP_HTML_BYTES};
use crate::config::Config;
use crate::provision::ProvisionSettings;

pub(crate) mod api;
mod page;

pub(crate) use api::router;

/// 并发 view session 上限；至少一个会话可用，其余请求返回 429。
pub const MAX_SESSIONS: usize = 4;
/// 单个会话未回答的 App→宿主请求上限；超出时该请求立即收到 `-32603`。
const MAX_PENDING_REQUESTS: usize = 64;
/// 关闭时的结算上限：页面 target 关闭与任务退出都不无限等待。
const CLOSE_WAIT: Duration = Duration::from_secs(5);

/// view session 注册表；同时是 `/view/sessions*` 路由的鉴权与状态来源。
pub struct ViewSessions {
    token: Option<Arc<str>>,
    documents: Arc<AppDocuments>,
    engine: Arc<EngineHandle>,
    /// 文档源基地址，例如 `http://127.0.0.1:7392`；`None` 表示 sandbox 关闭。
    sandbox_base: Option<String>,
    sessions: Mutex<BTreeMap<String, Arc<ViewSession>>>,
    stop: CancellationToken,
}

impl std::fmt::Debug for ViewSessions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ViewSessions")
            .field("enabled", &self.token.is_some())
            .field("sandbox", &self.sandbox_base.is_some())
            .field(
                "sessions",
                &self.sessions.lock().map(|s| s.len()).unwrap_or(0),
            )
            .finish()
    }
}

impl ViewSessions {
    /// Builds the registry from the gateway config and the document source.
    #[must_use]
    pub fn new(config: &Config, documents: Arc<AppDocuments>) -> Arc<Self> {
        let browser = &config.browser;
        Arc::new(Self {
            token: config.view_token.as_deref().map(Arc::from),
            documents,
            engine: Arc::new(EngineHandle {
                cdp_url: browser.suggested_endpoint.clone(),
                owner: browser.owner.clone(),
                provision: ProvisionSettings {
                    cdp_url: browser.suggested_endpoint.clone(),
                    owner: browser.owner.clone(),
                    lock: browser.provision_lock.clone(),
                    lock_wait: browser.provision_lock_wait,
                    stale_after: browser.lock_stale_after,
                    connect_timeout: browser.connect_timeout,
                },
                page_wait: browser.page_wait,
            }),
            sandbox_base: config.sandbox_bind.map(|bind| format!("http://{bind}")),
            sessions: Mutex::new(BTreeMap::new()),
            stop: CancellationToken::new(),
        })
    }

    /// Whether `PERI_BROWSER_VIEW_TOKEN` / `--view-token` was configured.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.token.is_some()
    }

    pub(crate) fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Creates one session for the supplied App HTML.
    ///
    /// The HTML is validated synchronously (size, UTF-8); the page is created
    /// in the background and its progress is reported through the SSE stream.
    pub(crate) fn create(&self, html: &[u8]) -> Result<Value, ViewError> {
        if !self.enabled() {
            return Err(ViewError::disabled());
        }
        let Some(sandbox_base) = self.sandbox_base.as_deref() else {
            return Err(ViewError::new(
                StatusCode::CONFLICT,
                "sandbox_disabled",
                "the sandbox listener is off; the app view cannot be served",
            ));
        };
        if html.is_empty() {
            return Err(ViewError::new(
                StatusCode::BAD_REQUEST,
                "empty_html",
                "the request body must be the App HTML",
            ));
        }
        if html.len() > MAX_APP_HTML_BYTES {
            return Err(ViewError::too_large());
        }
        let text = std::str::from_utf8(html).map_err(|_| {
            ViewError::new(
                StatusCode::BAD_REQUEST,
                "html_not_utf8",
                "app html must be UTF-8",
            )
        })?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let url = format!("{sandbox_base}/apps/view?view={id}");
        let session = Arc::new(ViewSession::new(
            id.clone(),
            url.clone(),
            self.stop.child_token(),
            self.documents.clone(),
        ));
        {
            let mut sessions = self.sessions.lock().expect("view sessions lock");
            // 失败/已关闭的记录不占用并发名额；它们只为 GET 保留到下次创建。
            sessions.retain(|_, session| session.active());
            if sessions.len() >= MAX_SESSIONS {
                return Err(ViewError::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    "session_limit",
                    format!("at most {MAX_SESSIONS} concurrent view sessions"),
                ));
            }
            self.documents.insert_view(&id, text);
            sessions.insert(id.clone(), session.clone());
        }
        tokio::spawn(page::run(session, self.engine.clone()));
        Ok(json!({
            "sessionId": id,
            "url": url,
            "eventsUrl": format!("/view/sessions/{id}/events"),
            "state": "starting",
        }))
    }

    pub(crate) fn session(&self, id: &str) -> Result<Arc<ViewSession>, ViewError> {
        self.sessions
            .lock()
            .expect("view sessions lock")
            .get(id)
            .cloned()
            .ok_or_else(|| {
                ViewError::new(
                    StatusCode::NOT_FOUND,
                    "session_not_found",
                    "unknown view session",
                )
            })
    }

    /// Closes one session: cancel its tasks, close the page target and release
    /// the document. The record is removed once the page is released.
    pub(crate) async fn delete(&self, id: &str) -> Result<Value, ViewError> {
        let session = self.session(id)?;
        session.close("deleted").await;
        self.sessions.lock().expect("view sessions lock").remove(id);
        self.documents.remove_view(id);
        Ok(json!({ "closed": true, "sessionId": id }))
    }

    pub(crate) fn status(&self, id: &str) -> Result<Value, ViewError> {
        Ok(self.session(id)?.status())
    }

    pub(crate) fn list(&self) -> Value {
        let sessions = self.sessions.lock().expect("view sessions lock");
        Value::Array(sessions.values().map(|session| session.status()).collect())
    }

    /// Stops every session when the gateway shuts down. Page targets stay in
    /// Kitesurf, matching the MCP session contract (README §会话生命周期).
    pub(crate) fn shutdown(&self) {
        self.stop.cancel();
        let sessions = std::mem::take(&mut *self.sessions.lock().expect("view sessions lock"));
        for (id, session) in sessions {
            session.note_reason("gateway_shutdown");
            session.stop.cancel();
            self.documents.remove_view(&id);
        }
    }
}

/// 驱动任务需要的只读配置；`cdp_url` 是 `--cdp-url` 给出的建议端点。
pub(crate) struct EngineHandle {
    pub cdp_url: String,
    pub owner: String,
    pub provision: ProvisionSettings,
    pub page_wait: Duration,
}

/// 会话阶段；Ready 携带唯一的页面 writer，输入与求值都经它下发。
#[derive(Clone)]
pub(crate) enum Phase {
    Starting,
    Ready(Arc<page::PageDriver>),
    Failed(Arc<Failure>),
    Closed,
}

impl std::fmt::Debug for Phase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name())
    }
}

impl Phase {
    fn name(&self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Ready(_) => "ready",
            Self::Failed(_) => "failed",
            Self::Closed => "closed",
        }
    }
}

#[derive(Debug)]
pub(crate) struct Failure {
    pub code: &'static str,
    pub message: String,
}

/// 一帧 screencast：JPEG 原始字节 + 序号 + 尺寸。
pub(crate) struct FrameView {
    pub seq: u64,
    pub jpeg: Bytes,
    pub width: u32,
    pub height: u32,
    pub viewport_width: u32,
    pub viewport_height: u32,
}

/// 一个未回答的 App→宿主请求。
#[derive(Clone)]
pub(crate) struct AppRequest {
    pub seq: u64,
    pub app_id: Value,
    pub method: String,
    pub params: Value,
}

impl AppRequest {
    pub fn to_json(&self) -> Value {
        json!({
            "seq": self.seq,
            "id": self.app_id,
            "method": self.method,
            "params": self.params,
        })
    }
}

#[derive(Default)]
struct Requests {
    next_seq: u64,
    pending: BTreeMap<u64, AppRequest>,
}

/// One view session: lifecycle, frames, App requests and subscriber fan-out.
pub(crate) struct ViewSession {
    id: String,
    url: String,
    stop: CancellationToken,
    phase: watch::Sender<Phase>,
    frames: watch::Sender<Option<Arc<FrameView>>>,
    requests: Mutex<Requests>,
    wake: Notify,
    frame_seq: AtomicU64,
    subscribers: AtomicUsize,
    target: Mutex<Option<String>>,
    closed_reason: Mutex<Option<&'static str>>,
    documents: Arc<AppDocuments>,
}

impl ViewSession {
    fn new(id: String, url: String, stop: CancellationToken, documents: Arc<AppDocuments>) -> Self {
        Self {
            id,
            url,
            stop,
            phase: watch::channel(Phase::Starting).0,
            frames: watch::channel(None).0,
            requests: Mutex::new(Requests::default()),
            wake: Notify::new(),
            frame_seq: AtomicU64::new(0),
            subscribers: AtomicUsize::new(0),
            target: Mutex::new(None),
            closed_reason: Mutex::new(None),
            documents,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn stop(&self) -> &CancellationToken {
        &self.stop
    }

    pub fn documents(&self) -> &Arc<AppDocuments> {
        &self.documents
    }

    pub fn subscribe_frames(&self) -> watch::Receiver<Option<Arc<FrameView>>> {
        self.frames.subscribe()
    }

    pub fn subscribe_phase(&self) -> watch::Receiver<Phase> {
        self.phase.subscribe()
    }

    pub fn wake(&self) -> &Notify {
        &self.wake
    }

    pub fn phase(&self) -> Phase {
        self.phase.borrow().clone()
    }

    /// True while the session still owns page resources (starting or ready).
    fn active(&self) -> bool {
        matches!(&*self.phase.borrow(), Phase::Starting | Phase::Ready(_))
    }

    pub fn ready(&self, driver: Arc<page::PageDriver>) {
        self.phase.send_replace(Phase::Ready(driver));
        self.wake.notify_waiters();
    }

    pub fn fail(&self, code: &'static str, message: impl Into<String>) {
        let message = message.into();
        tracing::warn!(session = %self.id, code, %message, "view session failed");
        self.phase
            .send_replace(Phase::Failed(Arc::new(Failure { code, message })));
        self.wake.notify_waiters();
    }

    pub fn set_target(&self, target: &str) {
        *self.target.lock().expect("view target lock") = Some(target.to_owned());
    }

    pub fn take_target(&self) -> Option<String> {
        self.target.lock().expect("view target lock").take()
    }

    pub fn driver(&self) -> Result<Arc<page::PageDriver>, ViewError> {
        match &*self.phase.borrow() {
            Phase::Ready(driver) => Ok(driver.clone()),
            Phase::Starting => Err(ViewError::new(
                StatusCode::CONFLICT,
                "session_starting",
                "the view session is still starting; wait for the SSE ready event",
            )),
            Phase::Failed(failure) => Err(ViewError::new(
                StatusCode::BAD_GATEWAY,
                failure.code,
                failure.message.clone(),
            )),
            Phase::Closed => Err(ViewError::new(
                StatusCode::NOT_FOUND,
                "session_closed",
                "the view session is closed",
            )),
        }
    }

    pub fn record_frame(
        &self,
        jpeg: Bytes,
        width: u32,
        height: u32,
        viewport: Option<crate::cdp::Viewport>,
    ) {
        let seq = self.frame_seq.fetch_add(1, Ordering::AcqRel) + 1;
        let (viewport_width, viewport_height) = viewport.map_or((width, height), |viewport| {
            (viewport.width, viewport.height)
        });
        self.frames.send_replace(Some(Arc::new(FrameView {
            seq,
            jpeg,
            width,
            height,
            viewport_width,
            viewport_height,
        })));
    }

    /// Queues one App request; `None` means the queue is full.
    pub fn enqueue(&self, app_id: Value, method: String, params: Value) -> Option<u64> {
        let mut requests = self.requests.lock().expect("view requests lock");
        if requests.pending.len() >= MAX_PENDING_REQUESTS {
            return None;
        }
        requests.next_seq += 1;
        let seq = requests.next_seq;
        requests.pending.insert(
            seq,
            AppRequest {
                seq,
                app_id,
                method,
                params,
            },
        );
        drop(requests);
        self.wake.notify_waiters();
        Some(seq)
    }

    /// Removes and returns one pending request, so a result is delivered once.
    pub fn answer(&self, seq: u64) -> Result<AppRequest, ViewError> {
        self.requests
            .lock()
            .expect("view requests lock")
            .pending
            .remove(&seq)
            .ok_or_else(|| {
                ViewError::new(
                    StatusCode::NOT_FOUND,
                    "unknown_request",
                    format!("no pending app request with seq {seq}"),
                )
            })
    }

    pub fn pending_after(&self, cursor: u64) -> Vec<AppRequest> {
        self.requests
            .lock()
            .expect("view requests lock")
            .pending
            .range(cursor + 1..)
            .map(|(_, request)| request.clone())
            .collect()
    }

    pub fn subscriber_joined(&self) {
        self.subscribers.fetch_add(1, Ordering::AcqRel);
    }

    pub fn subscriber_left(&self) {
        self.subscribers.fetch_sub(1, Ordering::AcqRel);
    }

    pub fn subscriber_count(&self) -> usize {
        self.subscribers.load(Ordering::Acquire)
    }

    /// Terminal phase after the page target is released; wakes SSE streams.
    pub fn finish(&self) {
        self.phase.send_replace(Phase::Closed);
        self.frames.send_replace(None);
        self.wake.notify_waiters();
    }

    pub fn status(&self) -> Value {
        let phase = self.phase();
        let target = self.target.lock().expect("view target lock").clone();
        let mut value = json!({
            "sessionId": self.id,
            "url": self.url,
            "state": phase.name(),
            "frames": self.frame_seq.load(Ordering::Acquire),
            "subscribers": self.subscribers.load(Ordering::Acquire),
            "pendingRequests": self.requests.lock().expect("view requests lock").pending.len(),
            "targetId": target,
        });
        match phase {
            Phase::Failed(failure) => {
                value["code"] = json!(failure.code);
                value["message"] = json!(failure.message);
            }
            Phase::Ready(driver) => {
                value["targetId"] = json!(driver.target_id());
            }
            Phase::Starting | Phase::Closed => {}
        }
        value
    }

    /// Cancels the session and waits for the page target to be released.
    pub async fn close(&self, reason: &'static str) {
        if self.record_reason(reason).is_none() {
            tracing::debug!(session = %self.id, reason, "closing view session");
        }
        self.stop.cancel();
        self.wake.notify_waiters();
        let mut phase = self.phase.subscribe();
        let deadline = tokio::time::Instant::now() + CLOSE_WAIT;
        loop {
            if matches!(&*phase.borrow_and_update(), Phase::Closed) {
                return;
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                tracing::warn!(session = %self.id, "view session did not settle before its deadline");
                return;
            }
            if tokio::time::timeout(remaining, phase.changed())
                .await
                .is_err()
            {
                tracing::warn!(session = %self.id, "view session did not settle before its deadline");
                return;
            }
        }
    }

    /// Records the first close reason; later callers keep the earliest one.
    pub fn note_reason(&self, reason: &'static str) {
        let _ = self.record_reason(reason);
    }

    fn record_reason(&self, reason: &'static str) -> Option<&'static str> {
        let mut slot = self.closed_reason.lock().expect("view closed reason lock");
        match *slot {
            Some(existing) => Some(existing),
            None => {
                *slot = Some(reason);
                None
            }
        }
    }

    pub fn closed_reason(&self) -> &'static str {
        self.closed_reason
            .lock()
            .expect("view closed reason lock")
            .unwrap_or("closed")
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// 控制面错误：JSON body 只带稳定 code 与可公开说明。
#[derive(Debug)]
pub(crate) struct ViewError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ViewError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn disabled() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "view_api_disabled",
            "view api needs PERI_BROWSER_VIEW_TOKEN (or --view-token) on the gateway",
        )
    }

    pub fn too_large() -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "html_too_large",
            format!("app html must be at most {MAX_APP_HTML_BYTES} bytes"),
        )
    }

    pub fn upstream(code: &'static str, error: impl std::fmt::Display) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, code, error.to_string())
    }

    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }
}

#[cfg(test)]
#[path = "view_test.rs"]
mod tests;
