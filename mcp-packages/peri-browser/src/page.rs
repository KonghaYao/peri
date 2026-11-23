//! One page target and its controller.
//!
//! A page owns at most one writable controller. Commands are serialised by a
//! mutex shared with the event pump; the pump holds it only for short slices so
//! a tool call never waits on an idle socket (README §4, §7).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use peri_browser_cdp::{
    dispatch_key, dispatch_mouse, insert_text, layout_metrics, navigate, navigation_state,
    Controller, KeyEventKind, MouseButton, MouseEventKind, NavigationState,
};
use serde_json::{json, Value};
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::core::BrowserCore;
use crate::diagnostics::Diagnostics;
use crate::error::BrowserError;
use crate::keys::parse_key_combo;
use crate::snapshot::{build_ax_snapshot, SnapshotBook};

/// How long `navigate_page` waits for `document.readyState === "complete"`.
const PAGE_NAV_TIMEOUT: Duration = Duration::from_secs(15);
/// Slice the event pump may hold the controller lock while waiting for an event.
const PUMP_WAIT_SLICE: Duration = Duration::from_millis(8);
/// Events drained before the pump yields the controller lock.
const PUMP_BATCH: usize = 24;
/// Poll interval for `wait_for` and `readyState`.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Address and title of a page as last reported by CDP.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PageMeta {
    pub url: String,
    pub title: String,
}

/// One managed page target.
pub struct PageHandle {
    /// Page id exposed to tools and to the monitor. It is the CDP target id so
    /// gateway ownership messages line up without a second identifier.
    pub id: String,
    pub target_id: String,
    meta: Mutex<PageMeta>,
    controller: Mutex<Option<Controller>>,
    diagnostics: Mutex<Diagnostics>,
    snapshot: Mutex<SnapshotBook>,
    attach_guard: Mutex<()>,
    pump_cancel: Mutex<Option<CancellationToken>>,
    pump_task: Mutex<Option<JoinHandle<()>>>,
    yielded: AtomicBool,
    reclaimed: Notify,
}

impl std::fmt::Debug for PageHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PageHandle")
            .field("id", &self.id)
            .field("target_id", &self.target_id)
            .finish_non_exhaustive()
    }
}

impl PageHandle {
    #[must_use]
    pub fn new(target_id: impl Into<String>) -> Arc<Self> {
        let target_id = target_id.into();
        Arc::new(Self {
            id: target_id.clone(),
            target_id,
            meta: Mutex::new(PageMeta {
                url: "about:blank".to_owned(),
                title: String::new(),
            }),
            controller: Mutex::new(None),
            diagnostics: Mutex::new(Diagnostics::new()),
            snapshot: Mutex::new(SnapshotBook::default()),
            attach_guard: Mutex::new(()),
            pump_cancel: Mutex::new(None),
            pump_task: Mutex::new(None),
            yielded: AtomicBool::new(false),
            reclaimed: Notify::new(),
        })
    }

    pub async fn meta(&self) -> PageMeta {
        self.meta.lock().await.clone()
    }

    pub async fn set_meta(&self, url: &str, title: &str) {
        let mut meta = self.meta.lock().await;
        meta.url = url.to_owned();
        meta.title = title.to_owned();
    }

    #[must_use]
    pub fn is_yielded(&self) -> bool {
        self.yielded.load(Ordering::Acquire)
    }

    pub fn mark_yielded(&self, yielded: bool) {
        self.yielded.store(yielded, Ordering::Release);
    }

    pub fn notify_reclaimed(&self) {
        self.yielded.store(false, Ordering::Release);
        self.reclaimed.notify_one();
    }

    pub async fn has_controller(&self) -> bool {
        self.controller.lock().await.is_some()
    }

    pub async fn invalidate_snapshot(&self) {
        self.snapshot.lock().await.invalidate();
    }

    pub async fn snapshot_generation(&self) -> u64 {
        self.snapshot.lock().await.generation()
    }

    /// Attaches a controller, waiting for the monitor to release it when this
    /// page was yielded. Idempotent when a controller is already present.
    ///
    /// # Errors
    ///
    /// [`BrowserError::NotConnected`] without an endpoint, and the CDP failure
    /// when another writer keeps the page.
    pub async fn ensure_controller(
        self: &Arc<Self>,
        core: &BrowserCore,
    ) -> Result<(), BrowserError> {
        let _attach = self.attach_guard.lock().await;
        if self.controller.lock().await.is_some() {
            return Ok(());
        }
        let cdp_url = core
            .config
            .cdp_url
            .clone()
            .ok_or(BrowserError::NotConnected)?;

        if self.is_yielded() && core.gateway.is_configured() {
            core.gateway.reclaim(&self.id);
            let _ = tokio::time::timeout(core.config.reclaim_wait, self.reclaimed.notified()).await;
        }

        let deadline = tokio::time::Instant::now() + core.config.reclaim_wait;
        loop {
            match Controller::connect(&cdp_url, &self.target_id, &core.config.owner).await {
                Ok(controller) => {
                    let _ = controller.command("Page.enable", json!({})).await;
                    *self.controller.lock().await = Some(controller);
                    self.mark_yielded(false);
                    self.start_pump().await;
                    return Ok(());
                }
                Err(error) => {
                    let mapped = BrowserError::from(error);
                    if mapped.code() == "another_controller"
                        && core.gateway.is_configured()
                        && tokio::time::Instant::now() < deadline
                    {
                        core.gateway.reclaim(&self.id);
                        let _ = tokio::time::timeout(
                            Duration::from_millis(200),
                            self.reclaimed.notified(),
                        )
                        .await;
                        continue;
                    }
                    if mapped.code() == "another_controller" {
                        return Err(BrowserError::Yielded);
                    }
                    return Err(mapped);
                }
            }
        }
    }

    /// Attaches a controller without asking the monitor to release it.
    ///
    /// Returns `false` when another writer holds the page. Used by `yield_page`
    /// to poll whether the human has handed the page back (README §8).
    ///
    /// # Errors
    ///
    /// [`BrowserError::NotConnected`] without an endpoint, and any CDP failure
    /// other than another controller.
    pub async fn try_attach(self: &Arc<Self>, core: &BrowserCore) -> Result<bool, BrowserError> {
        let _attach = self.attach_guard.lock().await;
        if self.controller.lock().await.is_some() {
            return Ok(true);
        }
        let cdp_url = core
            .config
            .cdp_url
            .clone()
            .ok_or(BrowserError::NotConnected)?;
        match Controller::connect(&cdp_url, &self.target_id, &core.config.owner).await {
            Ok(controller) => {
                let _ = controller.command("Page.enable", json!({})).await;
                *self.controller.lock().await = Some(controller);
                self.mark_yielded(false);
                self.start_pump().await;
                Ok(true)
            }
            Err(error) => {
                let mapped = BrowserError::from(error);
                if mapped.code() == "another_controller" {
                    Ok(false)
                } else {
                    Err(mapped)
                }
            }
        }
    }

    async fn start_pump(self: &Arc<Self>) {
        self.stop_pump().await;
        let token = CancellationToken::new();
        *self.pump_cancel.lock().await = Some(token.clone());
        let page = Arc::clone(self);
        let handle = tokio::spawn(async move { pump(page, token).await });
        *self.pump_task.lock().await = Some(handle);
    }

    async fn stop_pump(&self) {
        if let Some(token) = self.pump_cancel.lock().await.take() {
            token.cancel();
        }
        if let Some(handle) = self.pump_task.lock().await.take() {
            let _ = handle.await;
        }
    }

    /// Drops the writable controller but keeps the target and page state.
    ///
    /// Returns whether a controller was attached.
    ///
    /// # Errors
    ///
    /// Does not fail today; the `Result` keeps call sites uniform.
    pub async fn detach(self: &Arc<Self>) -> Result<bool, BrowserError> {
        if let Some(token) = self.pump_cancel.lock().await.take() {
            token.cancel();
        }
        let had_controller = {
            let mut slot = self.controller.lock().await;
            slot.take().is_some()
        };
        if let Some(handle) = self.pump_task.lock().await.take() {
            let _ = handle.await;
        }
        Ok(had_controller)
    }

    async fn command(&self, method: &str, params: Value) -> Result<Value, BrowserError> {
        let slot = self.controller.lock().await;
        let controller = slot.as_ref().ok_or(BrowserError::Yielded)?;
        controller.command(method, params).await.map_err(Into::into)
    }

    async fn handle_event(self: &Arc<Self>, event: peri_browser_cdp::CdpEvent) {
        if event.method == "Page.frameNavigated" {
            let frame = event.params.get("frame").unwrap_or(&Value::Null);
            if frame.get("parentId").is_none() {
                self.invalidate_snapshot().await;
                if let Some(url) = frame.get("url").and_then(Value::as_str) {
                    // The event carries no title, and the pump may deliver it
                    // after a navigation helper stored one. Keep a known title
                    // for the same address instead of blanking it (README §7).
                    let mut meta = self.meta.lock().await;
                    if meta.url != url {
                        meta.title.clear();
                    }
                    meta.url = url.to_owned();
                }
            }
        }
        self.diagnostics
            .lock()
            .await
            .observe(&event.method, &event.params);
    }

    async fn enable_runtime(&self) -> Result<(), BrowserError> {
        if self.diagnostics.lock().await.runtime_enabled() {
            return Ok(());
        }
        self.command("Runtime.enable", json!({})).await?;
        self.diagnostics.lock().await.mark_runtime_enabled();
        Ok(())
    }

    async fn enable_network(&self) -> Result<(), BrowserError> {
        if self.diagnostics.lock().await.network_enabled() {
            return Ok(());
        }
        self.command("Network.enable", json!({})).await?;
        self.diagnostics.lock().await.mark_network_enabled();
        Ok(())
    }

    /// Navigates and waits for the new document to load.
    ///
    /// The `peri-browser-cdp` helper performs `location.assign` because raw
    /// `Page.navigate` stops answering on Kitesurf; `readyState` is polled
    /// instead of relying on a load event (README §3, §7).
    ///
    /// # Errors
    ///
    /// Propagates CDP failures; a page that never reaches `complete` still
    /// returns its current URL and `status`.
    pub async fn navigate_page(
        self: &Arc<Self>,
        core: &BrowserCore,
        url: &str,
    ) -> Result<Value, BrowserError> {
        self.ensure_controller(core).await?;
        self.invalidate_snapshot().await;
        let slot = self.controller.lock().await;
        let controller = slot.as_ref().ok_or(BrowserError::Yielded)?;
        let state = navigate(controller, url).await?;
        let bound = core.config.page_wait.min(PAGE_NAV_TIMEOUT);
        let status = wait_ready_state(controller, bound).await;
        let final_state = navigation_state(controller)
            .await
            .unwrap_or_else(|_| state.clone());
        drop(slot);
        self.set_meta(&final_state.url, &final_state.title).await;
        self.invalidate_snapshot().await;
        Ok(json!({
            "url": final_state.url,
            "title": final_state.title,
            "status": status,
            "canGoBack": final_state.can_go_back,
            "canGoForward": final_state.can_go_forward,
            "pageId": self.id,
        }))
    }

    /// Text snapshot with fresh uids.
    ///
    /// # Errors
    ///
    /// Propagates CDP failures and reports an empty tree as a snapshot.
    pub async fn take_snapshot(
        self: &Arc<Self>,
        core: &BrowserCore,
    ) -> Result<Value, BrowserError> {
        self.ensure_controller(core).await?;
        let slot = self.controller.lock().await;
        let controller = slot.as_ref().ok_or(BrowserError::Yielded)?;
        let _ = controller.command("Accessibility.enable", json!({})).await;
        let tree = controller
            .command("Accessibility.getFullAXTree", json!({}))
            .await?;
        drop(slot);

        let generation = {
            let mut book = self.snapshot.lock().await;
            book.invalidate();
            book.generation()
        };
        let nodes = tree
            .get("nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let built = build_ax_snapshot(&nodes, generation);
        self.snapshot.lock().await.store(&built);
        Ok(json!({
            "snapshot": built.text,
            "uidCount": built.uids.len(),
            "nodeCount": built.node_count,
            "pageId": self.id,
        }))
    }

    /// Captures the viewport as JPEG, bounded to 1600x1000.
    ///
    /// # Errors
    ///
    /// Propagates a missing controller or CDP failure.
    pub async fn take_screenshot(
        self: &Arc<Self>,
        core: &BrowserCore,
    ) -> Result<Screenshot, BrowserError> {
        self.ensure_controller(core).await?;
        let slot = self.controller.lock().await;
        let controller = slot.as_ref().ok_or(BrowserError::Yielded)?;
        let viewport = layout_metrics(controller).await.ok().flatten();
        let mut params = json!({
            "format": "jpeg",
            "quality": 70,
            "fromSurface": true,
            "captureBeyondViewport": false,
        });
        if let Some(viewport) = viewport {
            let width = f64::from(viewport.width.max(1));
            let height = f64::from(viewport.height.max(1));
            let scale = (1600.0 / width).min(1000.0 / height).min(1.0);
            if scale < 1.0 {
                params["clip"] = json!({
                    "x": 0,
                    "y": 0,
                    "width": width,
                    "height": height,
                    "scale": scale,
                });
            }
        }
        let result = controller.command("Page.captureScreenshot", params).await?;
        drop(slot);
        let data = result
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(|| BrowserError::Internal("screenshot reply has no data".to_owned()))?;
        Ok(Screenshot {
            data: data.to_owned(),
            mime_type: "image/jpeg".to_owned(),
        })
    }

    /// Evaluates an expression with awaited promises and by-value results.
    ///
    /// # Errors
    ///
    /// [`BrowserError::Script`] when the page throws.
    pub async fn evaluate_script(
        self: &Arc<Self>,
        core: &BrowserCore,
        expression: &str,
    ) -> Result<Value, BrowserError> {
        self.ensure_controller(core).await?;
        let result = self
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
        if let Some(exception) = result.get("exceptionDetails") {
            return Err(BrowserError::Script(exception_text(exception)));
        }
        Ok(result
            .get("result")
            .cloned()
            .unwrap_or_else(|| json!({"type": "undefined"})))
    }

    /// Resolves a uid or CSS selector to a visible element and clicks its center.
    ///
    /// # Errors
    ///
    /// Stale uid, missing element, or CDP failure.
    pub async fn click(
        self: &Arc<Self>,
        core: &BrowserCore,
        uid: Option<&str>,
        selector: Option<&str>,
    ) -> Result<Value, BrowserError> {
        self.ensure_controller(core).await?;
        let backend = self.resolve_backend(uid, selector).await?;
        let (x, y) = self.element_center(backend).await?;
        let slot = self.controller.lock().await;
        let controller = slot.as_ref().ok_or(BrowserError::Yielded)?;
        dispatch_mouse(
            controller,
            MouseEventKind::Down,
            x,
            y,
            Some(MouseButton::Left),
            1,
            0,
        )
        .await?;
        dispatch_mouse(
            controller,
            MouseEventKind::Up,
            x,
            y,
            Some(MouseButton::Left),
            1,
            0,
        )
        .await?;
        drop(slot);
        Ok(json!({
            "clicked": true,
            "x": x,
            "y": y,
            "backendNodeId": backend,
            "pageId": self.id,
        }))
    }

    /// Focuses a field, replaces its contents and fires `input`/`change`.
    ///
    /// # Errors
    ///
    /// Stale uid, missing element, or CDP failure.
    pub async fn fill(
        self: &Arc<Self>,
        core: &BrowserCore,
        uid: Option<&str>,
        selector: Option<&str>,
        text: &str,
    ) -> Result<Value, BrowserError> {
        self.ensure_controller(core).await?;
        let backend = self.resolve_backend(uid, selector).await?;
        self.command(
            "DOM.scrollIntoViewIfNeeded",
            json!({"backendNodeId": backend}),
        )
        .await?;
        self.command("DOM.focus", json!({"backendNodeId": backend}))
            .await?;
        self.command(
            "Runtime.evaluate",
            json!({
                "expression": SELECT_ALL,
                "returnByValue": true,
            }),
        )
        .await?;
        let slot = self.controller.lock().await;
        let controller = slot.as_ref().ok_or(BrowserError::Yielded)?;
        if text.is_empty() {
            // `Input.insertText` with an empty payload is a no-op. The field was
            // select-all'd above, so one Backspace clears the selection.
            dispatch_key(
                controller,
                KeyEventKind::Down,
                "Backspace",
                "Backspace",
                None,
                0,
            )
            .await?;
            dispatch_key(
                controller,
                KeyEventKind::Up,
                "Backspace",
                "Backspace",
                None,
                0,
            )
            .await?;
        } else {
            insert_text(controller, text).await?;
        }
        drop(slot);
        self.command(
            "Runtime.evaluate",
            json!({
                "expression": NOTIFY_EDIT,
                "returnByValue": true,
            }),
        )
        .await?;
        Ok(json!({
            "filled": true,
            "cleared": text.is_empty(),
            "length": text.chars().count(),
            "backendNodeId": backend,
            "pageId": self.id,
        }))
    }

    /// Moves the pointer to the element center.
    ///
    /// # Errors
    ///
    /// Stale uid, missing element, or CDP failure.
    pub async fn hover(
        self: &Arc<Self>,
        core: &BrowserCore,
        uid: Option<&str>,
        selector: Option<&str>,
    ) -> Result<Value, BrowserError> {
        self.ensure_controller(core).await?;
        let backend = self.resolve_backend(uid, selector).await?;
        let (x, y) = self.element_center(backend).await?;
        let slot = self.controller.lock().await;
        let controller = slot.as_ref().ok_or(BrowserError::Yielded)?;
        dispatch_mouse(controller, MouseEventKind::Move, x, y, None, 0, 0).await?;
        drop(slot);
        Ok(json!({
            "hovered": true,
            "x": x,
            "y": y,
            "backendNodeId": backend,
            "pageId": self.id,
        }))
    }

    /// Dispatches one key chord (down and up).
    ///
    /// # Errors
    ///
    /// Invalid chord syntax or CDP failure.
    pub async fn press_key(
        self: &Arc<Self>,
        core: &BrowserCore,
        chord: &str,
    ) -> Result<Value, BrowserError> {
        let combo = parse_key_combo(chord).map_err(|reason| BrowserError::InvalidArgument {
            name: "key".to_owned(),
            reason,
        })?;
        self.ensure_controller(core).await?;
        let slot = self.controller.lock().await;
        let controller = slot.as_ref().ok_or(BrowserError::Yielded)?;
        dispatch_key(
            controller,
            KeyEventKind::Down,
            &combo.key,
            &combo.code,
            combo.text.as_deref(),
            combo.modifiers,
        )
        .await?;
        dispatch_key(
            controller,
            KeyEventKind::Up,
            &combo.key,
            &combo.code,
            None,
            combo.modifiers,
        )
        .await?;
        drop(slot);
        Ok(json!({
            "pressed": chord,
            "key": combo.key,
            "code": combo.code,
            "modifiers": combo.modifiers,
            "pageId": self.id,
        }))
    }

    /// Polls for page text or a JS predicate. A timeout returns `matched: false`.
    ///
    /// # Errors
    ///
    /// Invalid arguments, script syntax errors, or CDP failures.
    pub async fn wait_for(
        self: &Arc<Self>,
        core: &BrowserCore,
        text: Option<&str>,
        predicate: Option<&str>,
        timeout_ms: Option<u64>,
    ) -> Result<Value, BrowserError> {
        if text.is_some() && predicate.is_some() {
            return Err(BrowserError::InvalidArgument {
                name: "wait_for".to_owned(),
                reason: "pass text or predicate, not both".to_owned(),
            });
        }
        let bound = Duration::from_millis(timeout_ms.unwrap_or(10_000).min(60_000));
        let started = std::time::Instant::now();
        let expression = match (text, predicate) {
            (Some(text), _) => {
                let literal = serde_json::to_string(text)
                    .map_err(|error| BrowserError::Internal(error.to_string()))?;
                format!(
                    "(() => {{ const body = document.body; return Boolean(body && body.innerText && body.innerText.includes({literal})); }})()"
                )
            }
            (None, Some(predicate)) => format!("Boolean(({predicate}))"),
            (None, None) => String::new(),
        };
        if expression.is_empty() {
            tokio::time::sleep(bound).await;
            return Ok(json!({
                "matched": true,
                "waitedMs": started.elapsed().as_millis(),
                "pageId": self.id,
            }));
        }

        self.ensure_controller(core).await?;
        let deadline = tokio::time::Instant::now() + bound;
        loop {
            let value = self
                .command(
                    "Runtime.evaluate",
                    json!({
                        "expression": expression,
                        "awaitPromise": true,
                        "returnByValue": true,
                    }),
                )
                .await;
            match value {
                Ok(result) => {
                    if let Some(exception) = result.get("exceptionDetails") {
                        return Err(BrowserError::Script(exception_text(exception)));
                    }
                    let matched = result
                        .get("result")
                        .and_then(|result| result.get("value"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if matched {
                        return Ok(json!({
                            "matched": true,
                            "waitedMs": started.elapsed().as_millis(),
                            "pageId": self.id,
                        }));
                    }
                }
                Err(error) if error.code() == "context_destroyed" => {}
                Err(error) => return Err(error),
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(json!({
                    "matched": false,
                    "waitedMs": started.elapsed().as_millis(),
                    "pageId": self.id,
                }));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Steps the page history by `delta` entries (viewer address-bar commands).
    ///
    /// # Errors
    ///
    /// A missing controller or CDP failure. Out-of-range deltas return
    /// `navigated: false` instead of failing.
    pub async fn history(
        self: &Arc<Self>,
        core: &BrowserCore,
        delta: i64,
    ) -> Result<Value, BrowserError> {
        self.ensure_controller(core).await?;
        let history = self.command("Page.getNavigationHistory", json!({})).await?;
        let current = history
            .get("currentIndex")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let entries = history
            .get("entries")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let target = current.saturating_add(delta);
        if target < 0 || target as usize >= entries.len() {
            return Ok(json!({
                "navigated": false,
                "reason": "history bounds",
                "pageId": self.id,
            }));
        }
        let entry_id = entries[target as usize]
            .get("id")
            .and_then(Value::as_i64)
            .ok_or_else(|| BrowserError::BrowserControl {
                method: "Page.getNavigationHistory".to_owned(),
                message: "entry has no id".to_owned(),
            })?;
        self.command("Page.navigateToHistoryEntry", json!({"entryId": entry_id}))
            .await?;
        let state = {
            let slot = self.controller.lock().await;
            let controller = slot.as_ref().ok_or(BrowserError::Yielded)?;
            navigation_state(controller).await.ok()
        };
        self.invalidate_snapshot().await;
        if let Some(state) = &state {
            self.set_meta(&state.url, &state.title).await;
        }
        Ok(json!({
            "navigated": true,
            "url": state.as_ref().map(|state| state.url.clone()),
            "title": state.as_ref().map(|state| state.title.clone()),
            "pageId": self.id,
        }))
    }

    /// Console entries buffered for this page, enabling `Runtime` on first use.
    ///
    /// # Errors
    ///
    /// Propagates CDP failures when enabling the domain.
    pub async fn list_console(self: &Arc<Self>, core: &BrowserCore) -> Result<Value, BrowserError> {
        self.ensure_controller(core).await?;
        self.enable_runtime().await?;
        let entries = self.diagnostics.lock().await.console_entries();
        Ok(json!({
            "count": entries.len(),
            "entries": entries,
            "pageId": self.id,
        }))
    }

    /// Buffered network requests, newest first, enabling `Network` on first use.
    ///
    /// # Errors
    ///
    /// Propagates CDP failures when enabling the domain.
    pub async fn list_network(
        self: &Arc<Self>,
        core: &BrowserCore,
        filter: Option<&str>,
    ) -> Result<Value, BrowserError> {
        self.ensure_controller(core).await?;
        self.enable_network().await?;
        let requests = self.diagnostics.lock().await.list_network(filter);
        Ok(json!({
            "count": requests.len(),
            "requests": requests,
            "pageId": self.id,
        }))
    }

    /// One buffered request with headers and, when still buffered, its body.
    ///
    /// # Errors
    ///
    /// [`BrowserError::ElementNotFound`] for an unknown request id.
    pub async fn get_network_request(
        self: &Arc<Self>,
        core: &BrowserCore,
        request_id: &str,
    ) -> Result<Value, BrowserError> {
        self.ensure_controller(core).await?;
        self.enable_network().await?;
        let entry = self
            .diagnostics
            .lock()
            .await
            .get_network(request_id)
            .ok_or_else(|| BrowserError::ElementNotFound(request_id.to_owned()))?;
        let mut body = Value::Null;
        let mut base64_encoded = false;
        let mut body_truncated = false;
        let mut body_error = Value::Null;
        if entry.finished && entry.error.is_none() {
            match self
                .command("Network.getResponseBody", json!({"requestId": request_id}))
                .await
            {
                Ok(response) => {
                    if let Some(text) = response.get("body").and_then(Value::as_str) {
                        base64_encoded = response
                            .get("base64Encoded")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        if text.len() > crate::diagnostics::MAX_BODY_BYTES {
                            let mut cut = crate::diagnostics::MAX_BODY_BYTES;
                            while cut > 0 && !text.is_char_boundary(cut) {
                                cut -= 1;
                            }
                            body = Value::String(text[..cut].to_owned());
                            body_truncated = true;
                        } else {
                            body = Value::String(text.to_owned());
                        }
                    }
                }
                Err(error) => body_error = Value::String(error.render()),
            }
        }
        Ok(json!({
            "request": entry,
            "body": body,
            "base64Encoded": base64_encoded,
            "bodyTruncated": body_truncated,
            "bodyError": body_error,
            "pageId": self.id,
        }))
    }

    async fn resolve_backend(
        &self,
        uid: Option<&str>,
        selector: Option<&str>,
    ) -> Result<i64, BrowserError> {
        match (uid, selector) {
            (Some(uid), _) => {
                let book = self.snapshot.lock().await;
                book.resolve(uid)
            }
            (None, Some(selector)) => {
                let document = self.command("DOM.getDocument", json!({"depth": 0})).await?;
                let node_id = document
                    .get("root")
                    .and_then(|root| root.get("nodeId"))
                    .and_then(Value::as_i64)
                    .ok_or_else(|| {
                        BrowserError::Internal("DOM.getDocument has no root".to_owned())
                    })?;
                let found = self
                    .command(
                        "DOM.querySelector",
                        json!({"nodeId": node_id, "selector": selector}),
                    )
                    .await?;
                let node_id = found.get("nodeId").and_then(Value::as_i64).unwrap_or(0);
                if node_id == 0 {
                    return Err(BrowserError::ElementNotFound(selector.to_owned()));
                }
                let described = self
                    .command("DOM.describeNode", json!({"nodeId": node_id}))
                    .await?;
                described
                    .get("node")
                    .and_then(|node| node.get("backendNodeId"))
                    .and_then(Value::as_i64)
                    .filter(|id| *id > 0)
                    .ok_or_else(|| BrowserError::ElementNotFound(selector.to_owned()))
            }
            (None, None) => Err(BrowserError::InvalidArgument {
                name: "uid".to_owned(),
                reason: "pass uid or selector".to_owned(),
            }),
        }
    }

    async fn element_center(&self, backend: i64) -> Result<(f64, f64), BrowserError> {
        self.command(
            "DOM.scrollIntoViewIfNeeded",
            json!({"backendNodeId": backend}),
        )
        .await?;
        let model = self
            .command("DOM.getBoxModel", json!({"backendNodeId": backend}))
            .await?;
        let content = model
            .get("model")
            .and_then(|model| model.get("content"))
            .and_then(Value::as_array)
            .ok_or_else(|| BrowserError::NotVisible(backend.to_string()))?;
        if content.len() < 8 {
            return Err(BrowserError::NotVisible(backend.to_string()));
        }
        let value = |index: usize| content[index].as_f64();
        let (Some(x1), Some(y1), Some(x2), Some(y2), Some(x3), Some(y3)) =
            (value(0), value(1), value(2), value(3), value(4), value(5))
        else {
            return Err(BrowserError::NotVisible(backend.to_string()));
        };
        let _ = (x2, y2);
        Ok(((x1 + x3) / 2.0, (y1 + y3) / 2.0))
    }
}

/// JPEG bytes ready to become MCP image content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screenshot {
    pub data: String,
    pub mime_type: String,
}

/// Focused element: select existing content when the control supports it.
const SELECT_ALL: &str = "(() => { const el = document.activeElement; if (!el) return false; if (typeof el.select === 'function') { el.select(); return true; } if ('selectionStart' in el) { el.selectionStart = 0; el.selectionEnd = el.value.length; return true; } return false; })()";
/// Fire the edit events a real user edit produces after `Input.insertText`.
const NOTIFY_EDIT: &str = "(() => { const el = document.activeElement; if (!el) return false; el.dispatchEvent(new Event('input', { bubbles: true })); el.dispatchEvent(new Event('change', { bubbles: true })); return true; })()";

async fn pump(page: Arc<PageHandle>, token: CancellationToken) {
    loop {
        if token.is_cancelled() {
            return;
        }
        let mut drained = 0;
        loop {
            let mut slot = page.controller.lock().await;
            let Some(controller) = slot.as_mut() else {
                return;
            };
            let event = tokio::select! {
                _ = token.cancelled() => return,
                event = tokio::time::timeout(PUMP_WAIT_SLICE, controller.next_event()) => event,
            };
            match event {
                Ok(Some(event)) => {
                    drop(slot);
                    page.handle_event(event).await;
                    drained += 1;
                    if drained >= PUMP_BATCH {
                        break;
                    }
                }
                Ok(None) => return,
                Err(_) => break,
            }
        }
        tokio::select! {
            _ = token.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_millis(2)) => {}
        }
    }
}

async fn wait_ready_state(controller: &Controller, bound: Duration) -> String {
    let deadline = tokio::time::Instant::now() + bound;
    let mut last = "unknown".to_owned();
    loop {
        let result = controller
            .command(
                "Runtime.evaluate",
                json!({"expression": "document.readyState", "returnByValue": true}),
            )
            .await;
        if let Ok(value) = result {
            if let Some(state) = value
                .get("result")
                .and_then(|result| result.get("value"))
                .and_then(Value::as_str)
            {
                last = state.to_owned();
                if state == "complete" {
                    return last;
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn exception_text(exception: &Value) -> String {
    exception
        .get("exception")
        .and_then(|exception| {
            exception
                .get("description")
                .and_then(Value::as_str)
                .or_else(|| exception.get("value").and_then(Value::as_str))
        })
        .or_else(|| exception.get("text").and_then(Value::as_str))
        .unwrap_or("the page threw while evaluating the script")
        .to_owned()
}

/// Helper for tests: the state after a navigation.
#[allow(dead_code)]
pub(crate) fn navigation_snapshot(state: &NavigationState) -> Value {
    json!({"url": state.url, "title": state.title})
}

#[cfg(test)]
#[path = "page_test.rs"]
mod tests;
