//! Session state: pages, the active page, the runtime browser endpoint, agent
//! lifecycle and the gateway bridge.
//!
//! The core owns no browser connection itself. Page connections live in
//! [`PageHandle`]; this module coordinates them, answers tool calls, and turns
//! gateway messages into page operations (README §5, §6.2, §8). The endpoint is
//! chosen at runtime by [`BrowserCore::connect`] and lives in memory only: a
//! restart starts disconnected.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, warn};

use crate::config::BrowserConfig;
use crate::error::BrowserError;
use crate::gateway::{
    AgentState, BrowserCommand, GatewayClient, HumanStatus, Incoming, PageRef, YieldReason,
};
use crate::onboarding;
use crate::page::{PageHandle, Screenshot};
use crate::provision::{close_target, create_target, probe_engine, EngineProbe, ProvisionSettings};

/// Poll interval while a manual yield waits for the controller to come back.
const YIELD_POLL: Duration = Duration::from_millis(250);
/// Poll interval while a finished human handoff releases the controller.
const HANDOFF_POLL: Duration = Duration::from_millis(50);

struct PendingHuman {
    request_id: String,
    page_id: String,
    reply: oneshot::Sender<HumanOutcome>,
}

struct HumanOutcome {
    status: HumanStatus,
    url: String,
    title: String,
}

/// One MCP session's browser state.
pub struct BrowserCore {
    pub config: Arc<BrowserConfig>,
    pub gateway: GatewayClient,
    session_id: String,
    /// Canonical endpoint of the connected instance; `None` until the first
    /// successful `browser_connect`.
    endpoint: Mutex<Option<String>>,
    pages: Mutex<BTreeMap<String, Arc<PageHandle>>>,
    active: Mutex<Option<String>>,
    state: Mutex<AgentState>,
    human: Mutex<Option<PendingHuman>>,
    idle_generation: AtomicU64,
}

impl BrowserCore {
    /// Creates the session. The gateway writer is not started yet: it first
    /// registers after `browser_connect` supplied a real endpoint. Must run
    /// inside a Tokio runtime.
    #[must_use]
    pub fn new(config: BrowserConfig) -> Arc<Self> {
        let session_id = uuid::Uuid::new_v4().to_string();
        let (inbox_tx, inbox_rx) = mpsc::unbounded_channel();
        let gateway = GatewayClient::new(&config, &session_id, inbox_tx);
        let core = Arc::new(Self {
            config: Arc::new(config),
            gateway,
            session_id,
            endpoint: Mutex::new(None),
            pages: Mutex::new(BTreeMap::new()),
            active: Mutex::new(None),
            state: Mutex::new(AgentState::Idle),
            human: Mutex::new(None),
            idle_generation: AtomicU64::new(0),
        });
        let weak = Arc::downgrade(&core);
        tokio::spawn(async move { dispatch(weak, inbox_rx).await });
        core
    }

    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Canonical endpoint of the connected instance.
    ///
    /// # Errors
    ///
    /// [`BrowserError::NotConnected`] before the first `browser_connect`.
    pub async fn endpoint(&self) -> Result<String, BrowserError> {
        self.endpoint
            .lock()
            .await
            .clone()
            .ok_or(BrowserError::NotConnected)
    }

    /// Connects to the runtime endpoint and registers the monitor gateway.
    ///
    /// The first success fixes the endpoint for the process lifetime. Calling
    /// again with the same canonical URL is an idempotent success; a different
    /// URL is refused with [`BrowserError::AlreadyConnected`] rather than
    /// silently replacing an instance that already owns pages.
    ///
    /// # Errors
    ///
    /// [`BrowserError::InvalidArgument`] for a non-loopback/TLS/portless URL,
    /// [`BrowserError::ConnectFailed`] when the probe cannot reach the
    /// endpoint, [`BrowserError::AlreadyConnected`] for a second endpoint.
    pub async fn connect(&self, raw_url: &str) -> Result<Value, BrowserError> {
        let canonical = onboarding::normalize_endpoint(raw_url)?;
        // Probe before touching the state: an unreachable URL leaves the
        // current connection exactly as it was.
        let probe = probe_engine(&canonical, self.config.connect_timeout).await;
        if !probe.reachable {
            return Err(connect_failed(&canonical, &probe));
        }
        let mut slot = self.endpoint.lock().await;
        match slot.as_deref() {
            Some(current) if current == canonical => {
                drop(slot);
                return Ok(json!({
                    "connected": true,
                    "alreadyConnected": true,
                    "endpoint": canonical,
                    "engine": engine_json(&probe),
                }));
            }
            Some(current) => {
                return Err(BrowserError::AlreadyConnected {
                    endpoint: current.to_owned(),
                    requested: canonical,
                });
            }
            None => {}
        }
        *slot = Some(canonical.clone());
        drop(slot);
        self.gateway.set_endpoint(&canonical);
        self.gateway.start();
        debug!(endpoint = %canonical, "browser endpoint connected");
        Ok(json!({
            "connected": true,
            "endpoint": canonical,
            "engine": engine_json(&probe),
        }))
    }

    /// Cancels a pending idle-yield timer and arms a new one. Called after each
    /// tool call; without a monitor there is nobody to yield to.
    pub fn touch(self: &Arc<Self>) {
        if !self.gateway.is_configured() {
            return;
        }
        let generation = self.idle_generation.fetch_add(1, Ordering::AcqRel) + 1;
        let idle = self.config.idle_yield;
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(idle).await;
            let Some(core) = weak.upgrade() else {
                return;
            };
            if core.idle_generation.load(Ordering::Acquire) != generation {
                return;
            }
            core.idle_yield().await;
        });
    }

    /// One quiet period elapsed: close the page connection so the monitor can
    /// take over. The target and its JavaScript state stay alive.
    async fn idle_yield(&self) {
        if self.human.lock().await.is_some() {
            return;
        }
        let Ok(page) = self.active_page().await else {
            return;
        };
        if !page.has_controller().await {
            return;
        }
        match page.detach().await {
            Ok(true) => {
                page.mark_yielded(true);
                self.gateway.yield_ok(&page.id);
                self.set_state(AgentState::Idle).await;
                debug!(page = %page.id, "idle yield: page connection closed");
            }
            Ok(false) => {}
            Err(error) => debug!(error = %error, "idle yield could not close the page connection"),
        }
    }

    /// Reports connection, install, endpoint and ownership state (README §7
    /// `browser_status`).
    pub async fn status(&self) -> Value {
        let endpoint = self.endpoint.lock().await.clone();
        let probe = match endpoint.as_deref() {
            Some(url) => Some(probe_engine(url, self.config.connect_timeout).await),
            None => None,
        };
        let reachable = probe.as_ref().is_some_and(|probe| probe.reachable);
        // Probe the recommendation only while disconnected: the model uses it
        // to decide whether kitesurf still needs to be started.
        let listening = if reachable {
            None
        } else {
            Some(onboarding::probe_recommended(self.config.connect_timeout).await)
        };
        let install =
            onboarding::detect_install(&self.config.kitesurf_home, &|key| std::env::var(key).ok());
        let platform = onboarding::platform();
        let next_steps = onboarding::next_steps(
            install.installed,
            listening.unwrap_or(false),
            endpoint.as_deref(),
            reachable,
        );
        let pages = self.page_views().await;
        let active = self.active.lock().await.clone();
        let state = *self.state.lock().await;
        // The gateway is deliberately not started before `browser_connect`, so
        // a configured socket without an endpoint means the monitor is waiting
        // for the browser, not broken. After a connect, a missing gateway is the
        // pre-existing "unavailable" state.
        let monitor = if self.gateway.is_connected() {
            "connected"
        } else if endpoint.is_none() && self.gateway.sock_path().is_some() {
            "waiting_for_connect"
        } else {
            "unavailable"
        };
        json!({
            "connected": reachable,
            "endpoint": endpoint,
            "platform": platform.to_json(),
            "kitesurf": install.to_json(),
            "recommendedEndpoint": {
                "url": onboarding::recommended_endpoint(),
                "listening": listening,
            },
            "nextSteps": next_steps,
            "owner": self.config.owner,
            "sessionId": self.session_id,
            "state": state.as_str(),
            "pageCount": pages.len(),
            "activePageId": active,
            "pages": pages,
            "engine": probe.map(|probe| json!({
                "reachable": probe.reachable,
                "version": probe.version,
                "pageCount": probe.page_count,
                "error": probe.error,
            })),
            "monitor": monitor,
            "gatewaySock": self.gateway.sock_path().map(|path| path.display().to_string()),
            "yielded": self.any_yielded().await,
        })
    }

    /// Pages this session owns (README §7 `list_pages`).
    pub async fn list_pages(&self) -> Value {
        let pages = self.page_views().await;
        let active = self.active.lock().await.clone();
        json!({
            "count": pages.len(),
            "activePageId": active,
            "pages": pages,
        })
    }

    /// Switches the active page inside this session's own connections.
    pub async fn select_page(&self, page_id: &str) -> Result<Value, BrowserError> {
        let page = self.resolve_page(Some(page_id)).await?;
        *self.active.lock().await = Some(page.id.clone());
        let meta = page.meta().await;
        Ok(json!({
            "selected": page.id,
            "url": meta.url,
            "title": meta.title,
            "pageId": page.id,
        }))
    }

    /// Creates a target through the machine-local create lock and keeps the new
    /// page active (README §7 `new_page`).
    pub async fn new_page(&self, url: Option<&str>) -> Result<Value, BrowserError> {
        let settings = self.provision_settings().await?;
        let target_id = create_target(&settings).await?;
        let page = PageHandle::new(target_id.clone());
        self.pages
            .lock()
            .await
            .insert(target_id.clone(), Arc::clone(&page));
        *self.active.lock().await = Some(target_id.clone());
        self.set_state(AgentState::Driving).await;
        self.sync_pages().await;

        let mut navigation = Value::Null;
        if let Some(url) = url.map(str::trim).filter(|url| !url.is_empty()) {
            navigation = page.navigate_page(self, url).await?;
            self.sync_pages().await;
        }
        let meta = page.meta().await;
        Ok(json!({
            "pageId": page.id,
            "url": meta.url,
            "title": meta.title,
            "navigation": navigation,
            "count": self.pages.lock().await.len(),
        }))
    }

    /// Closes a target through the browser endpoint and forgets the page.
    pub async fn close_page(&self, page_id: Option<&str>) -> Result<Value, BrowserError> {
        let page = self.resolve_page(page_id).await?;
        let settings = self.provision_settings().await?;
        page.detach().await?;
        close_target(&settings, &page.target_id).await?;
        let mut pages = self.pages.lock().await;
        pages.remove(&page.id);
        let next = pages.keys().next().cloned();
        drop(pages);
        {
            let mut active = self.active.lock().await;
            if active.as_deref() == Some(page.id.as_str()) {
                *active = next;
            }
        }
        self.sync_pages().await;
        Ok(json!({
            "closed": page.id,
            "success": true,
            "count": self.pages.lock().await.len(),
            "activePageId": self.active.lock().await.clone(),
        }))
    }

    /// Closes the page connection and waits up to `yield_timeout` for the page
    /// to come back. While a human holds it, the tool returns `reclaimed:false`
    /// instead of forcing the controller away (README §6.2).
    pub async fn yield_page(&self) -> Result<Value, BrowserError> {
        if !self.gateway.is_configured() {
            return Err(BrowserError::GatewayUnavailable(
                "no monitor gateway is configured, so there is nobody to yield to".to_owned(),
            ));
        }
        let page = self.active_page().await?;
        page.detach().await?;
        page.mark_yielded(true);
        self.gateway.yield_ok(&page.id);
        self.set_state(AgentState::Idle).await;

        let deadline = tokio::time::Instant::now() + self.config.yield_timeout;
        loop {
            if page.try_attach(self).await? {
                self.set_state(AgentState::Driving).await;
                return Ok(json!({
                    "yielded": true,
                    "reclaimed": true,
                    "status": "reclaimed",
                    "pageId": page.id,
                }));
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(json!({
                    "yielded": true,
                    "reclaimed": false,
                    "status": "yield_pending",
                    "pageId": page.id,
                }));
            }
            tokio::time::sleep(YIELD_POLL).await;
        }
    }

    /// Blocks until a human finishes the requested step, declines, or the
    /// timeout expires. Page tools are refused while this is pending.
    pub async fn request_human(
        &self,
        prompt: &str,
        timeout_ms: Option<u64>,
    ) -> Result<Value, BrowserError> {
        if !self.gateway.is_connected() {
            return Err(BrowserError::GatewayUnavailable(
                "the monitor gateway is not connected; start it before requesting a human"
                    .to_owned(),
            ));
        }
        let page = self.active_page().await?;
        let request_id = uuid::Uuid::new_v4().to_string();
        let (reply, receiver) = oneshot::channel();
        {
            let mut human = self.human.lock().await;
            if human.is_some() {
                return Err(BrowserError::WaitingForHuman);
            }
            *human = Some(PendingHuman {
                request_id: request_id.clone(),
                page_id: page.id.clone(),
                reply,
            });
        }
        let configured = self.config.request_human_timeout;
        let bound = timeout_ms
            .map(Duration::from_millis)
            .map_or(configured, |requested| requested.min(configured));
        self.set_state(AgentState::WaitingForHuman).await;
        self.gateway.human_request(
            &request_id,
            &page.id,
            prompt,
            u64::try_from(bound.as_millis()).unwrap_or(u64::MAX),
        );

        let outcome = match tokio::time::timeout(bound, receiver).await {
            Ok(Ok(outcome)) => Some(outcome),
            Ok(Err(_)) => None,
            Err(_) => None,
        };
        {
            let mut human = self.human.lock().await;
            if human
                .as_ref()
                .is_some_and(|pending| pending.request_id == request_id)
            {
                human.take();
            }
        }
        self.set_state(AgentState::Driving).await;

        let Some(outcome) = outcome else {
            return Ok(json!({
                "status": "no_human",
                "reason": "timeout",
                "pageId": page.id,
            }));
        };
        match outcome.status {
            HumanStatus::Done => {
                let _ = tokio::time::timeout(self.config.reclaim_wait, async {
                    while page.is_yielded() {
                        tokio::time::sleep(HANDOFF_POLL).await;
                    }
                })
                .await;
                page.set_meta(&outcome.url, &outcome.title).await;
                let screenshot: Option<Screenshot> = match page.ensure_controller(self).await {
                    Ok(()) => page.take_screenshot(self).await.ok(),
                    Err(error) => {
                        debug!(error = %error, "cannot reattach after the human handoff");
                        None
                    }
                };
                self.sync_pages().await;
                Ok(json!({
                    "status": "done",
                    "url": outcome.url,
                    "title": outcome.title,
                    "pageId": page.id,
                    "screenshot": screenshot.map(|shot| json!({
                        "data": shot.data,
                        "mimeType": shot.mime_type,
                    })),
                }))
            }
            HumanStatus::Declined => Ok(json!({
                "status": "declined",
                "reason": "the human declined the handoff",
                "pageId": page.id,
            })),
            HumanStatus::Timeout => Ok(json!({
                "status": "no_human",
                "reason": "timeout",
                "pageId": page.id,
            })),
        }
    }

    /// Page for a tool call. The first browser call in a session creates one.
    pub async fn tool_page(&self, page_id: Option<&str>) -> Result<Arc<PageHandle>, BrowserError> {
        if self.human.lock().await.is_some() {
            return Err(BrowserError::WaitingForHuman);
        }
        match self.resolve_page(page_id).await {
            Ok(page) => Ok(page),
            Err(BrowserError::NoActivePage) if page_id.is_none() => {
                self.new_page(None).await?;
                self.active_page().await
            }
            Err(error) => Err(error),
        }
    }

    /// Records that a page changed its address so the monitor tab strip follows.
    pub async fn sync_pages(&self) {
        let pages = {
            let guard = self.pages.lock().await;
            let mut refs = Vec::with_capacity(guard.len());
            for (id, page) in guard.iter() {
                let meta = page.meta().await;
                refs.push(PageRef {
                    target_id: id.clone(),
                    url: meta.url,
                    title: meta.title,
                });
            }
            refs
        };
        self.gateway.update_pages(pages);
    }

    /// Advertised monitor state for tests and `browser_status`.
    pub async fn agent_state(&self) -> AgentState {
        *self.state.lock().await
    }

    // -- internals ---------------------------------------------------------

    async fn set_state(&self, state: AgentState) {
        let mut current = self.state.lock().await;
        if *current == state {
            return;
        }
        *current = state;
        self.gateway.update_state(state);
    }

    async fn provision_settings(&self) -> Result<ProvisionSettings, BrowserError> {
        Ok(ProvisionSettings {
            cdp_url: self.endpoint().await?,
            owner: self.config.owner.clone(),
            lock: self.config.provision_lock.clone(),
            lock_wait: self.config.provision_lock_wait,
            stale_after: self.config.lock_stale_after,
            connect_timeout: self.config.connect_timeout,
        })
    }

    async fn active_page(&self) -> Result<Arc<PageHandle>, BrowserError> {
        self.resolve_page(None).await
    }

    async fn resolve_page(&self, page_id: Option<&str>) -> Result<Arc<PageHandle>, BrowserError> {
        let pages = self.pages.lock().await;
        match page_id {
            Some(page_id) => {
                if let Some(page) = pages.get(page_id) {
                    return Ok(Arc::clone(page));
                }
                let matches: Vec<&String> =
                    pages.keys().filter(|id| id.starts_with(page_id)).collect();
                match matches.as_slice() {
                    [only] => Ok(Arc::clone(&pages[*only])),
                    [] => Err(BrowserError::PageNotFound(page_id.to_owned())),
                    _ => Err(BrowserError::AmbiguousPage(page_id.to_owned())),
                }
            }
            None => {
                let active = self.active.lock().await.clone();
                match active {
                    Some(id) => pages
                        .get(&id)
                        .map(Arc::clone)
                        .ok_or(BrowserError::NoActivePage),
                    None if pages.len() == 1 => {
                        Ok(Arc::clone(pages.values().next().expect("checked length")))
                    }
                    None => Err(BrowserError::NoActivePage),
                }
            }
        }
    }

    async fn page_views(&self) -> Vec<Value> {
        let active = self.active.lock().await.clone();
        let pages = self.pages.lock().await;
        let mut views = Vec::with_capacity(pages.len());
        for (id, page) in pages.iter() {
            let meta = page.meta().await;
            views.push(json!({
                "pageId": id,
                "url": meta.url,
                "title": meta.title,
                "active": active.as_deref() == Some(id.as_str()),
                "yielded": page.is_yielded(),
                "hasController": page.has_controller().await,
            }));
        }
        views
    }

    async fn any_yielded(&self) -> bool {
        let pages = self.pages.lock().await;
        let mut yielded = false;
        for page in pages.values() {
            if page.is_yielded() {
                yielded = true;
                break;
            }
        }
        yielded
    }

    async fn handle_message(self: &Arc<Self>, message: Incoming) {
        match message {
            Incoming::YieldRequest {
                page_id,
                reason,
                timeout_ms,
            } => {
                let page = self.pages.lock().await.get(&page_id).map(Arc::clone);
                if let Some(page) = page {
                    if let Err(error) = page.detach().await {
                        warn!(page = %page_id, error = %error, "cannot close the page connection for a yield");
                    }
                    page.mark_yielded(true);
                }
                debug!(
                    page = %page_id,
                    reason = reason.as_str(),
                    timeout_ms,
                    "yield acknowledged"
                );
                self.gateway.yield_ok(&page_id);
                self.set_state(AgentState::Idle).await;
            }
            Incoming::Command {
                request_id,
                page_id,
                command,
            } => {
                let result = self.run_browser_command(&page_id, command).await;
                let (ok, error) = match result {
                    Ok(()) => (true, None),
                    Err(error) => (false, Some(error.render())),
                };
                self.gateway.command_result(&request_id, ok, error);
            }
            Incoming::HumanResult {
                request_id,
                status,
                url,
                title,
            } => {
                let pending = {
                    let mut human = self.human.lock().await;
                    match human.as_ref() {
                        Some(pending) if pending.request_id == request_id => human.take(),
                        _ => None,
                    }
                };
                let Some(pending) = pending else {
                    debug!(request_id = %request_id, "ignoring a human result nobody waits for");
                    return;
                };
                if let Some(page) = self
                    .pages
                    .lock()
                    .await
                    .get(&pending.page_id)
                    .map(Arc::clone)
                {
                    page.set_meta(&url, &title).await;
                }
                let _ = pending.reply.send(HumanOutcome { status, url, title });
            }
            Incoming::Reclaimed { page_id, ok } => {
                let page = self.pages.lock().await.get(&page_id).map(Arc::clone);
                if let Some(page) = page {
                    page.notify_reclaimed();
                }
                debug!(page = %page_id, ok, "controller reclaimed");
            }
            Incoming::Unknown => {}
        }
    }

    async fn run_browser_command(
        &self,
        page_id: &str,
        command: BrowserCommand,
    ) -> Result<(), BrowserError> {
        match command {
            BrowserCommand::Navigate { url } => {
                let page = self.resolve_page(Some(page_id)).await?;
                page.navigate_page(self, &url).await?;
                self.sync_pages().await;
                Ok(())
            }
            BrowserCommand::History { delta } => {
                let page = self.resolve_page(Some(page_id)).await?;
                page.history(self, delta).await?;
                self.sync_pages().await;
                Ok(())
            }
            BrowserCommand::NewTab { url } => {
                self.new_page(Some(&url)).await?;
                Ok(())
            }
            BrowserCommand::CloseTab { page_id } => {
                self.close_page(Some(&page_id)).await?;
                Ok(())
            }
            BrowserCommand::Unknown => Ok(()),
        }
    }
}

async fn dispatch(core: Weak<BrowserCore>, mut inbox: mpsc::UnboundedReceiver<Incoming>) {
    while let Some(message) = inbox.recv().await {
        let Some(core) = core.upgrade() else {
            return;
        };
        core.handle_message(message).await;
    }
}

impl std::fmt::Debug for BrowserCore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserCore")
            .field("session_id", &self.session_id)
            .field(
                "endpoint",
                &self
                    .endpoint
                    .try_lock()
                    .ok()
                    .and_then(|endpoint| endpoint.clone()),
            )
            .finish_non_exhaustive()
    }
}

fn engine_json(probe: &EngineProbe) -> Value {
    json!({"version": probe.version, "pageCount": probe.page_count})
}

fn connect_failed(url: &str, probe: &EngineProbe) -> BrowserError {
    BrowserError::ConnectFailed {
        url: url.to_owned(),
        reason: probe
            .error
            .clone()
            .unwrap_or_else(|| "the endpoint did not answer /json/version".to_owned()),
    }
}

/// Reasons a manual yield may name; kept for logging and tests.
#[allow(dead_code)]
pub(crate) fn yield_reason_name(reason: YieldReason) -> &'static str {
    reason.as_str()
}

#[cfg(test)]
#[path = "core_test.rs"]
mod tests;
