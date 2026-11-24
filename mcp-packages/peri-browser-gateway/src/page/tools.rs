use std::time::Duration;

use serde_json::{json, Value};

use crate::cdp::{
    dispatch_key, dispatch_mouse, insert_text, layout_metrics, navigate, navigation_state,
    PageController,
};
use crate::error::BrowserError;
use crate::input::{KeyEventKind, MouseButton, MouseEventKind};
use crate::keys::parse_key_combo;
use crate::snapshot::build_ax_snapshot;

use super::{PageOperation, Screenshot};

const PAGE_NAV_TIMEOUT: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_millis(200);
const SELECT_ALL: &str = "(() => { const el = document.activeElement; if (!el) return false; if (typeof el.select === 'function') { el.select(); return true; } if ('selectionStart' in el) { el.selectionStart = 0; el.selectionEnd = el.value.length; return true; } return false; })()";
const NOTIFY_EDIT: &str = "(() => { const el = document.activeElement; if (!el) return false; el.dispatchEvent(new Event('input', { bubbles: true })); el.dispatchEvent(new Event('change', { bubbles: true })); return true; })()";

impl PageOperation<'_> {
    async fn command(&self, method: &str, params: Value) -> Result<Value, BrowserError> {
        PageController::command(self, method, params)
            .await
            .map_err(Into::into)
    }

    async fn enable_runtime(&self) -> Result<(), BrowserError> {
        if !self.diagnostics.lock().await.runtime_enabled() {
            self.command("Runtime.enable", json!({})).await?;
            self.diagnostics.lock().await.mark_runtime_enabled();
        }
        Ok(())
    }

    async fn enable_network(&self) -> Result<(), BrowserError> {
        if !self.diagnostics.lock().await.network_enabled() {
            self.command("Network.enable", json!({})).await?;
            self.diagnostics.lock().await.mark_network_enabled();
        }
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
    pub async fn navigate_page(&self, url: &str) -> Result<Value, BrowserError> {
        self.invalidate_snapshot().await;
        let controller = self;
        let state = navigate(controller, url).await?;
        let bound = self.page_wait.min(PAGE_NAV_TIMEOUT);
        let status = wait_ready_state(controller, bound).await;
        let final_state = navigation_state(controller)
            .await
            .unwrap_or_else(|_| state.clone());
        self.set_navigation(&final_state).await;
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
    pub async fn take_snapshot(&self) -> Result<Value, BrowserError> {
        let generation = {
            let mut book = self.snapshot.lock().await;
            book.invalidate();
            book.generation()
        };
        let document = self.document_key().await;
        let _ = self.command("Accessibility.enable", json!({})).await;
        let tree = self
            .command("Accessibility.getFullAXTree", json!({}))
            .await?;
        let nodes = tree
            .get("nodes")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let built = build_ax_snapshot(nodes, generation);
        let after = self.document_key().await;
        let mut book = self.snapshot.lock().await;
        if document != after || generation != book.generation() {
            return Err(BrowserError::StaleUid(
                "snapshot changed while reading the document".to_owned(),
            ));
        }
        book.store(&built);
        book.document = document;
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
    pub async fn take_screenshot(&self) -> Result<Screenshot, BrowserError> {
        let controller = self;
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
    pub async fn evaluate_script(&self, expression: &str) -> Result<Value, BrowserError> {
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
        &self,
        uid: Option<&str>,
        selector: Option<&str>,
    ) -> Result<Value, BrowserError> {
        let backend = self.resolve_backend(uid, selector).await?;
        let (x, y) = self.element_center(backend).await?;
        let controller = self;
        // 真实点击先移动指针：hover/pointerover 先于按下发生，依赖 :hover 的样式
        // 与悬停分支才和浏览器行为一致；顶层文档的输入路径依赖这一点。
        dispatch_mouse(controller, MouseEventKind::Move, x, y, None, 0, 0).await?;
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
        &self,
        uid: Option<&str>,
        selector: Option<&str>,
        text: &str,
    ) -> Result<Value, BrowserError> {
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
        let controller = self;
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
        &self,
        uid: Option<&str>,
        selector: Option<&str>,
    ) -> Result<Value, BrowserError> {
        let backend = self.resolve_backend(uid, selector).await?;
        let (x, y) = self.element_center(backend).await?;
        let controller = self;
        dispatch_mouse(controller, MouseEventKind::Move, x, y, None, 0, 0).await?;
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
    pub async fn press_key(&self, chord: &str) -> Result<Value, BrowserError> {
        let combo = parse_key_combo(chord).map_err(|reason| BrowserError::InvalidArgument {
            name: "key".to_owned(),
            reason,
        })?;
        let controller = self;
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
        &self,
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
    pub async fn history(&self, delta: i64) -> Result<Value, BrowserError> {
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
        self.invalidate_snapshot().await;
        self.command("Page.navigateToHistoryEntry", json!({"entryId": entry_id}))
            .await?;
        let state = { navigation_state(self).await.ok() };
        self.invalidate_snapshot().await;
        if let Some(state) = &state {
            self.set_navigation(state).await;
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
    pub async fn list_console(&self) -> Result<Value, BrowserError> {
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
    pub async fn list_network(&self, filter: Option<&str>) -> Result<Value, BrowserError> {
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
    pub async fn get_network_request(&self, request_id: &str) -> Result<Value, BrowserError> {
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

    async fn document_key(&self) -> Option<String> {
        let tree = self.command("Page.getFrameTree", json!({})).await.ok()?;
        let frame = &tree["frameTree"]["frame"];
        let id = frame["id"].as_str()?;
        let loader = frame["loaderId"].as_str()?;
        (!loader.is_empty()).then(|| format!("{id}:{loader}"))
    }

    async fn resolve_backend(
        &self,
        uid: Option<&str>,
        selector: Option<&str>,
    ) -> Result<i64, BrowserError> {
        match (uid, selector) {
            (Some(uid), _) => {
                let document = {
                    let book = self.snapshot.lock().await;
                    book.resolve(uid)?;
                    book.document.clone()
                };
                if document.is_none() || self.document_key().await != document {
                    self.invalidate_snapshot().await;
                    return Err(BrowserError::StaleUid(uid.to_owned()));
                }
                self.snapshot.lock().await.resolve(uid)
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

async fn wait_ready_state(controller: &dyn PageController, bound: Duration) -> String {
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
