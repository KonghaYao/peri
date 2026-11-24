//! 网关 view-session HTTP 客户端：建会话 / SSE 订阅 / 关闭。
//!
//! 安全约束：
//! - App HTML 只作为 `POST /view/sessions` 的 body 发送，不落盘、不进日志；
//! - token 只进 `Authorization: Bearer` 头，不进 URL；[`ViewToken`] 的 `Debug`
//!   已脱敏，本模块的错误文本也只含状态码/错误码；
//! - 帧 base64 只在本模块内解析成字节，不写日志。

use std::collections::VecDeque;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::config::ViewToken;
use super::mapping::ViewportPoint;
use super::request_queue::PendingAppRequest;
use super::sse::{RawSseEvent, SseParser};

/// App HTML 上限（与网关 `MAX_APP_HTML_BYTES` 同源契约：5 MiB UTF-8）。
pub const MAX_VIEW_HTML_BYTES: usize = 5 * 1024 * 1024;

/// 单次 HTTP 操作的超时（SSE 长连接除外）。
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// SSE 两次数据之间的最长静默；网关每 15s 发 keepalive 注释。
const SSE_READ_TIMEOUT: Duration = Duration::from_secs(45);

/// view 客户端错误：只保留错误码与（可选）HTTP 状态码。
///
/// 不携带 URL、响应正文或 token——调用方可以安全地展示/记录 `Display`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewClientError {
    code: String,
    status: Option<u16>,
}

impl ViewClientError {
    #[must_use]
    pub fn new(code: impl Into<String>) -> Self {
        Self {
            code: sanitize_code(&code.into()).unwrap_or_else(|| "view_client_error".to_string()),
            status: None,
        }
    }

    fn with_status(status: u16, body: &[u8]) -> Self {
        // 网关 `ViewError` 用 `{error, message}`；部分代理/测试用 `{code}`。
        // 两个键都接受，始终只保留短错误码，绝不回显 message。
        let body_code = serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|value| {
                value
                    .get("code")
                    .or_else(|| value.get("error"))
                    .and_then(Value::as_str)
                    .and_then(sanitize_code)
            })
            .unwrap_or_else(|| format!("http_{status}"));
        Self {
            code: body_code,
            status: Some(status),
        }
    }

    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    #[must_use]
    pub fn status(&self) -> Option<u16> {
        self.status
    }
}

impl std::fmt::Display for ViewClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(status) => write!(formatter, "{} (HTTP {status})", self.code),
            None => formatter.write_str(&self.code),
        }
    }
}

impl std::error::Error for ViewClientError {}

/// 错误码白名单：只接受短 ASCII 标识符，防响应正文注入 UI/日志。
fn sanitize_code(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return None;
    }
    trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/'))
        .then(|| trimmed.to_string())
}

// ── 业务事件 ───────────────────────────────────────────────────────────────

/// `POST /view/sessions` 的应答（`201`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedViewSession {
    pub session_id: String,
    /// 相对于控制面基地址的事件路径（网关当前返回 `/view/sessions/{id}/events`）。
    pub events_url: String,
}

/// 回填给 App 的 `tools/call` 结果：原始 MCP result 或 JSON-RPC error。
///
/// `Result` 保存**原始** `SessionInvocationOutcome::Completed` 的 JSON value，
/// 不做文本化；`Error` 只允许清洗后的 code/message（不含 token/HTML/帧）。
#[derive(Clone, PartialEq)]
pub enum ViewAppResult {
    Result(Value),
    Error { code: i64, message: String },
}

impl std::fmt::Debug for ViewAppResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Result(_) => formatter.write_str("ViewAppResult::Result(<redacted>)"),
            Self::Error { code, message } => formatter
                .debug_struct("ViewAppResult::Error")
                .field("code", code)
                .field("message", message)
                .finish(),
        }
    }
}

#[derive(Debug, Serialize)]
struct ClickBody {
    action: &'static str,
    x: f64,
    y: f64,
}

#[derive(Debug, Serialize)]
struct AppResultBody<'a> {
    seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<AppErrorBody<'a>>,
}

#[derive(Debug, Serialize)]
struct AppErrorBody<'a> {
    code: i64,
    message: &'a str,
}

/// 网关状态快照（`meta` / `state` 事件）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewSessionStatus {
    pub state: String,
    /// `state:"failed"` 时的错误码；不含 message 正文。
    pub code: Option<String>,
    pub frames: u64,
    pub pending_requests: u64,
}

/// 一帧 screencast（`frame` 事件，`format` 固定为 jpeg）。
///
/// 自定义 `Debug`：`data_base64` 是画面内容，禁止随日志输出。
#[derive(Clone, PartialEq, Eq)]
pub struct FrameEvent {
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    pub viewport_width: u32,
    pub viewport_height: u32,
    /// base64 编码的 JPEG payload；只在 [`AppsViewClient`] 的消费方解码。
    pub data_base64: String,
}

impl std::fmt::Debug for FrameEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FrameEvent")
            .field("seq", &self.seq)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("viewport_width", &self.viewport_width)
            .field("viewport_height", &self.viewport_height)
            .field("data_base64", &"<redacted>")
            .finish()
    }
}

/// 解码后的 view 事件。
#[derive(Debug, Clone, PartialEq)]
pub enum ViewEvent {
    /// 状态快照（含当前帧序号统计）。
    Meta(ViewSessionStatus),
    /// 状态变化（starting / ready / failed）。
    Status(ViewSessionStatus),
    Frame(FrameEvent),
    /// App 回调请求；runtime 的中继队列据此回填 `app-results`。
    AppRequest(PendingAppRequest),
    /// 会话终结；`reason` 为网关给的关闭原因（无秘密）。
    Closed {
        reason: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawStatus {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    frames: u64,
    #[serde(default)]
    pending_requests: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFrame {
    seq: u64,
    width: u32,
    height: u32,
    viewport_width: u32,
    viewport_height: u32,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    data: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawClosed {
    #[serde(default)]
    reason: Option<String>,
}

/// 网关事件名 → 业务事件。未知事件 / keepalive / 解析失败返回 `None`。
#[must_use]
pub fn decode_event(raw: &RawSseEvent) -> Option<ViewEvent> {
    let value: Value = serde_json::from_str(&raw.data).ok()?;
    match raw.event.as_str() {
        "meta" => {
            let status: RawStatus = serde_json::from_value(value).ok()?;
            Some(ViewEvent::Meta(ViewSessionStatus {
                state: status.state?,
                code: status.code.and_then(|code| sanitize_code(&code)),
                frames: status.frames,
                pending_requests: status.pending_requests,
            }))
        }
        "state" => {
            let status: RawStatus = serde_json::from_value(value).ok()?;
            Some(ViewEvent::Status(ViewSessionStatus {
                state: status.state?,
                code: status.code.and_then(|code| sanitize_code(&code)),
                frames: status.frames,
                pending_requests: status.pending_requests,
            }))
        }
        "frame" => {
            let frame: RawFrame = serde_json::from_value(value).ok()?;
            if frame.format.as_deref() != Some("jpeg") {
                return None;
            }
            let data_base64 = frame.data.filter(|data| !data.is_empty())?;
            Some(ViewEvent::Frame(FrameEvent {
                seq: frame.seq,
                width: frame.width,
                height: frame.height,
                viewport_width: frame.viewport_width,
                viewport_height: frame.viewport_height,
                data_base64,
            }))
        }
        "app-request" => PendingAppRequest::from_wire(value).map(ViewEvent::AppRequest),
        "closed" => {
            let closed: RawClosed = serde_json::from_value(value).ok()?;
            Some(ViewEvent::Closed {
                reason: closed.reason.and_then(|reason| sanitize_code(&reason)),
            })
        }
        _ => None,
    }
}

/// 把 base64 帧解码成 JPEG 字节（不校验内容；解码由调用方的图像线程完成）。
pub fn decode_frame_bytes(frame: &FrameEvent) -> Result<Vec<u8>, ViewClientError> {
    base64::engine::general_purpose::STANDARD
        .decode(frame.data_base64.as_bytes())
        .map_err(|_| ViewClientError::new("frame_base64_invalid"))
}

// ── 客户端 ─────────────────────────────────────────────────────────────────

/// view-session 控制面客户端。
#[derive(Clone)]
pub struct AppsViewClient {
    http: reqwest::Client,
    base: String,
    token: ViewToken,
}

impl std::fmt::Debug for AppsViewClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppsViewClient")
            .field("base", &self.base)
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
}

impl AppsViewClient {
    /// 构造客户端；redirect 一律禁用，避免 Authorization 被带到别的主机。
    pub fn new(base: &str, token: ViewToken) -> Result<Self, ViewClientError> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|_| ViewClientError::new("view_client_build_failed"))?;
        Ok(Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            token,
        })
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.base, path.trim_start_matches('/'))
    }

    /// `POST /view/sessions`：body 是 App HTML 原文。
    pub async fn create_session(&self, html: &str) -> Result<CreatedViewSession, ViewClientError> {
        if html.is_empty() {
            return Err(ViewClientError::new("empty_html"));
        }
        if html.len() > MAX_VIEW_HTML_BYTES {
            return Err(ViewClientError::new("html_too_large"));
        }
        let response = tokio::time::timeout(
            HTTP_TIMEOUT,
            self.http
                .post(self.endpoint("/view/sessions"))
                .header(reqwest::header::AUTHORIZATION, self.authorization())
                .header(reqwest::header::CONTENT_TYPE, "text/plain; charset=utf-8")
                .body(html.to_string())
                .send(),
        )
        .await
        .map_err(|_| ViewClientError::new("view_create_timeout"))?
        .map_err(|_| ViewClientError::new("view_create_transport"))?;

        let status = response.status();
        let body = response.bytes().await.unwrap_or_default();
        if status != reqwest::StatusCode::CREATED {
            return Err(ViewClientError::with_status(status.as_u16(), &body));
        }
        let parsed: Value = serde_json::from_slice(&body)
            .map_err(|_| ViewClientError::new("view_create_bad_json"))?;
        let session_id = parsed
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| ViewClientError::new("view_create_missing_session"))?;
        let events_url = parsed
            .get("eventsUrl")
            .and_then(Value::as_str)
            .filter(|url| !url.is_empty())
            .ok_or_else(|| ViewClientError::new("view_create_missing_events_url"))?;
        Ok(CreatedViewSession {
            session_id: session_id.to_string(),
            events_url: events_url.to_string(),
        })
    }

    /// `DELETE /view/sessions/{id}`；404 视为已关闭（幂等）。
    pub async fn delete_session(&self, session_id: &str) -> Result<(), ViewClientError> {
        let response = tokio::time::timeout(
            HTTP_TIMEOUT,
            self.http
                .delete(self.endpoint(&format!("/view/sessions/{session_id}")))
                .header(reqwest::header::AUTHORIZATION, self.authorization())
                .send(),
        )
        .await
        .map_err(|_| ViewClientError::new("view_delete_timeout"))?
        .map_err(|_| ViewClientError::new("view_delete_transport"))?;
        if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        let status = response.status().as_u16();
        let body = response.bytes().await.unwrap_or_default();
        Err(ViewClientError::with_status(status, &body))
    }

    /// `POST /view/sessions/{id}/click`：真实 CDP 鼠标点击（CSS 视口坐标）。
    ///
    /// 坐标来自真实终端事件经 [`crate::kit::apps::mapping`] 的映射；本方法
    /// 不记录坐标（日志/错误只保留错误码）。
    pub async fn click(
        &self,
        session_id: &str,
        point: ViewportPoint,
    ) -> Result<(), ViewClientError> {
        if !point.x.is_finite() || !point.y.is_finite() {
            return Err(ViewClientError::new("invalid_point"));
        }
        let body = ClickBody {
            action: "click",
            x: point.x,
            y: point.y,
        };
        let response = tokio::time::timeout(
            HTTP_TIMEOUT,
            self.http
                .post(self.endpoint(&format!("/view/sessions/{session_id}/click")))
                .header(reqwest::header::AUTHORIZATION, self.authorization())
                .json(&body)
                .send(),
        )
        .await
        .map_err(|_| ViewClientError::new("view_click_timeout"))?
        .map_err(|_| ViewClientError::new("view_click_transport"))?;
        if response.status().is_success() {
            return Ok(());
        }
        let status = response.status().as_u16();
        let body = response.bytes().await.unwrap_or_default();
        Err(ViewClientError::with_status(status, &body))
    }

    /// `POST /view/sessions/{id}/app-results`：回填 App 的 `tools/call`。
    ///
    /// body 形状以网关 `AppResultBody` 为准：`{seq, result}` 或
    /// `{seq, error:{code,message}}`，二者恰好一个。
    pub async fn app_result(
        &self,
        session_id: &str,
        seq: u64,
        result: ViewAppResult,
    ) -> Result<(), ViewClientError> {
        let body = match &result {
            ViewAppResult::Result(value) => AppResultBody {
                seq,
                result: Some(value),
                error: None,
            },
            ViewAppResult::Error { code, message } => AppResultBody {
                seq,
                result: None,
                error: Some(AppErrorBody {
                    code: *code,
                    message,
                }),
            },
        };
        let response = tokio::time::timeout(
            HTTP_TIMEOUT,
            self.http
                .post(self.endpoint(&format!("/view/sessions/{session_id}/app-results")))
                .header(reqwest::header::AUTHORIZATION, self.authorization())
                .json(&body)
                .send(),
        )
        .await
        .map_err(|_| ViewClientError::new("app_result_timeout"))?
        .map_err(|_| ViewClientError::new("app_result_transport"))?;
        if response.status().is_success() {
            return Ok(());
        }
        let status = response.status().as_u16();
        let body = response.bytes().await.unwrap_or_default();
        Err(ViewClientError::with_status(status, &body))
    }

    /// 解析事件地址：相对路径拼到控制面；绝对 URL 必须与控制面同源
    /// （网关响应不可把 bearer token 引到别的主机）。
    fn events_endpoint(
        &self,
        session_id: &str,
        events_url: Option<&str>,
    ) -> Result<String, ViewClientError> {
        let Some(path) = events_url else {
            return Ok(self.endpoint(&format!("/view/sessions/{session_id}/events")));
        };
        if !path.starts_with("http://") && !path.starts_with("https://") {
            return Ok(self.endpoint(path));
        }
        let base = reqwest::Url::parse(&self.base)
            .map_err(|_| ViewClientError::new("view_base_invalid"))?;
        let target =
            reqwest::Url::parse(path).map_err(|_| ViewClientError::new("events_url_invalid"))?;
        if base.origin() != target.origin() {
            return Err(ViewClientError::new("events_url_origin_mismatch"));
        }
        Ok(target.to_string())
    }

    /// `GET /view/sessions/{id}/events` 并构造事件流。
    pub async fn connect_events(
        &self,
        session_id: &str,
        events_url: Option<&str>,
    ) -> Result<ViewEventStream, ViewClientError> {
        let url = self.events_endpoint(session_id, events_url)?;
        let response = tokio::time::timeout(
            HTTP_TIMEOUT,
            self.http
                .get(url)
                .header(reqwest::header::AUTHORIZATION, self.authorization())
                .header(reqwest::header::ACCEPT, "text/event-stream")
                .send(),
        )
        .await
        .map_err(|_| ViewClientError::new("view_events_timeout"))?
        .map_err(|_| ViewClientError::new("view_events_transport"))?;
        let status = response.status();
        if status != reqwest::StatusCode::OK {
            let body = response.bytes().await.unwrap_or_default();
            return Err(ViewClientError::with_status(status.as_u16(), &body));
        }
        Ok(ViewEventStream {
            response,
            parser: SseParser::new(),
            pending: VecDeque::new(),
        })
    }

    fn authorization(&self) -> String {
        format!("Bearer {}", self.token.expose())
    }
}

/// 一次 SSE 连接的事件流；`next_event` 返回 `Ok(None)` 表示连接结束。
pub struct ViewEventStream {
    response: reqwest::Response,
    parser: SseParser,
    pending: VecDeque<RawSseEvent>,
}

impl std::fmt::Debug for ViewEventStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ViewEventStream")
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
}

impl ViewEventStream {
    /// 读取下一个业务事件；未知事件与 keepalive 被跳过。
    pub async fn next_event(&mut self) -> Result<Option<ViewEvent>, ViewClientError> {
        loop {
            if let Some(raw) = self.pending.pop_front() {
                if let Some(event) = decode_event(&raw) {
                    return Ok(Some(event));
                }
                continue;
            }
            if self.parser.overflowed() {
                return Err(ViewClientError::new("sse_overflow"));
            }
            let chunk = tokio::time::timeout(SSE_READ_TIMEOUT, self.response.chunk())
                .await
                .map_err(|_| ViewClientError::new("sse_timeout"))?
                .map_err(|_| ViewClientError::new("sse_transport"))?;
            let Some(chunk) = chunk else {
                return Ok(None);
            };
            self.pending.extend(self.parser.feed(&chunk));
        }
    }
}

// ── 抽象：便于 runtime 用 fake 替换（测试不依赖真实网关） ────────────────────

/// 事件源抽象：生产实现是 [`ViewEventStream`]。
#[async_trait]
pub trait ViewEventSource: Send {
    /// `Ok(None)` = 连接正常结束；`Err` = 断线/错误，由调用方决定重连。
    async fn next_event(&mut self) -> Result<Option<ViewEvent>, ViewClientError>;
}

#[async_trait]
impl ViewEventSource for ViewEventStream {
    async fn next_event(&mut self) -> Result<Option<ViewEvent>, ViewClientError> {
        ViewEventStream::next_event(self).await
    }
}

/// view-session API 抽象；runtime 只依赖它，测试注入本地 stub。
#[async_trait]
pub trait ViewSessionApi: Send + Sync {
    async fn create_session(&self, html: String) -> Result<CreatedViewSession, ViewClientError>;
    async fn delete_session(&self, session_id: &str) -> Result<(), ViewClientError>;
    async fn connect_events(
        &self,
        session_id: &str,
        events_url: Option<&str>,
    ) -> Result<Box<dyn ViewEventSource>, ViewClientError>;
    async fn click(&self, session_id: &str, point: ViewportPoint) -> Result<(), ViewClientError>;
    async fn app_result(
        &self,
        session_id: &str,
        seq: u64,
        result: ViewAppResult,
    ) -> Result<(), ViewClientError>;
}

#[async_trait]
impl ViewSessionApi for AppsViewClient {
    async fn create_session(&self, html: String) -> Result<CreatedViewSession, ViewClientError> {
        AppsViewClient::create_session(self, &html).await
    }

    async fn delete_session(&self, session_id: &str) -> Result<(), ViewClientError> {
        AppsViewClient::delete_session(self, session_id).await
    }

    async fn connect_events(
        &self,
        session_id: &str,
        events_url: Option<&str>,
    ) -> Result<Box<dyn ViewEventSource>, ViewClientError> {
        let stream = AppsViewClient::connect_events(self, session_id, events_url).await?;
        Ok(Box::new(stream))
    }

    async fn click(&self, session_id: &str, point: ViewportPoint) -> Result<(), ViewClientError> {
        AppsViewClient::click(self, session_id, point).await
    }

    async fn app_result(
        &self,
        session_id: &str,
        seq: u64,
        result: ViewAppResult,
    ) -> Result<(), ViewClientError> {
        AppsViewClient::app_result(self, session_id, seq, result).await
    }
}

#[cfg(test)]
#[path = "view_client_test.rs"]
mod tests;
