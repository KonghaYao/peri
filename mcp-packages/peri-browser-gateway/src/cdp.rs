//! Kitesurf 的 page controller、只读 viewer 和共用 CDP helper。

use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use serde::Deserialize;
use serde_json::{json, Value};
use url::Url;

mod connection;
mod navigation;

use connection::Connection;
pub use navigation::{navigate, navigation_state, NavigationState};

use crate::input::{KeyEventKind, MouseButton, MouseEventKind};

/// Viewer attach timeout (README §10 `PERI_BROWSER_CONNECT_TIMEOUT_MS`).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound for one CDP command round trip on loopback.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
/// Event buffer between the socket reader and the screencast loop. Frames are
/// acked before the next one is produced, so this only absorbs a short stall.
pub const EVENT_CAPACITY: usize = 64;

/// Screencast defaults from README §6.4.
pub const FRAME_QUALITY: u8 = 70;
pub const FRAME_MAX_WIDTH: u32 = 1600;
pub const FRAME_MAX_HEIGHT: u32 = 1000;
/// Thumbnail capture is a still image, not a second screencast.
pub const THUMBNAIL_QUALITY: u8 = 40;

#[derive(Debug, thiserror::Error)]
pub enum CdpError {
    #[error("invalid CDP endpoint {url:?}: {reason}")]
    InvalidUrl { url: String, reason: String },
    #[error("cannot connect to {url}: {source}")]
    Connect {
        url: String,
        #[source]
        source: tokio_tungstenite::tungstenite::Error,
    },
    #[error("another controller owns this page session (HTTP 409)")]
    AnotherController,
    #[error("engine did not confirm CDP socket close")]
    UnconfirmedClose,
    #[error("CDP connection to {url} closed")]
    Closed { url: String },
    #[error("CDP command {method} timed out after {timeout_ms} ms")]
    Timeout { method: String, timeout_ms: u64 },
    #[error("CDP {code}: {message}")]
    Protocol { code: i64, message: String },
    #[error("invalid CDP payload: {0}")]
    Decode(String),
    #[error("{method} was rejected: {message}")]
    Rejected { method: String, message: String },
    #[error("CDP WebSocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
}

/// One CDP event (a message without an `id`).
#[derive(Debug, Clone)]
pub struct CdpEvent {
    pub method: String,
    pub params: Value,
}

/// A read-only page connection (`?role=viewer`).
pub struct Viewer {
    connection: Connection,
}

impl Viewer {
    /// Connects to `target_id` below `cdp_url` as a viewer.
    pub async fn connect(cdp_url: &str, target_id: &str) -> Result<Self, CdpError> {
        let url = page_viewer_url(cdp_url, target_id)?;
        Ok(Self {
            connection: Connection::open(url, CONNECT_TIMEOUT, false).await?,
        })
    }

    #[must_use]
    pub fn url(&self) -> &str {
        self.connection.url()
    }

    /// Sends one CDP command and waits for its response.
    pub async fn command(&self, method: &str, params: Value) -> Result<Value, CdpError> {
        self.connection.command(method, params).await
    }

    pub async fn close(&mut self) -> Result<(), CdpError> {
        self.connection.close().await
    }

    /// Waits for the next CDP event. Returns `None` once the socket is gone.
    pub async fn next_event(&mut self) -> Option<CdpEvent> {
        self.connection.next_event().await
    }
}

impl std::fmt::Debug for Viewer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Viewer")
            .field("url", &self.connection.url())
            .finish_non_exhaustive()
    }
}

/// A writable page connection (no `role=viewer` query).
///
/// 同一 target 的第二个 controller 会被 Kitesurf 以 HTTP 409 拒绝。
/// 网关通过 `PageControl` 将一条 writer 连接排他地交给 Agent 或 monitor。
pub struct Controller {
    connection: Connection,
}

impl Controller {
    /// Connects to `target_id` below `cdp_url` as a page controller owned by
    /// `owner`.
    ///
    /// The owner is the process's identity on the page, not a CDP concept: the
    /// MCP and the gateway pass different names so an accidental second writer is
    /// reported by Kitesurf instead of silently sharing the page.
    ///
    /// # Errors
    ///
    /// Fails for a non-WebSocket endpoint, an invalid target id or owner, a
    /// refused attach (for example `Another controller owns this page session`),
    /// and a connect timeout.
    pub async fn connect(cdp_url: &str, target_id: &str, owner: &str) -> Result<Self, CdpError> {
        Self::connect_with_timeout(cdp_url, target_id, owner, CONNECT_TIMEOUT).await
    }

    pub(crate) async fn connect_with_timeout(
        cdp_url: &str,
        target_id: &str,
        owner: &str,
        timeout: Duration,
    ) -> Result<Self, CdpError> {
        let url = page_controller_url(cdp_url, target_id, owner)?;
        Ok(Self {
            connection: Connection::open(url, timeout, true).await?,
        })
    }

    #[must_use]
    pub fn url(&self) -> &str {
        self.connection.url()
    }

    /// Sends one CDP command and waits for its response.
    pub async fn command(&self, method: &str, params: Value) -> Result<Value, CdpError> {
        self.connection.command(method, params).await
    }

    pub async fn close(&mut self) -> Result<(), CdpError> {
        self.connection.close().await
    }

    pub(crate) fn take_events(&mut self) -> tokio::sync::mpsc::Receiver<CdpEvent> {
        self.connection.take_events()
    }

    pub(crate) fn reusable(&self) -> bool {
        self.connection.reusable()
    }

    /// Waits for the next CDP event. Returns `None` once the socket is gone.
    pub async fn next_event(&mut self) -> Option<CdpEvent> {
        self.connection.next_event().await
    }
}

impl std::fmt::Debug for Controller {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Controller")
            .field("url", &self.connection.url())
            .finish_non_exhaustive()
    }
}

/// What the ownership driver needs from a page connection.
///
/// The trait exists so the state machine can be tested without a browser: the
/// real implementation is [`Controller`], tests use an in-memory recorder.
#[async_trait]
pub trait PageController: Send + Sync {
    /// Sends one CDP command and waits for its response.
    async fn command(&self, method: &str, params: Value) -> Result<Value, CdpError>;
    /// Waits for the next CDP event. Returns `None` once the socket is gone.
    async fn next_event(&mut self) -> Option<CdpEvent>;
    /// Endpoint of the underlying socket, for logs.
    fn endpoint(&self) -> &str;
}

#[async_trait]
impl PageController for Controller {
    async fn command(&self, method: &str, params: Value) -> Result<Value, CdpError> {
        Self::command(self, method, params).await
    }

    async fn next_event(&mut self) -> Option<CdpEvent> {
        Self::next_event(self).await
    }

    fn endpoint(&self) -> &str {
        self.url()
    }
}

/// Builds the read-only page endpoint below `cdp_url`.
///
/// `ws://host:port/devtools/browser` and `ws://host:port` both map to
/// `ws://host:port/devtools/page/{target}?role=viewer`.
///
/// # Errors
///
/// Fails when `cdp_url` is not a `ws://` or `wss://` URL with a host.
pub fn page_viewer_url(cdp_url: &str, target_id: &str) -> Result<String, CdpError> {
    page_url(cdp_url, target_id, "role=viewer")
}

/// Builds the writable page endpoint below `cdp_url`.
///
/// The controller URL is the same page target without `role=viewer`, tagged
/// with `owner=`. Kitesurf accepts `?owner=...` without the instance `session`
/// query (measured on 0.5.3); the page target id is the page-session key.
///
/// # Errors
///
/// Fails for a non-WebSocket URL, a missing host, or an empty/invalid target id
/// or owner.
pub fn page_controller_url(
    cdp_url: &str,
    target_id: &str,
    owner: &str,
) -> Result<String, CdpError> {
    if owner.is_empty() || !owner.chars().all(is_endpoint_token_char) {
        return Err(CdpError::InvalidUrl {
            url: cdp_url.to_owned(),
            reason: format!("invalid controller owner {owner:?}"),
        });
    }
    page_url(cdp_url, target_id, &format!("owner={owner}"))
}

fn page_url(cdp_url: &str, target_id: &str, query: &str) -> Result<String, CdpError> {
    let mut url = Url::parse(cdp_url).map_err(|error| CdpError::InvalidUrl {
        url: cdp_url.to_owned(),
        reason: error.to_string(),
    })?;
    match url.scheme() {
        "ws" | "wss" => {}
        scheme => {
            return Err(CdpError::InvalidUrl {
                url: cdp_url.to_owned(),
                reason: format!("expected ws:// or wss://, got {scheme}://"),
            });
        }
    }
    if url.host_str().is_none() {
        return Err(CdpError::InvalidUrl {
            url: cdp_url.to_owned(),
            reason: "missing host".to_owned(),
        });
    }
    if target_id.is_empty() || !target_id.chars().all(is_endpoint_token_char) {
        return Err(CdpError::InvalidUrl {
            url: cdp_url.to_owned(),
            reason: format!("invalid target id {target_id:?}"),
        });
    }
    url.set_path(&format!("/devtools/page/{target_id}"));
    url.set_query(Some(query));
    Ok(url.to_string())
}

fn is_endpoint_token_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
}

impl MouseButton {
    /// Wire name expected by `Input.dispatchMouseEvent`.
    #[must_use]
    pub fn cdp_name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Middle => "middle",
        }
    }

    /// `buttons` bitmask: Left = 1, Right = 2, Middle = 4.
    #[must_use]
    pub fn cdp_mask(self) -> u32 {
        match self {
            Self::Left => 1,
            Self::Right => 2,
            Self::Middle => 4,
        }
    }
}

impl MouseEventKind {
    #[must_use]
    pub fn cdp_name(self) -> &'static str {
        match self {
            Self::Down => "mousePressed",
            Self::Up => "mouseReleased",
            Self::Move => "mouseMoved",
        }
    }

    /// `buttons` bitmask while the event is dispatched.
    ///
    /// The wire message carries one pointer event at a time, not a held-button
    /// set, so a press reports its own button and release/move report none.
    #[must_use]
    pub fn cdp_buttons(self, button: Option<MouseButton>) -> u32 {
        match self {
            Self::Down => button.map_or(0, MouseButton::cdp_mask),
            Self::Up | Self::Move => 0,
        }
    }
}

/// Dispatches one pointer event at CSS viewport coordinates.
///
/// # Errors
///
/// Propagates the CDP error (for example a closed controller socket).
pub async fn dispatch_mouse(
    controller: &dyn PageController,
    event: MouseEventKind,
    x: f64,
    y: f64,
    button: Option<MouseButton>,
    click_count: u32,
    modifiers: u32,
) -> Result<Value, CdpError> {
    controller
        .command(
            "Input.dispatchMouseEvent",
            json!({
                "type": event.cdp_name(),
                "x": x,
                "y": y,
                "button": if event == MouseEventKind::Move {
                    "none".to_owned()
                } else {
                    button.map_or("none", MouseButton::cdp_name).to_owned()
                },
                "buttons": event.cdp_buttons(button),
                "clickCount": click_count,
                "modifiers": modifiers,
            }),
        )
        .await
}

/// Dispatches one wheel event at CSS viewport coordinates.
///
/// # Errors
///
/// Propagates the CDP error.
pub async fn dispatch_wheel(
    controller: &dyn PageController,
    x: f64,
    y: f64,
    delta_x: f64,
    delta_y: f64,
) -> Result<Value, CdpError> {
    controller
        .command(
            "Input.dispatchMouseWheel",
            json!({
                "type": "mouseWheel",
                "x": x,
                "y": y,
                "deltaX": delta_x,
                "deltaY": delta_y,
            }),
        )
        .await
}

/// Dispatches one keyboard event.
///
/// A key-down with `text` produces the character; without `text` it is a
/// `rawKeyDown`, so arrow keys and shortcuts do not insert a glyph.
///
/// # Errors
///
/// Propagates the CDP error.
pub async fn dispatch_key(
    controller: &dyn PageController,
    event: KeyEventKind,
    key: &str,
    code: &str,
    text: Option<&str>,
    modifiers: u32,
) -> Result<Value, CdpError> {
    let kind = match (event, text) {
        (KeyEventKind::Down, Some(text)) if !text.is_empty() => "keyDown",
        (KeyEventKind::Down, _) => "rawKeyDown",
        (KeyEventKind::Up, _) => "keyUp",
    };
    let mut params = json!({
        "type": kind,
        "key": key,
        "code": code,
        "modifiers": modifiers,
    });
    if let Some(virtual_key) = virtual_key_code(key, code) {
        params["windowsVirtualKeyCode"] = json!(virtual_key);
    }
    if let Some(text) = text.filter(|text| !text.is_empty()) {
        params["text"] = json!(text);
    }
    controller.command("Input.dispatchKeyEvent", params).await
}

/// Inserts text without emulating individual key events (paste, IME commit).
///
/// # Errors
///
/// Propagates the CDP error.
pub async fn insert_text(controller: &dyn PageController, text: &str) -> Result<Value, CdpError> {
    controller
        .command("Input.insertText", json!({ "text": text }))
        .await
}

/// Windows virtual-key code for the keys a browser monitor actually sends.
///
/// The wire message carries `key` and `code` but no `keyCode`; CDP accepts the
/// field being absent, so unknown keys simply omit it.
#[must_use]
pub fn virtual_key_code(key: &str, code: &str) -> Option<i64> {
    // `code` is the DOM `KeyboardEvent.code` (`KeyA`, `Digit5`, ...). A bare
    // single-character code is accepted too: it is what non-DOM clients send.
    if let Some(character) = single_char(code.strip_prefix("Key").unwrap_or(code)) {
        if character.is_ascii_alphabetic() {
            return Some(65 + i64::from(character.to_ascii_uppercase() as u8 - b'A'));
        }
        if let Some(value) = character.to_digit(10) {
            return Some(48 + i64::from(value));
        }
    }
    if let Some(digit) = code.strip_prefix("Digit") {
        if let Ok(value) = digit.parse::<i64>() {
            return (0..=9).contains(&value).then_some(48 + value);
        }
    }
    if let Some(digit) = code.strip_prefix("Numpad") {
        if let Ok(value) = digit.parse::<i64>() {
            return (0..=9).contains(&value).then_some(96 + value);
        }
    }
    let named = match key {
        "Enter" => 13,
        "Tab" => 9,
        "Backspace" => 8,
        "Escape" => 27,
        " " | "Spacebar" => 32,
        "Delete" => 46,
        "Home" => 36,
        "End" => 35,
        "PageUp" => 33,
        "PageDown" => 34,
        "ArrowLeft" => 37,
        "ArrowUp" => 38,
        "ArrowRight" => 39,
        "ArrowDown" => 40,
        "Shift" => 16,
        "Control" => 17,
        "Alt" => 18,
        "Meta" => 91,
        _ => return None,
    };
    Some(named)
}

fn single_char(value: &str) -> Option<char> {
    let mut characters = value.chars();
    match (characters.next(), characters.next()) {
        (Some(character), None) => Some(character),
        _ => None,
    }
}

/// Address, history position and title of the controller's page.
/// Enables the diagnostics domains the monitor reproduces.
///
/// `Runtime.enable`, `Network.enable` and `Page.enable` are rejected on a viewer
/// connection; this is the first thing the driver does after taking the
/// controller. `Page.enable` is what makes `Page.frameNavigated` arrive, so the
/// address bar follows links the page opens on its own.
///
/// # Errors
///
/// Fails when a domain cannot be enabled.
pub async fn enable_diagnostics(controller: &dyn PageController) -> Result<(), CdpError> {
    controller.command("Page.enable", json!({})).await?;
    controller.command("Runtime.enable", json!({})).await?;
    controller.command("Network.enable", json!({})).await?;
    Ok(())
}

/// CSS viewport of the controller's page, from `Page.getLayoutMetrics`.
///
/// # Errors
///
/// Propagates the CDP error; an unparsable result is `Ok(None)`.
pub async fn layout_metrics(controller: &dyn PageController) -> Result<Option<Viewport>, CdpError> {
    let value = controller
        .command("Page.getLayoutMetrics", json!({}))
        .await?;
    Ok(viewport_from_metrics(&value))
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreencastMetadata {
    #[serde(default)]
    pub device_width: u32,
    #[serde(default)]
    pub device_height: u32,
    #[serde(default = "one")]
    pub page_scale_factor: f64,
    #[serde(default)]
    pub scroll_offset_x: f64,
    #[serde(default)]
    pub scroll_offset_y: f64,
}

fn one() -> f64 {
    1.0
}

/// One decoded `Page.screencastFrame`.
#[derive(Debug, Clone)]
pub struct ScreencastFrame {
    pub jpeg: Bytes,
    pub metadata: ScreencastMetadata,
    /// Opaque frame session id echoed back in `Page.screencastFrameAck`.
    pub session_id: Value,
}

impl ScreencastFrame {
    /// Decodes the base64 `data` field and its metadata.
    ///
    /// # Errors
    ///
    /// Fails when the payload lacks `data`/`metadata` or the base64 is invalid.
    pub fn parse(params: &Value) -> Result<Self, CdpError> {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine as _;

        let data = params
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(|| CdpError::Decode("screencast frame without data".to_owned()))?;
        let jpeg = STANDARD.decode(data).map_err(|error| {
            CdpError::Decode(format!("screencast frame is not base64: {error}"))
        })?;
        let metadata = params
            .get("metadata")
            .cloned()
            .ok_or_else(|| CdpError::Decode("screencast frame without metadata".to_owned()))
            .and_then(|value| {
                serde_json::from_value(value)
                    .map_err(|error| CdpError::Decode(format!("bad screencast metadata: {error}")))
            })?;
        Ok(Self {
            jpeg: Bytes::from(jpeg),
            metadata,
            session_id: params.get("sessionId").cloned().unwrap_or(Value::Null),
        })
    }
}

/// CSS viewport size from `Page.getLayoutMetrics`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LayoutViewport {
    #[serde(default)]
    client_width: u32,
    #[serde(default)]
    client_height: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LayoutMetrics {
    #[serde(default)]
    css_layout_viewport: Option<LayoutViewport>,
    #[serde(default)]
    layout_viewport: Option<LayoutViewport>,
}

/// Reads the viewport from a `Page.getLayoutMetrics` result.
///
/// `cssLayoutViewport` is authoritative when present; older engines only send
/// `layoutViewport`.
#[must_use]
pub fn viewport_from_metrics(value: &Value) -> Option<Viewport> {
    let metrics: LayoutMetrics = serde_json::from_value(value.clone()).ok()?;
    let viewport = metrics.css_layout_viewport.or(metrics.layout_viewport)?;
    if viewport.client_width == 0 || viewport.client_height == 0 {
        return None;
    }
    Some(Viewport {
        width: viewport.client_width,
        height: viewport.client_height,
    })
}

/// Starts a screencast with the gateway defaults (JPEG q70, 1600×1000 bound).
pub async fn start_screencast(viewer: &Viewer) -> Result<(), CdpError> {
    viewer
        .command(
            "Page.startScreencast",
            json!({
                "format": "jpeg",
                "quality": FRAME_QUALITY,
                "maxWidth": FRAME_MAX_WIDTH,
                "maxHeight": FRAME_MAX_HEIGHT,
                "everyNthFrame": 1,
            }),
        )
        .await?;
    Ok(())
}

/// Captures one JPEG as base64 for a tab thumbnail.
///
/// The scale is native (`scale: 1`): a scaled capture leaves a retained capture
/// surface on the page and can shrink the next screencast.
pub async fn capture_thumbnail(cdp_url: &str, target_id: &str) -> Result<String, CdpError> {
    let mut viewer = Viewer::connect(cdp_url, target_id).await?;
    let captured = thumbnail(&viewer).await;
    let closed = viewer.close().await;
    captured.and_then(|data| closed.map(|()| data))
}

pub(crate) async fn thumbnail(viewer: &Viewer) -> Result<String, CdpError> {
    let result = viewer
        .command(
            "Page.captureScreenshot",
            json!({"format": "jpeg", "quality": THUMBNAIL_QUALITY}),
        )
        .await?;
    result
        .get("data")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| CdpError::Decode("captureScreenshot without data".to_owned()))
}

#[cfg(test)]
#[path = "cdp_test.rs"]
mod tests;
