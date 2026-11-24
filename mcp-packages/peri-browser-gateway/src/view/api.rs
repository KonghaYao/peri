//! `/view/sessions*` 的 HTTP 面：token 鉴权、创建/查询/关闭、SSE 帧与请求流、
//! 真实 CDP 输入、求值与 `tools/call` 结果回填。
//!
//! 路由挂在主 listener（`/mcp` 同一个），文档源只提供 HTML；所有方法先过
//! [`guard`]，缺少 token 返回 401、token 不匹配返回 403、未配置返回 403。

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Extension, Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{AppRequest, Failure, FrameView, Phase, ViewError, ViewSession, ViewSessions};
use crate::apps::MAX_APP_HTML_BYTES;
use crate::session::SessionDirectory;
use crate::token::constant_time_eq;

/// SSE 请求重放的兜底周期：Notify 丢失时仍能送出未回答的请求。
const REPLAY_INTERVAL: Duration = Duration::from_secs(2);
/// `POST /evaluate` 的表达式上限，避免把控制面当作大文件通道。
const MAX_EXPRESSION_BYTES: usize = 1024 * 1024;

/// `/view/sessions*` 路由；鉴权只覆盖这组路径。
pub(crate) fn router(views: Arc<ViewSessions>) -> Router<Arc<SessionDirectory>> {
    Router::new()
        .route("/view/sessions", get(list).post(create))
        .route("/view/sessions/{id}", get(status).delete(delete))
        .route("/view/sessions/{id}/events", get(events))
        .route("/view/sessions/{id}/click", post(click))
        .route("/view/sessions/{id}/evaluate", post(evaluate))
        .route("/view/sessions/{id}/app-results", post(app_result))
        .route("/view/sessions/{id}/notify", post(notify))
        // route_layer 只覆盖匹配到的路由：不能让鉴权落到 router fallback
        // （例如 `/mcp/` 这类未知路径），否则 404 会变成 403。
        .route_layer(middleware::from_fn_with_state(views.clone(), guard))
        // axum 默认 2 MiB；App HTML 的上限是 5 MiB，超出仍由 handler 回 413。
        .route_layer(DefaultBodyLimit::max(MAX_APP_HTML_BYTES + 1))
        .route_layer(Extension(views))
}

/// 鉴权中间件：bearer token（`Authorization: Bearer …` 或
/// `X-Peri-View-Token`）。token 只来自配置和环境变量，不回显、不写日志。
pub(crate) async fn guard(
    State(views): State<Arc<ViewSessions>>,
    request: Request,
    next: Next,
) -> Response {
    match authorize(&views, request.headers()) {
        Ok(()) => next.run(request).await,
        Err(error) => error.into_response(),
    }
}

fn authorize(views: &ViewSessions, headers: &HeaderMap) -> Result<(), ViewError> {
    let Some(expected) = views.token() else {
        return Err(ViewError::disabled());
    };
    let Some(candidate) = bearer(headers) else {
        return Err(ViewError::new(
            StatusCode::UNAUTHORIZED,
            "token_required",
            "view api requires Authorization: Bearer <PERI_BROWSER_VIEW_TOKEN>",
        ));
    };
    if constant_time_eq(expected.as_bytes(), candidate.as_bytes()) {
        Ok(())
    } else {
        Err(ViewError::new(
            StatusCode::FORBIDDEN,
            "invalid_token",
            "the view token does not match this gateway",
        ))
    }
}

pub(super) fn bearer(headers: &HeaderMap) -> Option<&str> {
    if let Some(value) = headers
        .get("x-peri-view-token")
        .and_then(|value| value.to_str().ok())
    {
        let value = value.trim();
        if !value.is_empty() {
            return Some(value);
        }
    }
    let value = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .trim();
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    (!token.is_empty()).then_some(token)
}

/// `POST /view/sessions`：请求体是 App HTML（≤5 MiB，UTF-8）。
async fn create(
    Extension(views): Extension<Arc<ViewSessions>>,
    body: Bytes,
) -> Result<Response, ViewError> {
    if body.len() > MAX_APP_HTML_BYTES {
        return Err(ViewError::too_large());
    }
    let created = views.create(&body)?;
    Ok((StatusCode::CREATED, Json(created)).into_response())
}

/// `GET /view/sessions`：当前记录（含失败会话），便于宿主与排障。
async fn list(Extension(views): Extension<Arc<ViewSessions>>) -> Response {
    Json(json!({ "sessions": views.list() })).into_response()
}

/// `GET /view/sessions/{id}`：状态、帧序号、订阅者与未回答请求数。
async fn status(
    Extension(views): Extension<Arc<ViewSessions>>,
    Path(id): Path<String>,
) -> Result<Response, ViewError> {
    views.status(&id).map(|value| Json(value).into_response())
}

/// `DELETE /view/sessions/{id}`：取消任务、关闭页面 target、移除文档。
async fn delete(
    Extension(views): Extension<Arc<ViewSessions>>,
    Path(id): Path<String>,
) -> Result<Response, ViewError> {
    views
        .delete(&id)
        .await
        .map(|value| Json(value).into_response())
}

/// `GET /view/sessions/{id}/events`：SSE，事件 `meta`/`state`/`frame`/
/// `app-request`/`closed`。
async fn events(
    Extension(views): Extension<Arc<ViewSessions>>,
    Path(id): Path<String>,
) -> Result<Response, ViewError> {
    let session = views.session(&id)?;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(32);
    tokio::spawn(pump(session, tx));
    let stream = futures::stream::poll_fn(move |cx| rx.poll_recv(cx)).map(Ok::<_, Infallible>);
    Ok(Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keepalive"),
        )
        .into_response())
}

async fn pump(session: Arc<ViewSession>, tx: tokio::sync::mpsc::Sender<Event>) {
    session.subscriber_joined();
    let _guard = SubscriberGuard(session.clone());
    let mut frame_rx = session.subscribe_frames();
    let mut phase_rx = session.subscribe_phase();
    let mut replay = tokio::time::interval(REPLAY_INTERVAL);
    replay.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut cursor = 0_u64;
    let mut open = send_event(&tx, "meta", &session.status()).await;
    if open {
        let snapshot = frame_rx.borrow_and_update().clone();
        if let Some(frame) = snapshot {
            open = send_frame(&tx, &frame).await;
        }
    }
    while open {
        for request in session.pending_after(cursor) {
            cursor = request.seq;
            if !send_request(&tx, &request).await {
                return;
            }
        }
        tokio::select! {
            biased;
            _ = session.stop().cancelled() => break,
            changed = frame_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                let snapshot = frame_rx.borrow_and_update().clone();
                if let Some(frame) = snapshot {
                    open = send_frame(&tx, &frame).await;
                }
            }
            changed = phase_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                let phase = phase_rx.borrow_and_update().clone();
                match phase {
                    Phase::Closed => break,
                    Phase::Failed(failure) => {
                        open = send_error(&tx, &failure).await;
                    }
                    Phase::Starting | Phase::Ready(_) => {
                        open = send_event(&tx, "state", &session.status()).await;
                    }
                }
            }
            _ = session.wake().notified() => {}
            _ = replay.tick() => {}
        }
    }
    let _ = send_event(&tx, "closed", &json!({ "reason": session.closed_reason() })).await;
}

async fn send_event(
    tx: &tokio::sync::mpsc::Sender<Event>,
    name: &'static str,
    data: &Value,
) -> bool {
    tx.send(Event::default().event(name).data(data.to_string()))
        .await
        .is_ok()
}

async fn send_error(tx: &tokio::sync::mpsc::Sender<Event>, failure: &Failure) -> bool {
    send_event(
        tx,
        "state",
        &json!({
            "state": "failed",
            "code": failure.code,
            "message": failure.message,
        }),
    )
    .await
}

async fn send_request(tx: &tokio::sync::mpsc::Sender<Event>, request: &AppRequest) -> bool {
    send_event(tx, "app-request", &request.to_json()).await
}

async fn send_frame(tx: &tokio::sync::mpsc::Sender<Event>, frame: &FrameView) -> bool {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;

    let data = json!({
        "seq": frame.seq,
        "width": frame.width,
        "height": frame.height,
        "viewportWidth": frame.viewport_width,
        "viewportHeight": frame.viewport_height,
        "format": "jpeg",
        "data": STANDARD.encode(&frame.jpeg),
    });
    send_event(tx, "frame", &data).await
}

struct SubscriberGuard(Arc<ViewSession>);

impl Drop for SubscriberGuard {
    fn drop(&mut self) {
        self.0.subscriber_left();
        if self.0.subscriber_count() == 0 {
            self.0.wake().notify_waiters();
        }
    }
}

// ---------------------------------------------------------------------------
// Input / evaluate / relay
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ClickBody {
    /// `click`（默认）、`move` 或 `scroll`；`x`/`y` 始终是 CSS 视口坐标。
    #[serde(default)]
    pub(super) action: Option<String>,
    pub(super) x: f64,
    pub(super) y: f64,
    #[serde(default)]
    pub(super) delta_x: f64,
    #[serde(default)]
    pub(super) delta_y: f64,
}

impl ClickBody {
    pub(super) fn point(&self) -> Result<(f64, f64), ViewError> {
        if !self.x.is_finite() || !self.y.is_finite() {
            return Err(ViewError::bad_request(
                "invalid_point",
                "x and y must be finite numbers",
            ));
        }
        if self.x.abs() > 100_000.0 || self.y.abs() > 100_000.0 {
            return Err(ViewError::bad_request(
                "invalid_point",
                "x and y must stay within the viewport range",
            ));
        }
        Ok((self.x, self.y))
    }
}

/// `POST /view/sessions/{id}/click`：真实 CDP 输入（move → press → release）。
async fn click(
    Extension(views): Extension<Arc<ViewSessions>>,
    Path(id): Path<String>,
    Json(body): Json<ClickBody>,
) -> Result<Response, ViewError> {
    let session = views.session(&id)?;
    let driver = session.driver()?;
    let (x, y) = body.point()?;
    let result = match body.action.as_deref().unwrap_or("click") {
        "click" => {
            driver.click(x, y).await?;
            json!({ "clicked": true, "x": x, "y": y, "sessionId": session.id() })
        }
        "move" => {
            driver.move_to(x, y).await?;
            json!({ "moved": true, "x": x, "y": y, "sessionId": session.id() })
        }
        "scroll" => {
            driver.scroll(x, y, body.delta_x, body.delta_y).await?;
            json!({ "scrolled": true, "x": x, "y": y, "sessionId": session.id() })
        }
        other => {
            return Err(ViewError::bad_request(
                "unknown_action",
                format!("action must be click, move or scroll; got {other:?}"),
            ))
        }
    };
    Ok(Json(result).into_response())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvaluateBody {
    expression: String,
}

/// `POST /view/sessions/{id}/evaluate`：宿主读取页面状态（例如定位点击目标）。
///
/// 与宿主注入 App HTML 同属一个信任域：页面在 loopback 文档源上，CSP 禁止
/// 网络，也没有任何凭据。
async fn evaluate(
    Extension(views): Extension<Arc<ViewSessions>>,
    Path(id): Path<String>,
    Json(body): Json<EvaluateBody>,
) -> Result<Response, ViewError> {
    if body.expression.len() > MAX_EXPRESSION_BYTES {
        return Err(ViewError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "expression_too_large",
            format!("expression must be at most {MAX_EXPRESSION_BYTES} bytes"),
        ));
    }
    let session = views.session(&id)?;
    let driver = session.driver()?;
    let result = driver.evaluate(&body.expression).await?;
    Ok(Json(json!({ "result": result, "sessionId": session.id() })).into_response())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorBody {
    code: i64,
    message: String,
    #[serde(default)]
    data: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppResultBody {
    /// 网关在 `app-request` 事件里给出的序号。
    seq: u64,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<ErrorBody>,
}

/// `POST /view/sessions/{id}/app-results`：宿主回填 `tools/call` 结果。
async fn app_result(
    Extension(views): Extension<Arc<ViewSessions>>,
    Path(id): Path<String>,
    Json(body): Json<AppResultBody>,
) -> Result<Response, ViewError> {
    if body.result.is_some() == body.error.is_some() {
        return Err(ViewError::bad_request(
            "invalid_result",
            "send exactly one of result or error",
        ));
    }
    let session = views.session(&id)?;
    let driver = session.driver()?;
    let request = session.answer(body.seq)?;
    let mut message = json!({ "jsonrpc": "2.0", "id": request.app_id });
    if let Some(result) = body.result {
        message["result"] = result;
    } else if let Some(error) = body.error {
        let mut value = json!({ "code": error.code, "message": error.message });
        if let Some(data) = error.data {
            value["data"] = data;
        }
        message["error"] = value;
    }
    driver.host_reply(message).await?;
    Ok(
        Json(json!({ "delivered": true, "seq": body.seq, "sessionId": session.id() }))
            .into_response(),
    )
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NotifyBody {
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

/// `POST /view/sessions/{id}/notify`：宿主主动给 App 发通知（例如
/// `ui/notifications/tool-input` / `tool-result`）。
async fn notify(
    Extension(views): Extension<Arc<ViewSessions>>,
    Path(id): Path<String>,
    Json(body): Json<NotifyBody>,
) -> Result<Response, ViewError> {
    let session = views.session(&id)?;
    let driver = session.driver()?;
    driver
        .host_notify(&body.method, body.params.unwrap_or(Value::Null))
        .await?;
    Ok(Json(json!({
        "sent": true,
        "method": body.method,
        "sessionId": session.id(),
    }))
    .into_response())
}

impl IntoResponse for ViewError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({ "error": self.code, "message": self.message })),
        )
            .into_response()
    }
}
