//! 浏览器（emscripten）部署的 MCP HTTP 通道：以宿主 `fetch` 实现 rmcp 的
//! `StreamableHttpClient`。
//!
//! 浏览器的网络只有一个入口——宿主 `fetch`；reqwest 在 wasm 目标上无法发起真实
//! I/O。因此把 HTTP 连接的 I/O 载体换成 fetch。线协议语义（请求头、状态码分支、
//! JSON-RPC 错误解释、SSE 分块解析、事件大小上限）与 rmcp 的 reqwest 实现逐条对齐，
//! 差异只有 I/O 载体，以及 SSE 限流器按原始字节自行实现（rmcp 的
//! `bounded_sse_stream` 是其 crate 私有，无法复用）。
//!
//! `JsValue` 不是 `Send`：所有 JS 交互都关在 `spawn_local` 任务里，任务只把纯数据
//! （状态、响应头、正文文本、字节分块）经 channel 送回，因此方法返回的 future 满足
//! rmcp 要求的 `Send`。状态分类、限流与保留头校验不依赖 JS，在原生目标照常编译
//! 并由单测覆盖（浏览器侧回归只能人工验证，见 `docs/verification`）。
#![cfg_attr(not(target_os = "emscripten"), allow(dead_code))]

use std::borrow::Cow;
use std::collections::HashMap;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use bytes::Bytes;
use futures::{stream::BoxStream, Stream, StreamExt};
use reqwest::header::{HeaderName, HeaderValue};
use rmcp::model::{
    ClientJsonRpcMessage, ClientRequest, ErrorData, JsonRpcMessage, ServerJsonRpcMessage,
};
use rmcp::transport::common::http_header::{
    EVENT_STREAM_MIME_TYPE, HEADER_LAST_EVENT_ID, HEADER_MCP_PROTOCOL_VERSION, HEADER_SESSION_ID,
    JSON_MIME_TYPE,
};
use rmcp::transport::streamable_http_client::{
    AuthRequiredError, InsufficientScopeError, StreamableHttpError, StreamableHttpPostResponse,
};
use sse_stream::{Error as SseError, Sse, SseStream};
use tokio::sync::mpsc;

#[cfg(target_os = "emscripten")]
use js_sys::{Array, Promise, Reflect, Uint8Array};
#[cfg(target_os = "emscripten")]
use rmcp::transport::streamable_http_client::StreamableHttpClient;
#[cfg(target_os = "emscripten")]
use std::sync::Arc;
#[cfg(target_os = "emscripten")]
use tokio::sync::oneshot;
#[cfg(target_os = "emscripten")]
use wasm_bindgen::{prelude::*, JsCast, JsValue};
#[cfg(target_os = "emscripten")]
use wasm_bindgen_futures::{spawn_local, JsFuture};

/// 单条 SSE 事件的字节上限；与 rmcp 的 `DEFAULT_MAX_SSE_EVENT_SIZE` 同值。
pub(crate) const DEFAULT_MAX_SSE_EVENT_SIZE: usize = 16 * 1024 * 1024;

/// SSE 分块通道容量：靠背压而不是缓冲堆叠，避免远端慢速或超大事件撑爆内存。
const SSE_CHANNEL_CAPACITY: usize = 2;

/// 解析失败时正文预览的字节长度（与 rmcp 的 `JSON_RPC_BODY_PREVIEW_LEN` 同值）。
const JSON_RPC_BODY_PREVIEW_LEN: usize = 256;

// ---------------------------------------------------------------------------
// 纯判定逻辑（不依赖 JS，原生目标同样编译）
// ---------------------------------------------------------------------------

/// fetch 通道的错误：`JsValue` 不能跨 `Send` 边界，JS 失败在前端收敛为文本。
#[derive(Debug, Clone)]
pub(crate) struct FetchHttpError {
    message: String,
}

impl FetchHttpError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for FetchHttpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for FetchHttpError {}

/// 通道错误 → rmcp 传输错误的 `Client` 变体（rmcp 对 `reqwest::Error` 亦如此实现）。
/// 库里没有 `From<E> for StreamableHttpError<E>` 的通用实现，`?` 需要这一条才能用。
impl From<FetchHttpError> for StreamableHttpError<FetchHttpError> {
    fn from(error: FetchHttpError) -> Self {
        Self::Client(error)
    }
}

/// 一次 fetch 响应的纯数据形态：状态、响应头与正文来源。
pub(crate) struct FetchResponse {
    status: u16,
    content_type: Option<String>,
    content_length: Option<u64>,
    session_id: Option<String>,
    www_authenticate: Option<String>,
    body: FetchBody,
}

/// 正文来源：非 SSE 形态读全量文本，SSE 形态交给分块通道。
enum FetchBody {
    Text(String),
    Stream(FetchByteStream),
}

/// fetch 正文的字节分块来源，由 `spawn_local` 泵送任务写入。
///
/// 接收端 drop 后发送失败，泵送任务随之退出并中止 fetch（`FetchHandle::drop`）。
struct FetchByteStream {
    receiver: mpsc::Receiver<Result<Bytes, FetchHttpError>>,
}

/// 自定义请求头不得覆盖的保留头；`MCP-Protocol-Version` 由 transport worker
/// 在握手后注入，放行（对齐 rmcp 的 `validate_custom_header`）。
fn validate_custom_header(name: &HeaderName) -> Result<(), StreamableHttpError<FetchHttpError>> {
    let reserved = [
        "accept",
        HEADER_SESSION_ID,
        HEADER_MCP_PROTOCOL_VERSION,
        HEADER_LAST_EVENT_ID,
    ];
    let allowed_override = name
        .as_str()
        .eq_ignore_ascii_case(HEADER_MCP_PROTOCOL_VERSION);
    if !allowed_override
        && reserved
            .iter()
            .any(|entry| name.as_str().eq_ignore_ascii_case(entry))
    {
        return Err(StreamableHttpError::ReservedHeaderConflict(
            name.to_string(),
        ));
    }
    Ok(())
}

/// 组装一次请求的头部表：接受集、鉴权、会话与自定义头（顺序即最终覆盖顺序）。
fn request_headers(
    auth_header: Option<&str>,
    session_id: Option<&str>,
    last_event_id: Option<&str>,
    custom_headers: &HashMap<HeaderName, HeaderValue>,
) -> Result<Vec<(String, String)>, StreamableHttpError<FetchHttpError>> {
    let mut headers = vec![(
        "accept".to_string(),
        format!("{EVENT_STREAM_MIME_TYPE}, {JSON_MIME_TYPE}"),
    )];
    if let Some(token) = auth_header {
        headers.push(("authorization".to_string(), format!("Bearer {token}")));
    }
    if let Some(session) = session_id {
        headers.push((HEADER_SESSION_ID.to_string(), session.to_string()));
    }
    if let Some(last_event) = last_event_id {
        headers.push((HEADER_LAST_EVENT_ID.to_string(), last_event.to_string()));
    }
    for (name, value) in custom_headers {
        validate_custom_header(name)?;
        let value = value.to_str().map_err(|_| {
            StreamableHttpError::Client(FetchHttpError::new(format!(
                "请求头 {name} 不是可发送的文本值"
            )))
        })?;
        headers.push((name.as_str().to_string(), value.to_string()));
    }
    Ok(headers)
}

/// 把 JSON-RPC 错误正文解析成 `ServerJsonRpcMessage`（非错误正文返回 `None`）。
fn parse_json_rpc_error(body: &str) -> Option<ServerJsonRpcMessage> {
    match serde_json::from_str::<ServerJsonRpcMessage>(body) {
        Ok(message @ JsonRpcMessage::Error(_)) => Some(message),
        _ => None,
    }
}

/// 解析失败正文的短预览：空正文、HTML 与 `{}` 必须能互相区分。
fn body_preview(body: &str) -> String {
    if body.is_empty() {
        return "<empty>".to_string();
    }
    if body.len() <= JSON_RPC_BODY_PREVIEW_LEN {
        return body.to_string();
    }
    let mut end = JSON_RPC_BODY_PREVIEW_LEN;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &body[..end])
}

/// 无会话 attached 的 4xx：把正文里的 JSON-RPC 错误重新关联到 `server/discover`，
/// 让 lifecycle 能区分「现代拒绝」与「legacy 对端」，而不是丢掉错误只看 HTTP 文案。
///
/// 对齐 rmcp 的同名逻辑（其实现是 crate 私有，无法复用）：只有 client error、
/// 未附会话、且请求确实是 discover 时才重关联；鉴权失败与 5xx 走原路径。
fn legacy_discover_response(
    message: &ClientJsonRpcMessage,
    session_was_attached: bool,
    status: u16,
    body: &str,
) -> Option<StreamableHttpPostResponse> {
    if session_was_attached || !(400..500).contains(&status) || matches!(status, 401 | 403) {
        return None;
    }
    let ClientJsonRpcMessage::Request(request) = message else {
        return None;
    };
    if !matches!(request.request, ClientRequest::DiscoverRequest(_)) {
        return None;
    }
    let error = match serde_json::from_str::<ServerJsonRpcMessage>(body) {
        Ok(ServerJsonRpcMessage::Error(error)) => error.error,
        _ => ErrorData::invalid_request(
            format!("server/discover rejected with HTTP {status}: {body}"),
            None,
        ),
    };
    Some(StreamableHttpPostResponse::Json(
        ServerJsonRpcMessage::error(error, Some(request.id.clone())),
        None,
    ))
}

/// POST 响应的状态分类：与 rmcp 的 reqwest 实现逐条对齐。
fn classify_post_response(
    response: FetchResponse,
    message: &ClientJsonRpcMessage,
    session_was_attached: bool,
    max_sse_event_size: usize,
) -> Result<StreamableHttpPostResponse, StreamableHttpError<FetchHttpError>> {
    let FetchResponse {
        status,
        content_type,
        content_length,
        session_id,
        www_authenticate,
        body,
    } = response;
    if status == 401 {
        if let Some(header) = www_authenticate {
            return Err(StreamableHttpError::AuthRequired(AuthRequiredError::new(
                header,
            )));
        }
    }
    if status == 403 {
        if let Some(header) = www_authenticate {
            return Err(StreamableHttpError::InsufficientScope(
                InsufficientScopeError::new(header, None),
            ));
        }
    }
    if matches!(status, 202 | 204) {
        return Ok(StreamableHttpPostResponse::Accepted);
    }
    if status == 404 && session_was_attached {
        return Err(StreamableHttpError::SessionExpired);
    }
    // 通知 / 回执 / 错误 POST 不等回复：空正文（含缺 content-length 的 200）等价 202。
    let no_reply_expected = matches!(
        message,
        ClientJsonRpcMessage::Notification(_)
            | ClientJsonRpcMessage::Response(_)
            | ClientJsonRpcMessage::Error(_)
    );
    let body_is_empty = match &body {
        FetchBody::Text(text) => text.is_empty(),
        FetchBody::Stream(_) => false,
    };
    if (200..300).contains(&status)
        && no_reply_expected
        && (content_length == Some(0) || body_is_empty)
    {
        return Ok(StreamableHttpPostResponse::Accepted);
    }
    if !(200..300).contains(&status) {
        let text = match body {
            FetchBody::Text(text) => text,
            // 非 2xx 的正文形态按文本解释（任务只在 SSE 内容类型下给出分块）。
            FetchBody::Stream(_) => String::new(),
        };
        if let Some(response) =
            legacy_discover_response(message, session_was_attached, status, &text)
        {
            return Ok(response);
        }
        if content_type_is(&content_type, JSON_MIME_TYPE) {
            if let Some(parsed) = parse_json_rpc_error(&text) {
                return Ok(StreamableHttpPostResponse::Json(parsed, session_id));
            }
            tracing::warn!(status, "JSON-RPC 错误正文解析失败，按传输失败上报");
        }
        return Err(StreamableHttpError::UnexpectedServerResponse(Cow::Owned(
            format!("HTTP {status}: {text}"),
        )));
    }
    if content_type_is(&content_type, EVENT_STREAM_MIME_TYPE) {
        let FetchBody::Stream(stream) = body else {
            return Err(StreamableHttpError::UnexpectedServerResponse(
                Cow::Borrowed("SSE 内容类型缺少分块正文"),
            ));
        };
        return Ok(StreamableHttpPostResponse::Sse(
            sse_stream(stream, max_sse_event_size),
            session_id,
        ));
    }
    if content_type_is(&content_type, JSON_MIME_TYPE) {
        let FetchBody::Text(text) = body else {
            return Err(StreamableHttpError::UnexpectedServerResponse(
                Cow::Borrowed("JSON 内容类型却收到分块正文"),
            ));
        };
        return match serde_json::from_str::<ServerJsonRpcMessage>(&text) {
            Ok(parsed) => Ok(StreamableHttpPostResponse::Json(parsed, session_id)),
            Err(error) if no_reply_expected => {
                tracing::warn!(%error, "JSON 正文不可解析，按 Accepted 处理通知类 POST");
                Ok(StreamableHttpPostResponse::Accepted)
            }
            Err(error) => Err(StreamableHttpError::UnexpectedServerResponse(Cow::Owned(
                format!(
                    "could not parse JSON response as ServerJsonRpcMessage: {error}: {}",
                    body_preview(&text)
                ),
            ))),
        };
    }
    Err(StreamableHttpError::UnexpectedContentType(content_type))
}

fn content_type_is(content_type: &Option<String>, mime: &str) -> bool {
    content_type
        .as_deref()
        .is_some_and(|value| value.as_bytes().starts_with(mime.as_bytes()))
}

/// 原始字节流 → SSE 事件流，并在字节层强制单事件大小上限。
fn sse_stream(
    source: FetchByteStream,
    max_sse_event_size: usize,
) -> BoxStream<'static, Result<Sse, SseError>> {
    let bounded = BoundedSseBytes {
        inner: source,
        limiter: SseEventSizeLimiter::new(max_sse_event_size),
        failed: false,
    };
    SseStream::from_bytes_stream(bounded).boxed()
}

/// 单事件字节限流：统计自上一个空行以来的原始字节，超限即终止流。
///
/// 与 rmcp 的实现相比注释行同样计入（更严格、更简单，内存上界一致）。
struct SseEventSizeLimiter {
    max_size: usize,
    event_size: usize,
    line_size: usize,
    previous_was_cr: bool,
}

impl SseEventSizeLimiter {
    fn new(max_size: usize) -> Self {
        Self {
            max_size,
            event_size: 0,
            line_size: 0,
            previous_was_cr: false,
        }
    }

    fn observe(&mut self, chunk: &[u8]) -> Result<(), ()> {
        for &byte in chunk {
            if self.previous_was_cr {
                self.previous_was_cr = false;
                if byte == b'\n' {
                    continue;
                }
            }
            match byte {
                b'\r' => {
                    self.finish_line()?;
                    self.previous_was_cr = true;
                }
                b'\n' => self.finish_line()?,
                _ => {
                    self.line_size = self.line_size.saturating_add(1);
                    self.check_limit()?;
                }
            }
        }
        Ok(())
    }

    fn finish_line(&mut self) -> Result<(), ()> {
        if self.line_size == 0 {
            // 空行即事件边界：计数归零，下一个事件重新计量。
            self.event_size = 0;
        } else {
            self.event_size = self
                .event_size
                .saturating_add(self.line_size)
                .saturating_add(1);
        }
        self.line_size = 0;
        self.check_limit()
    }

    fn check_limit(&self) -> Result<(), ()> {
        if self.event_size.saturating_add(self.line_size) > self.max_size {
            Err(())
        } else {
            Ok(())
        }
    }
}

/// 限流在中游：先于 SSE 解析，超限事件根本不会进入解析器的行缓冲。
struct BoundedSseBytes {
    inner: FetchByteStream,
    limiter: SseEventSizeLimiter,
    failed: bool,
}

impl Stream for BoundedSseBytes {
    type Item = Result<Bytes, FetchHttpError>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.failed {
            return Poll::Ready(None);
        }
        match ready!(this.inner.receiver.poll_recv(context)) {
            None => Poll::Ready(None),
            Some(Err(error)) => {
                this.failed = true;
                Poll::Ready(Some(Err(error)))
            }
            Some(Ok(chunk)) => {
                if this.limiter.observe(&chunk).is_err() {
                    this.failed = true;
                    Poll::Ready(Some(Err(FetchHttpError::new(format!(
                        "SSE 单事件超过 {} 字节上限",
                        this.limiter.max_size
                    )))))
                } else {
                    Poll::Ready(Some(Ok(chunk)))
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 宿主 fetch 桥（仅 emscripten 编译）
// ---------------------------------------------------------------------------

#[cfg(target_os = "emscripten")]
#[wasm_bindgen(inline_js = r#"
export function periMcpFetchBegin(url, method, headers, body) {
    const controller = new AbortController();
    const handle = { controller, reader: null };
    const init = { method, headers, signal: controller.signal, redirect: 'manual' };
    if (body !== null) init.body = body;
    handle.response = globalThis.fetch(url, init);
    return handle;
}
export function periMcpFetchResponse(handle) { return handle.response; }
export function periMcpFetchMeta(response) {
    return [response.status, JSON.stringify(Object.fromEntries(response.headers))];
}
export function periMcpFetchRead(handle, response) {
    if (!response.body) return Promise.resolve(null);
    handle.reader ??= response.body.getReader();
    return handle.reader.read().then((chunk) => (chunk.done ? null : chunk.value));
}
export function periMcpFetchText(handle, response) { return response.text(); }
export function periMcpFetchDrop(handle) {
    try { handle.controller.abort(); } catch (error) { console.error('MCP fetch abort failed', error); }
    if (handle.reader) {
        try {
            // 已中止时 reader.cancel() 以 AbortError 拒绝是中止本身的结果，不是清理失败。
            handle.reader.cancel().catch((error) => {
                if (error?.name !== 'AbortError') console.error('MCP fetch reader cancel failed', error);
            });
        } catch (error) { console.error('MCP fetch reader cancel failed', error); }
    }
}
"#)]
#[cfg(target_os = "emscripten")]
extern "C" {
    #[wasm_bindgen(js_name = periMcpFetchBegin, catch)]
    fn begin_fetch(
        url: &str,
        method: &str,
        headers: &JsValue,
        body: &JsValue,
    ) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = periMcpFetchResponse)]
    fn fetch_response(handle: &JsValue) -> Promise;
    #[wasm_bindgen(js_name = periMcpFetchMeta, catch)]
    fn fetch_meta(response: &JsValue) -> Result<Array, JsValue>;
    #[wasm_bindgen(js_name = periMcpFetchRead, catch)]
    fn fetch_read(handle: &JsValue, response: &JsValue) -> Result<Promise, JsValue>;
    #[wasm_bindgen(js_name = periMcpFetchText, catch)]
    fn fetch_text(handle: &JsValue, response: &JsValue) -> Result<Promise, JsValue>;
    #[wasm_bindgen(js_name = periMcpFetchDrop)]
    fn fetch_drop(handle: &JsValue);
}

/// 一次 fetch 请求的纯数据描述（可跨 `spawn_local` 边界移动）。
#[cfg(target_os = "emscripten")]
struct FetchRequest {
    url: String,
    method: &'static str,
    headers: Vec<(String, String)>,
    body: Option<String>,
}

/// 持有宿主侧请求句柄：drop 即中止请求并释放读取器。
#[cfg(target_os = "emscripten")]
struct FetchHandle {
    handle: JsValue,
}

#[cfg(target_os = "emscripten")]
impl FetchHandle {
    fn open(request: &FetchRequest) -> Result<Self, StreamableHttpError<FetchHttpError>> {
        let headers = headers_object(&request.headers)?;
        let body = match &request.body {
            Some(body) => JsValue::from_str(body),
            None => JsValue::NULL,
        };
        let handle =
            begin_fetch(&request.url, request.method, &headers, &body).map_err(js_error)?;
        Ok(Self { handle })
    }
}

#[cfg(target_os = "emscripten")]
impl Drop for FetchHandle {
    fn drop(&mut self) {
        fetch_drop(&self.handle);
    }
}

/// 响应头与状态（JS 侧的 `Headers` 迭代顺序是「按名去重、小写」）。
#[cfg(target_os = "emscripten")]
struct ResponseMeta {
    status: u16,
    content_type: Option<String>,
    content_length: Option<u64>,
    session_id: Option<String>,
    www_authenticate: Option<String>,
}

#[cfg(target_os = "emscripten")]
impl ResponseMeta {
    fn into_response(self, body: FetchBody) -> FetchResponse {
        FetchResponse {
            status: self.status,
            content_type: self.content_type,
            content_length: self.content_length,
            session_id: self.session_id,
            www_authenticate: self.www_authenticate,
            body,
        }
    }
}

/// 浏览器 fetch 客户端：无状态句柄，全部 JS 交互都在 `spawn_local` 任务内完成。
#[cfg(target_os = "emscripten")]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FetchHttpClient;

#[cfg(target_os = "emscripten")]
impl StreamableHttpClient for FetchHttpClient {
    type Error = FetchHttpError;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        self.post_message_with_max_sse_event_size(
            uri,
            message,
            session_id,
            auth_header,
            custom_headers,
            DEFAULT_MAX_SSE_EVENT_SIZE,
        )
        .await
    }

    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        let session_was_attached = session_id.is_some();
        let body = serde_json::to_string(&message).map_err(|error| {
            StreamableHttpError::Client(FetchHttpError::new(format!(
                "JSON-RPC 消息序列化失败: {error}"
            )))
        })?;
        let mut headers = request_headers(
            auth_header.as_deref(),
            session_id.as_deref(),
            None,
            &custom_headers,
        )?;
        // 放在最后：JSON 正文的内容类型由本客户端决定（与 reqwest 的 `.json()` 同序）。
        headers.push(("content-type".to_string(), JSON_MIME_TYPE.to_string()));
        let request = FetchRequest {
            url: uri.to_string(),
            method: "POST",
            headers,
            body: Some(body),
        };
        let response = spawn_post(request).await?;
        classify_post_response(response, &message, session_was_attached, max_sse_event_size)
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        let headers = request_headers(
            auth_header.as_deref(),
            Some(session_id.as_ref()),
            None,
            &custom_headers,
        )?;
        let request = FetchRequest {
            url: uri.to_string(),
            method: "DELETE",
            headers,
            body: None,
        };
        spawn_delete(request).await
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>> {
        self.get_stream_with_max_sse_event_size(
            uri,
            session_id,
            last_event_id,
            auth_header,
            custom_headers,
            DEFAULT_MAX_SSE_EVENT_SIZE,
        )
        .await
    }

    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>> {
        let headers = request_headers(
            auth_header.as_deref(),
            session_id.as_deref(),
            last_event_id.as_deref(),
            &custom_headers,
        )?;
        let request = FetchRequest {
            url: uri.to_string(),
            method: "GET",
            headers,
            body: None,
        };
        let stream = spawn_stream(request).await?;
        Ok(sse_stream(stream, max_sse_event_size))
    }
}

/// POST：响应判定在任务内完成；SSE 形态把句柄移交泵送任务后继续推送分块。
#[cfg(target_os = "emscripten")]
async fn spawn_post(
    request: FetchRequest,
) -> Result<FetchResponse, StreamableHttpError<FetchHttpError>> {
    let (sender, receiver) = oneshot::channel();
    spawn_local(async move {
        let _ = sender.send(post_once(request).await);
    });
    receiver
        .await
        .map_err(|_| StreamableHttpError::TransportChannelClosed)?
}

#[cfg(target_os = "emscripten")]
async fn post_once(
    request: FetchRequest,
) -> Result<FetchResponse, StreamableHttpError<FetchHttpError>> {
    let handle = FetchHandle::open(&request)?;
    let response = await_response(&handle).await?;
    let meta = response_meta(&response)?;
    if content_type_is(&meta.content_type, EVENT_STREAM_MIME_TYPE) {
        let (sender, receiver) = mpsc::channel(SSE_CHANNEL_CAPACITY);
        // 句柄随泵送任务存活；接收端 drop 后任务退出并中止请求。
        spawn_local(pump_body(handle, response, sender));
        return Ok(meta.into_response(FetchBody::Stream(FetchByteStream { receiver })));
    }
    let text = read_text(&handle, &response).await?;
    Ok(meta.into_response(FetchBody::Text(text)))
}

/// GET：只判定响应头，正文始终是 SSE 分块。
#[cfg(target_os = "emscripten")]
async fn spawn_stream(
    request: FetchRequest,
) -> Result<FetchByteStream, StreamableHttpError<FetchHttpError>> {
    let (sender, receiver) = oneshot::channel();
    spawn_local(async move {
        let _ = sender.send(stream_once(request).await);
    });
    receiver
        .await
        .map_err(|_| StreamableHttpError::TransportChannelClosed)?
}

#[cfg(target_os = "emscripten")]
async fn stream_once(
    request: FetchRequest,
) -> Result<FetchByteStream, StreamableHttpError<FetchHttpError>> {
    let handle = FetchHandle::open(&request)?;
    let response = await_response(&handle).await?;
    let meta = response_meta(&response)?;
    if meta.status == 401 {
        if let Some(header) = meta.www_authenticate {
            return Err(StreamableHttpError::AuthRequired(AuthRequiredError::new(
                header,
            )));
        }
    }
    if meta.status == 403 {
        if let Some(header) = meta.www_authenticate {
            return Err(StreamableHttpError::InsufficientScope(
                InsufficientScopeError::new(header, None),
            ));
        }
    }
    if meta.status == 405 {
        return Err(StreamableHttpError::ServerDoesNotSupportSse);
    }
    if !(200..300).contains(&meta.status) {
        return Err(StreamableHttpError::UnexpectedServerResponse(Cow::Owned(
            format!("HTTP {}: SSE 流打开失败", meta.status),
        )));
    }
    match meta.content_type.as_deref() {
        Some(content_type)
            if content_type
                .as_bytes()
                .starts_with(EVENT_STREAM_MIME_TYPE.as_bytes())
                || content_type
                    .as_bytes()
                    .starts_with(JSON_MIME_TYPE.as_bytes()) => {}
        other => {
            return Err(StreamableHttpError::UnexpectedContentType(
                other.map(str::to_string),
            ))
        }
    }
    let (sender, receiver) = mpsc::channel(SSE_CHANNEL_CAPACITY);
    spawn_local(pump_body(handle, response, sender));
    Ok(FetchByteStream { receiver })
}

/// DELETE：删除会话是状态变更请求，正文读到结束再返回，避免半途中止连接。
#[cfg(target_os = "emscripten")]
async fn spawn_delete(request: FetchRequest) -> Result<(), StreamableHttpError<FetchHttpError>> {
    let (sender, receiver) = oneshot::channel();
    spawn_local(async move {
        let _ = sender.send(delete_once(request).await);
    });
    receiver
        .await
        .map_err(|_| StreamableHttpError::TransportChannelClosed)?
}

#[cfg(target_os = "emscripten")]
async fn delete_once(request: FetchRequest) -> Result<(), StreamableHttpError<FetchHttpError>> {
    let handle = FetchHandle::open(&request)?;
    let response = await_response(&handle).await?;
    let meta = response_meta(&response)?;
    if meta.status == 405 {
        // server 不支持删除会话（rmcp 同语义：按成功处理）。
        tracing::debug!("MCP server 不支持删除会话");
        return Ok(());
    }
    let text = read_text(&handle, &response).await?;
    if !(200..300).contains(&meta.status) {
        return Err(StreamableHttpError::UnexpectedServerResponse(Cow::Owned(
            format!(
                "HTTP {}: 会话删除失败: {}",
                meta.status,
                body_preview(&text)
            ),
        )));
    }
    Ok(())
}

/// 分块泵：把宿主读取器的分块推进通道；接收端消失即退出（`FetchHandle` drop 中止请求）。
#[cfg(target_os = "emscripten")]
async fn pump_body(
    handle: FetchHandle,
    response: JsValue,
    sender: mpsc::Sender<Result<Bytes, FetchHttpError>>,
) {
    loop {
        let reading = match fetch_read(&handle.handle, &response) {
            Ok(promise) => promise,
            Err(error) => {
                let _ = sender.send(Err(js_client_error(&error))).await;
                return;
            }
        };
        let chunk = match JsFuture::from(reading).await {
            Ok(chunk) => chunk,
            Err(error) => {
                let _ = sender.send(Err(js_client_error(&error))).await;
                return;
            }
        };
        if chunk.is_null() || chunk.is_undefined() {
            return;
        }
        let bytes = match chunk.dyn_into::<Uint8Array>() {
            Ok(chunk) => Bytes::from(chunk.to_vec()),
            Err(error) => {
                let _ = sender.send(Err(js_client_error(&error))).await;
                return;
            }
        };
        if sender.send(Ok(bytes)).await.is_err() {
            return;
        }
    }
}

#[cfg(target_os = "emscripten")]
async fn await_response(
    handle: &FetchHandle,
) -> Result<JsValue, StreamableHttpError<FetchHttpError>> {
    JsFuture::from(fetch_response(&handle.handle))
        .await
        .map_err(js_error)
}

#[cfg(target_os = "emscripten")]
async fn read_text(
    handle: &FetchHandle,
    response: &JsValue,
) -> Result<String, StreamableHttpError<FetchHttpError>> {
    let promise = fetch_text(&handle.handle, response).map_err(js_error)?;
    let text = JsFuture::from(promise).await.map_err(js_error)?;
    text.as_string()
        .ok_or_else(|| js_error(JsValue::from_str("fetch 正文不是文本")))
}

#[cfg(target_os = "emscripten")]
fn response_meta(response: &JsValue) -> Result<ResponseMeta, StreamableHttpError<FetchHttpError>> {
    let meta = fetch_meta(response).map_err(js_error)?;
    let status = meta
        .get(0)
        .as_f64()
        .filter(|status| status.fract() == 0.0 && (100.0..=599.0).contains(status))
        .ok_or_else(|| js_client_error(&JsValue::from_str("fetch 返回了非法状态码")))?;
    let headers_json = meta.get(1).as_string().unwrap_or_else(|| "{}".to_string());
    let headers: HashMap<String, String> =
        serde_json::from_str(&headers_json).map_err(|error| {
            js_client_error(&JsValue::from_str(&format!(
                "fetch 响应头不是 JSON 对象: {error}"
            )))
        })?;
    Ok(ResponseMeta {
        status: status as u16,
        content_type: find_header(&headers, "content-type"),
        content_length: find_header(&headers, "content-length")
            .and_then(|value| value.parse().ok()),
        session_id: find_header(&headers, HEADER_SESSION_ID),
        www_authenticate: find_header(&headers, "www-authenticate"),
    })
}

/// 宿主 `Headers` 迭代后按小写名去重；这里仍按大小写不敏感查找，避免依赖宿主行为。
fn find_header(headers: &HashMap<String, String>, name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(key, value)| key.eq_ignore_ascii_case(name) && !value.is_empty())
        .map(|(_, value)| value.clone())
}

#[cfg(target_os = "emscripten")]
fn headers_object(
    headers: &[(String, String)],
) -> Result<JsValue, StreamableHttpError<FetchHttpError>> {
    let object = js_sys::Object::new();
    for (name, value) in headers {
        Reflect::set(&object, &JsValue::from_str(name), &JsValue::from_str(value))
            .map_err(|error| js_client_error(&error))?;
    }
    Ok(object.into())
}

#[cfg(target_os = "emscripten")]
fn js_error(error: JsValue) -> StreamableHttpError<FetchHttpError> {
    StreamableHttpError::Client(js_client_error(&error))
}

#[cfg(target_os = "emscripten")]
fn js_client_error(error: &JsValue) -> FetchHttpError {
    FetchHttpError::new(js_error_message(error))
}

/// JS 异常 → 文本：保留 `name` 与 `message`（网络失败、AbortError 必须可辨）。
#[cfg(target_os = "emscripten")]
fn js_error_message(error: &JsValue) -> String {
    if let Some(message) = error.as_string() {
        return message;
    }
    let field = |name: &str| Reflect::get(error, &JsValue::from_str(name)).ok();
    let name = field("name").and_then(|value| value.as_string());
    let message = field("message").and_then(|value| value.as_string());
    match (name, message) {
        (Some(name), Some(message)) => format!("{name}: {message}"),
        (Some(name), None) => name,
        (None, Some(message)) => message,
        (None, None) => format!("宿主 fetch 失败: {error:?}"),
    }
}

#[cfg(test)]
#[path = "fetch_http_test.rs"]
mod tests;
