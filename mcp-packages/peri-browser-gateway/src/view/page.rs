//! view session 的页面驱动：真实 CDP 页面、真实输入、screencast 与宿主中继。
//!
//! 两个 CDP 连接：writer controller 负责输入、求值和页面队列 drain；只读
//! viewer 负责 screencast。两者都只作用于本会话创建的 target，不进入 MCP
//! 会话的页面集合。

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{EngineHandle, ViewError, ViewSession};
use crate::cdp::{self, CdpEvent, Controller, PageController, Viewer};
use crate::input::{MouseButton, MouseEventKind};
use crate::provision::{close_target, create_target};

/// 页面导航到 `readyState complete` 的上限；与 `PERI_BROWSER_PAGE_WAIT_MS` 取小。
const NAV_WAIT: Duration = Duration::from_secs(15);
const READY_POLL: Duration = Duration::from_millis(100);
const RECONNECT_DELAY: Duration = Duration::from_millis(500);
const PENDING_CLOSE: Duration = Duration::from_secs(5);
/// 页面 console 唤醒前缀；与 [`crate::apps`] 的 bridge 保持一致。
const WAKE_PREFIX: &str = "peri-view:app-request";
/// console 暂停（例如 Runtime.enable 未被引擎支持）时的兜底 drain 周期。
const REQUEST_POLL: Duration = Duration::from_secs(1);

/// 本会话唯一的页面 writer；输入、求值和回填都经它下发。
pub(crate) struct PageDriver {
    target_id: String,
    controller: Arc<Controller>,
}

impl PageDriver {
    pub fn target_id(&self) -> &str {
        &self.target_id
    }

    /// 真实点击：`mouseMoved` → `mousePressed` → `mouseReleased`。
    pub async fn click(&self, x: f64, y: f64) -> Result<(), ViewError> {
        let controller: &dyn PageController = self.controller.as_ref();
        cdp::dispatch_mouse(controller, MouseEventKind::Move, x, y, None, 0, 0).await?;
        cdp::dispatch_mouse(
            controller,
            MouseEventKind::Down,
            x,
            y,
            Some(MouseButton::Left),
            1,
            0,
        )
        .await?;
        cdp::dispatch_mouse(
            controller,
            MouseEventKind::Up,
            x,
            y,
            Some(MouseButton::Left),
            1,
            0,
        )
        .await?;
        Ok(())
    }

    pub async fn move_to(&self, x: f64, y: f64) -> Result<(), ViewError> {
        let controller: &dyn PageController = self.controller.as_ref();
        cdp::dispatch_mouse(controller, MouseEventKind::Move, x, y, None, 0, 0).await?;
        Ok(())
    }

    pub async fn scroll(
        &self,
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
    ) -> Result<(), ViewError> {
        let controller: &dyn PageController = self.controller.as_ref();
        cdp::dispatch_wheel(controller, x, y, delta_x, delta_y).await?;
        Ok(())
    }

    /// Evaluates an expression and returns its by-value result.
    pub async fn evaluate(&self, expression: &str) -> Result<Value, ViewError> {
        let reply = self
            .controller
            .command(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "awaitPromise": true,
                    "returnByValue": true,
                    "userGesture": true,
                }),
            )
            .await?;
        if let Some(exception) = reply.get("exceptionDetails") {
            return Err(ViewError::bad_request(
                "script_failed",
                exception_message(exception),
            ));
        }
        Ok(reply
            .get("result")
            .and_then(|result| result.get("value"))
            .cloned()
            .unwrap_or(Value::Null))
    }

    /// Delivers one JSON-RPC result/error to the App bridge.
    ///
    /// The message must stay pure JSON-RPC: the SDK's response schema is
    /// strict, so an extra marker field makes the App drop it.
    pub async fn host_reply(&self, message: Value) -> Result<(), ViewError> {
        let expression = format!(
            "window.__bridge && typeof window.__bridge.hostReply === 'function' \
             ? window.__bridge.hostReply({message}) : false"
        );
        if self.evaluate(&expression).await? != Value::Bool(true) {
            return Err(ViewError::new(
                StatusCode::BAD_GATEWAY,
                "bridge_unavailable",
                "the page has no host bridge; the app may have navigated away",
            ));
        }
        Ok(())
    }

    /// Delivers one host notification (no `id`, no response expected).
    pub async fn host_notify(&self, method: &str, params: Value) -> Result<(), ViewError> {
        let expression = format!(
            "window.__bridge && typeof window.__bridge.hostNotify === 'function' \
             ? window.__bridge.hostNotify({}, {params}) : false",
            Value::String(method.to_owned())
        );
        if self.evaluate(&expression).await? != Value::Bool(true) {
            return Err(ViewError::new(
                StatusCode::BAD_GATEWAY,
                "bridge_unavailable",
                "the page has no host bridge; the app may have navigated away",
            ));
        }
        Ok(())
    }
}

impl From<cdp::CdpError> for ViewError {
    fn from(error: cdp::CdpError) -> Self {
        Self::upstream("cdp_failed", error)
    }
}

/// Drives one view session from page creation to teardown.
pub(crate) async fn run(session: Arc<ViewSession>, engine: Arc<EngineHandle>) {
    let stop = session.stop().clone();
    let started = tokio::select! {
        biased;
        _ = stop.cancelled() => None,
        value = start(&session, &engine) => Some(value),
    };
    if let Some(result) = started {
        match result {
            Ok((driver, events)) => {
                session.ready(driver.clone());
                let screencast = tokio::spawn(screencast(
                    session.clone(),
                    driver.target_id().to_owned(),
                    engine.cdp_url.clone(),
                ));
                tokio::select! {
                    biased;
                    _ = stop.cancelled() => {}
                    () = console_pump(&session, &driver, events) => {}
                }
                stop.cancel();
                let _ = tokio::time::timeout(PENDING_CLOSE, screencast).await;
            }
            Err(error) => {
                session.note_reason("start_failed");
                session.fail(error.code, error.message);
                session.stop().cancel();
            }
        }
    }
    teardown(&session, &engine).await;
}

/// Creates the target, attaches the writer, enables diagnostics and navigates.
async fn start(
    session: &Arc<ViewSession>,
    engine: &EngineHandle,
) -> Result<(Arc<PageDriver>, mpsc::Receiver<CdpEvent>), ViewError> {
    let target = create_target(&engine.provision)
        .await
        .map_err(|error| ViewError::upstream("create_target_failed", error))?;
    session.set_target(&target);
    if session.stop().is_cancelled() {
        return Err(ViewError::new(
            StatusCode::CONFLICT,
            "session_closed",
            "the view session was closed while starting",
        ));
    }
    let mut controller = Controller::connect(&engine.cdp_url, &target, &engine.owner)
        .await
        .map_err(|error| ViewError::upstream("attach_failed", error))?;
    let events = controller.take_events();
    let controller = Arc::new(controller);
    // console 事件只是唤醒信号；引擎拒绝该域时仍能靠周期 drain 工作。
    if let Err(error) = controller.command("Runtime.enable", json!({})).await {
        tracing::debug!(%error, "view session: Runtime.enable failed; relay falls back to polling");
    }
    navigate_ready(&controller, session.url(), engine.page_wait).await?;
    Ok((
        Arc::new(PageDriver {
            target_id: target,
            controller,
        }),
        events,
    ))
}

async fn navigate_ready(
    controller: &Arc<Controller>,
    url: &str,
    page_wait: Duration,
) -> Result<(), ViewError> {
    cdp::navigate(controller.as_ref(), url)
        .await
        .map_err(|error| ViewError::upstream("navigate_failed", error))?;
    let deadline = tokio::time::Instant::now() + page_wait.min(NAV_WAIT);
    loop {
        let ready = controller
            .command(
                "Runtime.evaluate",
                json!({"expression": "document.readyState", "returnByValue": true}),
            )
            .await
            .ok()
            .and_then(|reply| {
                reply
                    .get("result")
                    .and_then(|result| result.get("value"))
                    .and_then(Value::as_str)
                    .map(|value| value == "complete")
            })
            .unwrap_or(false);
        if ready {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(ViewError::upstream(
                "ready_timeout",
                "the app view did not reach readyState complete",
            ));
        }
        tokio::time::sleep(READY_POLL).await;
    }
}

/// 只读 viewer 上的 screencast 循环；socket 断开或引擎重启时重连。
async fn screencast(session: Arc<ViewSession>, target: String, cdp_url: String) {
    let stop = session.stop().clone();
    while !stop.is_cancelled() {
        let attached = tokio::select! {
            biased;
            _ = stop.cancelled() => return,
            value = Viewer::connect(&cdp_url, &target) => value,
        };
        let Ok(mut viewer) = attached else {
            if wait_or_stop(&stop, RECONNECT_DELAY).await {
                return;
            }
            continue;
        };
        if cdp::start_screencast(&viewer).await.is_err() {
            let _ = viewer.close().await;
            if wait_or_stop(&stop, RECONNECT_DELAY).await {
                return;
            }
            continue;
        }
        // 必须在 `start_screencast` 之后读取：`maxWidth`/`maxHeight` 会同步把
        // 布局视口缩放到帧尺寸（实测 1280x720 → 1600x1000），启动前读到的
        // CSS 视口会立刻过期，宿主按它换算的点击坐标整体偏移。
        let viewport = viewer
            .command("Page.getLayoutMetrics", json!({}))
            .await
            .ok()
            .and_then(|value| cdp::viewport_from_metrics(&value));
        loop {
            let event = tokio::select! {
                biased;
                _ = stop.cancelled() => {
                    let _ = viewer.close().await;
                    return;
                }
                value = viewer.next_event() => value,
            };
            let Some(event) = event else { break };
            if event.method != "Page.screencastFrame" {
                continue;
            }
            let Ok(frame) = cdp::ScreencastFrame::parse(&event.params) else {
                continue;
            };
            let ack = viewer
                .command(
                    "Page.screencastFrameAck",
                    json!({ "sessionId": frame.session_id }),
                )
                .await;
            if ack.is_err() {
                break;
            }
            session.record_frame(
                frame.jpeg,
                frame.metadata.device_width.max(1),
                frame.metadata.device_height.max(1),
                viewport,
            );
        }
        let _ = viewer.close().await;
    }
}

async fn wait_or_stop(stop: &CancellationToken, wait: Duration) -> bool {
    tokio::select! {
        biased;
        _ = stop.cancelled() => true,
        _ = tokio::time::sleep(wait) => false,
    }
}

/// writer 连接上的事件泵：console 唤醒 → drain App 请求；周期兜底 drain。
async fn console_pump(
    session: &Arc<ViewSession>,
    driver: &PageDriver,
    mut events: mpsc::Receiver<CdpEvent>,
) {
    let stop = session.stop().clone();
    let mut poll = tokio::time::interval(REQUEST_POLL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            _ = stop.cancelled() => return,
            event = events.recv() => match event {
                None => {
                    session.note_reason("controller_closed");
                    session.fail("controller_closed", "the page controller socket closed");
                    session.stop().cancel();
                    return;
                }
                Some(event) => {
                    if event.method == "Peri.targetExpired" {
                        session.note_reason("target_expired");
                        session.fail("target_expired", "the page target was closed by the engine");
                        session.stop().cancel();
                        return;
                    }
                    if is_wake(&event) {
                        drain(session, driver).await;
                    }
                }
            },
            _ = poll.tick() => drain(session, driver).await,
        }
    }
}

fn is_wake(event: &CdpEvent) -> bool {
    event.method == "Runtime.consoleAPICalled"
        && event
            .params
            .get("args")
            .and_then(Value::as_array)
            .and_then(|args| args.first())
            .and_then(|arg| arg.get("value"))
            .and_then(Value::as_str)
            .is_some_and(|value| value.starts_with(WAKE_PREFIX))
}

/// Takes the App's queued `tools/call` requests from the page and queues them
/// for the host. The page queue is drained (returned and cleared) so a request
/// is never queued twice.
async fn drain(session: &Arc<ViewSession>, driver: &PageDriver) {
    let expression = "window.__bridge && typeof window.__bridge.takeRequests === 'function' \
         ? window.__bridge.takeRequests() : []";
    let Ok(value) = driver.evaluate(expression).await else {
        return;
    };
    let Some(items) = value.as_array() else {
        return;
    };
    for item in items {
        let app_id = item.get("id").cloned().unwrap_or(Value::Null);
        let method = item
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("tools/call")
            .to_owned();
        let params = item.get("params").cloned().unwrap_or(Value::Null);
        if session.enqueue(app_id.clone(), method, params).is_none() {
            let error = json!({
                "jsonrpc": "2.0",
                "id": app_id,
                "error": { "code": -32603, "message": "host request queue is full" },
            });
            let _ = driver.host_reply(error).await;
        }
    }
}

async fn teardown(session: &Arc<ViewSession>, engine: &EngineHandle) {
    session.stop().cancel();
    if let Some(target) = session.take_target() {
        match tokio::time::timeout(PENDING_CLOSE, close_target(&engine.provision, &target)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(session = session.id(), %error, "view page target was not closed");
            }
            Err(_) => {
                tracing::warn!(session = session.id(), "view page target close timed out");
            }
        }
    }
    session.documents().remove_view(session.id());
    session.finish();
}

fn exception_message(exception: &Value) -> String {
    exception
        .get("exception")
        .and_then(|value| value.get("description"))
        .and_then(Value::as_str)
        .or_else(|| exception.get("text").and_then(Value::as_str))
        .unwrap_or("script evaluation failed")
        .to_owned()
}
