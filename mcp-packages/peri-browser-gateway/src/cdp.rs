//! CDP page connections: one read-only viewer, one writable controller.
//!
//! Both connection kinds ride the same `/devtools/page/{targetId}` WebSocket and
//! share the same pending-command/event plumbing in [`Connection`]. They differ
//! in the query Kitesurf sees:
//!
//! - [`Viewer`] appends `?role=viewer`, so Kitesurf itself rejects navigation,
//!   input and `Runtime.enable` (`Viewer connection cannot control the browser`).
//!   This is the connection that carries frames (README §6.4).
//! - [`Controller`] appends `?owner=peri-browser-gateway` and no `role`: it is a
//!   page controller, the only writer of the page while the human holds it.
//!   Measured on Kitesurf 0.5.3 (throwaway instance): a second controller on the
//!   same target is refused with `HTTP 409 {"error":"Another controller owns this
//!   page session"}`, a different owner can attach once the previous controller
//!   disconnects, and `?owner=...` works without the instance `session` query.
//!
//! One connection owns one WebSocket: a writer task serialises commands and a
//! reader task resolves responses by id and forwards events. Dropping the
//! connection aborts both tasks, which closes the socket and (for a viewer) the
//! Kitesurf-side screencast.
//!
//! Typed helpers ([`dispatch_mouse`], [`dispatch_key`], [`navigate`], ...) are
//! free functions over [`PageController`] so the ownership driver can run its
//! state machine against a fake controller in unit tests while the real
//! connection stays a thin transport.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

use crate::protocol::{KeyEventKind, MouseButton, MouseEventKind};

/// Viewer attach timeout (README §10 `PERI_BROWSER_CONNECT_TIMEOUT_MS`).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound for one CDP command round trip on loopback.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
/// Event buffer between the socket reader and the screencast loop. Frames are
/// acked before the next one is produced, so this only absorbs a short stall.
pub const EVENT_CAPACITY: usize = 64;

/// Screencast defaults from README §6.4.
pub const FRAME_QUALITY: u8 = 55;
pub const FRAME_MAX_WIDTH: u32 = 1600;
pub const FRAME_MAX_HEIGHT: u32 = 1000;
/// Thumbnail capture is a still image, not a second screencast.
pub const THUMBNAIL_QUALITY: u8 = 40;

/// Owner name the gateway uses for its writable page connection.
///
/// It is deliberately distinct from the MCP's owner: the two never hold the same
/// page at once, and a distinct name makes a mistaken double attach a visible
/// Kitesurf error instead of a silent second writer.
pub const CONTROLLER_OWNER: &str = "peri-browser-gateway";

/// How often `history_step` re-reads the index after `navigateToHistoryEntry`.
const HISTORY_RETRIES: usize = 4;
const HISTORY_RETRY_DELAY: Duration = Duration::from_millis(50);

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
    #[error("CDP connection to {url} closed")]
    Closed { url: String },
    #[error("CDP command {method} timed out after {timeout_ms} ms")]
    Timeout { method: String, timeout_ms: u64 },
    #[error("CDP {code}: {message}")]
    Protocol { code: i64, message: String },
    #[error("invalid CDP payload: {0}")]
    Decode(String),
    #[error("CDP WebSocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
}

/// One CDP event (a message without an `id`).
#[derive(Debug, Clone)]
pub struct CdpEvent {
    pub method: String,
    pub params: Value,
}

struct Command {
    id: u64,
    method: String,
    params: Value,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, CdpError>>>>>;

/// One page-scoped CDP WebSocket.
struct Connection {
    url: String,
    commands: mpsc::UnboundedSender<Command>,
    events: mpsc::Receiver<CdpEvent>,
    pending: Pending,
    next_id: AtomicU64,
    writer: JoinHandle<()>,
    reader: JoinHandle<()>,
}

impl Connection {
    async fn open(url: String) -> Result<Self, CdpError> {
        let (socket, _response) = tokio::time::timeout(CONNECT_TIMEOUT, {
            let url = url.clone();
            async move { tokio_tungstenite::connect_async(url).await }
        })
        .await
        .map_err(|_| CdpError::Timeout {
            method: "WebSocket connect".to_owned(),
            timeout_ms: CONNECT_TIMEOUT.as_millis() as u64,
        })?
        .map_err(|source| CdpError::Connect {
            url: url.clone(),
            source,
        })?;

        let (mut sink, mut stream) = socket.split();
        let (command_tx, mut command_rx) = mpsc::unbounded_channel::<Command>();
        let (event_tx, event_rx) = mpsc::channel(EVENT_CAPACITY);
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));

        let writer_pending = pending.clone();
        let writer = tokio::spawn(async move {
            while let Some(command) = command_rx.recv().await {
                let payload = json!({
                    "id": command.id,
                    "method": command.method,
                    "params": command.params,
                });
                let text = match serde_json::to_string(&payload) {
                    Ok(text) => text,
                    Err(error) => {
                        // A `Value` always serialises; keep the pending entry
                        // bounded by failing the whole connection if it does not.
                        tracing::error!(error = %error, "cannot encode a CDP command");
                        fail_pending(&writer_pending, &CdpError::Decode(error.to_string()));
                        break;
                    }
                };
                if let Err(error) = sink.send(Message::Text(text.into())).await {
                    fail_pending(&writer_pending, &CdpError::WebSocket(error));
                    break;
                }
            }
            let _ = sink.close().await;
        });

        let reader_pending = pending.clone();
        let reader_url = url.clone();
        let reader = tokio::spawn(async move {
            while let Some(message) = stream.next().await {
                match message {
                    Ok(Message::Text(text)) => {
                        if !dispatch(text.as_str(), &reader_pending, &event_tx).await {
                            break;
                        }
                    }
                    Ok(Message::Close(_)) => break,
                    Ok(
                        Message::Binary(_)
                        | Message::Ping(_)
                        | Message::Pong(_)
                        | Message::Frame(_),
                    ) => {}
                    Err(error) => {
                        fail_pending(&reader_pending, &CdpError::WebSocket(error));
                        break;
                    }
                }
            }
            fail_pending(&reader_pending, &CdpError::Closed { url: reader_url });
        });

        Ok(Self {
            url,
            commands: command_tx,
            events: event_rx,
            pending,
            next_id: AtomicU64::new(1),
            writer,
            reader,
        })
    }

    fn url(&self) -> &str {
        &self.url
    }

    /// Sends one CDP command and waits for its response.
    async fn command(&self, method: &str, params: Value) -> Result<Value, CdpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply, response) = oneshot::channel();
        self.pending
            .lock()
            .expect("CDP pending map poisoned")
            .insert(id, reply);

        if self
            .commands
            .send(Command {
                id,
                method: method.to_owned(),
                params,
            })
            .is_err()
        {
            self.pending
                .lock()
                .expect("CDP pending map poisoned")
                .remove(&id);
            return Err(CdpError::Closed {
                url: self.url.clone(),
            });
        }

        match tokio::time::timeout(COMMAND_TIMEOUT, response).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(CdpError::Closed {
                url: self.url.clone(),
            }),
            Err(_) => {
                self.pending
                    .lock()
                    .expect("CDP pending map poisoned")
                    .remove(&id);
                Err(CdpError::Timeout {
                    method: method.to_owned(),
                    timeout_ms: COMMAND_TIMEOUT.as_millis() as u64,
                })
            }
        }
    }

    /// Waits for the next CDP event. Returns `None` once the socket is gone.
    async fn next_event(&mut self) -> Option<CdpEvent> {
        self.events.recv().await
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.writer.abort();
        self.reader.abort();
    }
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Connection")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
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
            connection: Connection::open(url).await?,
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
/// The gateway only opens one after the MCP confirmed the yield; owning two
/// controllers for one target is refused by Kitesurf (HTTP 409), so this type is
/// created exclusively by the ownership driver.
pub struct Controller {
    connection: Connection,
}

impl Controller {
    /// Connects to `target_id` below `cdp_url` as a page controller.
    pub async fn connect(cdp_url: &str, target_id: &str) -> Result<Self, CdpError> {
        let url = page_controller_url(cdp_url, target_id, CONTROLLER_OWNER)?;
        Ok(Self {
            connection: Connection::open(url).await?,
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NavigationState {
    pub url: String,
    pub title: String,
    pub can_go_back: bool,
    pub can_go_forward: bool,
}

#[derive(Debug, Clone)]
struct HistoryEntry {
    id: i64,
    url: String,
    title: String,
}

#[derive(Debug, Clone)]
struct History {
    current: usize,
    entries: Vec<HistoryEntry>,
}

fn parse_history(value: &Value) -> Option<History> {
    let current = usize::try_from(value.get("currentIndex")?.as_i64()?).ok()?;
    let entries = value
        .get("entries")?
        .as_array()?
        .iter()
        .map(|entry| {
            Some(HistoryEntry {
                id: entry.get("id")?.as_i64()?,
                url: entry.get("url")?.as_str()?.to_owned(),
                title: entry
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    if entries.is_empty() || current >= entries.len() {
        return None;
    }
    Some(History { current, entries })
}

async fn read_history(controller: &dyn PageController) -> Result<History, CdpError> {
    let value = controller
        .command("Page.getNavigationHistory", json!({}))
        .await?;
    parse_history(&value).ok_or_else(|| CdpError::Decode("no navigation entries".to_owned()))
}

async fn document_title(controller: &dyn PageController) -> Option<String> {
    let value = controller
        .command(
            "Runtime.evaluate",
            json!({"expression": "document.title", "returnByValue": true}),
        )
        .await
        .ok()?;
    value
        .get("result")?
        .get("value")?
        .as_str()
        .map(str::to_owned)
}

fn navigation_state_from(history: &History, title: Option<String>) -> NavigationState {
    let entry = &history.entries[history.current];
    NavigationState {
        url: entry.url.clone(),
        title: title.filter(|title| !title.is_empty()).unwrap_or_else(|| {
            if entry.title.is_empty() {
                entry.url.clone()
            } else {
                entry.title.clone()
            }
        }),
        can_go_back: history.current > 0,
        can_go_forward: history.current + 1 < history.entries.len(),
    }
}

/// Reads the controller's current navigation state.
///
/// # Errors
///
/// Fails when the history cannot be read or parsed.
pub async fn navigation_state(
    controller: &dyn PageController,
) -> Result<NavigationState, CdpError> {
    let history = read_history(controller).await?;
    let title = document_title(controller).await;
    Ok(navigation_state_from(&history, title))
}

/// Navigates the controller's page and reports the resulting state.
///
/// # Errors
///
/// Fails when `Page.navigate` or the follow-up history read fails.
pub async fn navigate(
    controller: &dyn PageController,
    url: &str,
) -> Result<NavigationState, CdpError> {
    controller
        .command("Page.navigate", json!({ "url": url }))
        .await?;
    navigation_state(controller).await
}

/// Steps `delta` entries through the controller's navigation history.
///
/// Kitesurf answers `Page.navigateToHistoryEntry` immediately; the history index
/// can need a moment to reflect the new entry, so the read is retried briefly
/// before giving up. Out-of-range steps are a no-op that reports the current
/// state.
///
/// # Errors
///
/// Fails when the history cannot be read or the step is rejected.
pub async fn history_step(
    controller: &dyn PageController,
    delta: i64,
) -> Result<NavigationState, CdpError> {
    let history = read_history(controller).await?;
    let target = i64::try_from(history.current).unwrap_or(i64::MAX) + delta;
    if target < 0 || usize::try_from(target).map_or(true, |target| target >= history.entries.len())
    {
        let title = document_title(controller).await;
        return Ok(navigation_state_from(&history, title));
    }
    let target_index = usize::try_from(target).unwrap_or(history.current);
    controller
        .command(
            "Page.navigateToHistoryEntry",
            json!({ "entryId": history.entries[target_index].id }),
        )
        .await?;

    let mut current = history;
    for attempt in 0..HISTORY_RETRIES {
        current = read_history(controller).await?;
        if current.current == target_index {
            break;
        }
        if attempt + 1 < HISTORY_RETRIES {
            tokio::time::sleep(HISTORY_RETRY_DELAY).await;
        }
    }
    let title = document_title(controller).await;
    Ok(navigation_state_from(&current, title))
}

/// Enables the diagnostics domains the monitor reproduces.
///
/// `Runtime.enable` and `Network.enable` are rejected on a viewer connection;
/// this is the first thing the driver does after taking the controller.
///
/// # Errors
///
/// Fails when either domain cannot be enabled.
pub async fn enable_diagnostics(controller: &dyn PageController) -> Result<(), CdpError> {
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

async fn dispatch(text: &str, pending: &Pending, events: &mpsc::Sender<CdpEvent>) -> bool {
    let value: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(error = %error, "ignoring malformed CDP message");
            return true;
        }
    };

    if let Some(id) = value.get("id").and_then(Value::as_u64) {
        let reply = pending
            .lock()
            .expect("CDP pending map poisoned")
            .remove(&id);
        if let Some(reply) = reply {
            let outcome = match value.get("error") {
                Some(error) => Err(CdpError::Protocol {
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown CDP error")
                        .to_owned(),
                }),
                None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = reply.send(outcome);
        }
        return true;
    }

    if let Some(method) = value.get("method").and_then(Value::as_str) {
        tracing::trace!(method, "CDP event");
        let event = CdpEvent {
            method: method.to_owned(),
            params: value.get("params").cloned().unwrap_or(Value::Null),
        };
        if events.send(event).await.is_err() {
            return false;
        }
    }
    true
}

fn fail_pending(pending: &Pending, error: &CdpError) {
    let drained: Vec<_> = pending
        .lock()
        .expect("CDP pending map poisoned")
        .drain()
        .map(|(_, reply)| reply)
        .collect();
    for reply in drained {
        let _ = reply.send(Err(clone_error(error)));
    }
}

fn clone_error(error: &CdpError) -> CdpError {
    match error {
        CdpError::InvalidUrl { url, reason } => CdpError::InvalidUrl {
            url: url.clone(),
            reason: reason.clone(),
        },
        CdpError::Connect { url, .. } => CdpError::Closed { url: url.clone() },
        CdpError::Closed { url } => CdpError::Closed { url: url.clone() },
        CdpError::Timeout { method, timeout_ms } => CdpError::Timeout {
            method: method.clone(),
            timeout_ms: *timeout_ms,
        },
        CdpError::Protocol { code, message } => CdpError::Protocol {
            code: *code,
            message: message.clone(),
        },
        CdpError::Decode(message) => CdpError::Decode(message.clone()),
        CdpError::WebSocket(_) => CdpError::Closed {
            url: "CDP socket".to_owned(),
        },
    }
}

/// Screencast metadata from `Page.screencastFrame`.
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

/// Starts a screencast with the gateway defaults (JPEG q55, 1600×1000 bound).
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
    let viewer = Viewer::connect(cdp_url, target_id).await?;
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
