//! In-memory session registry.
//!
//! One [`Session`] per registered MCP session. Registration is idempotent: an
//! MCP that reconnects after a gateway restart registers again and keeps the
//! same derived token, so monitor URLs survive both sides.
//!
//! Every session owns a broadcast channel. The WebSocket viewers subscribe to
//! it; mutating calls publish the resulting snapshot (`tabs`, `agent`) so all
//! viewers converge without polling. A slow viewer is allowed to lag and drop
//! intermediate messages (tokio broadcasts report `Lagged`); it re-syncs from
//! the next snapshot.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{broadcast, RwLock};

use crate::ownership::{OwnershipHandle, OwnershipStore};
use crate::protocol::{AgentState, Caps, PageRef, ServerMessage, TabInfo};
use crate::stream::{self, Frame, StreamHandle, FRAME_CAPACITY};
use crate::token;

/// Per-viewer queue depth before tokio reports `Lagged`. Frames are not queued
/// here (screencast frames keep only the newest one in the CDP stage); this
/// channel only carries state snapshots.
pub const BROADCAST_CAPACITY: usize = 256;

/// Snapshot of one registered session.
///
/// Cloning is cheap (strings, a broadcast sender and the ownership handle);
/// handlers work on clones and never hold the store lock while sending.
#[derive(Clone)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub cdp_ws: String,
    pub token: String,
    pub pages: Vec<PageRef>,
    /// Active tab for the tab strip; `None` falls back to the first page.
    pub active_page: Option<String>,
    pub agent_state: AgentState,
    /// Who currently writes this page (README §6.2). The handle reads the live
    /// state from the ownership driver; the snapshot stored on the session is
    /// deliberately not duplicated here.
    pub ownership: OwnershipHandle,
    tx: broadcast::Sender<ServerMessage>,
    frames: broadcast::Sender<Arc<Frame>>,
    /// Newest frame per page, replayed to a viewer that connects or switches to
    /// the page, so a static page does not show a blank canvas until its next
    /// damage event.
    ///
    /// Frames belong to a page, not to the session: clearing the cache on every
    /// tab switch would trade one blank canvas for another when the viewer comes
    /// back to a page whose screencast has not damaged yet.
    last_frames: Arc<Mutex<HashMap<String, Arc<Frame>>>>,
}

impl Session {
    #[allow(clippy::too_many_arguments)]
    fn new(
        id: String,
        name: String,
        cdp_ws: String,
        token: String,
        pages: Vec<PageRef>,
        agent_state: AgentState,
        tx: broadcast::Sender<ServerMessage>,
        ownership: OwnershipHandle,
    ) -> Self {
        let (frames, _frame_rx) = broadcast::channel(FRAME_CAPACITY);
        Self {
            id,
            name,
            cdp_ws,
            token,
            pages,
            active_page: None,
            agent_state,
            ownership,
            tx,
            frames,
            last_frames: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<ServerMessage> {
        self.tx.subscribe()
    }

    /// Newest-first JPEG stream; a lagging viewer converges on the last frame.
    #[must_use]
    pub fn subscribe_frames(&self) -> broadcast::Receiver<Arc<Frame>> {
        self.frames.subscribe()
    }

    /// Newest frame cached for `page_id`.
    ///
    /// Used for the first paint of a fresh viewer and for the tab switch that
    /// re-enters a page whose screencast is still waiting for damage.
    #[must_use]
    pub fn cached_frame(&self, page_id: &str) -> Option<Arc<Frame>> {
        self.last_frames
            .lock()
            .expect("frame cache lock poisoned")
            .get(page_id)
            .cloned()
    }

    pub(crate) fn frame_sender(&self) -> broadcast::Sender<Arc<Frame>> {
        self.frames.clone()
    }

    pub(crate) fn frame_cache(&self) -> Arc<Mutex<HashMap<String, Arc<Frame>>>> {
        self.last_frames.clone()
    }

    pub(crate) fn message_sender(&self) -> broadcast::Sender<ServerMessage> {
        self.tx.clone()
    }

    /// Publishes a snapshot to every viewer. Sending without receivers is not
    /// an error: nobody is watching.
    pub fn broadcast(&self, message: ServerMessage) {
        let _ = self.tx.send(message);
    }

    /// Page that the tab strip marks active; falls back to the first page.
    #[must_use]
    pub fn effective_active(&self) -> Option<&str> {
        match self.active_page.as_deref() {
            Some(active) if self.pages.iter().any(|page| page.target_id == active) => Some(active),
            _ => self.pages.first().map(|page| page.target_id.as_str()),
        }
    }

    /// Marks `page_id` as active if the session still owns it.
    pub fn set_active_page(&mut self, page_id: &str) -> bool {
        if self.pages.iter().any(|page| page.target_id == page_id) {
            self.active_page = Some(page_id.to_owned());
            true
        } else {
            false
        }
    }

    #[must_use]
    pub fn tabs(&self) -> Vec<TabInfo> {
        let active = self.effective_active();
        self.pages
            .iter()
            .map(|page| TabInfo {
                id: page.target_id.clone(),
                url: page.url.clone(),
                title: page.title.clone(),
                active: active == Some(page.target_id.as_str()),
            })
            .collect()
    }

    #[must_use]
    pub fn tabs_message(&self) -> ServerMessage {
        ServerMessage::Tabs { tabs: self.tabs() }
    }

    /// First frame after a viewer connects.
    ///
    /// `interactive` describes the current writer: only a gateway-held page
    /// accepts mouse/keyboard input (README §6.3). It is read from the live
    /// ownership snapshot rather than from the session, because ownership
    /// changes without the session table changing. `caps` describes what the
    /// gateway can request from the MCP at all: diagnostics need a controller
    /// connection, which stage F wires, so the capability is always advertised
    /// and the drawer itself follows `interactive`.
    #[must_use]
    pub fn hello_message(&self) -> ServerMessage {
        let snapshot = self.ownership.snapshot();
        ServerMessage::Hello {
            session: self.id.clone(),
            name: self.name.clone(),
            agent: self.agent_state,
            interactive: snapshot.interactive(),
            caps: Caps {
                new_tab: true,
                diagnostics: true,
            },
        }
    }

    /// Handoff banner state for a viewer that connects mid-request.
    ///
    /// The handoff is broadcast on transitions; a viewer that reconnects while
    /// the request is still pending would otherwise have to wait for the next
    /// one to learn that a human is being asked for help.
    #[must_use]
    pub fn handoff_message(&self) -> Option<ServerMessage> {
        self.ownership
            .snapshot()
            .handoff
            .map(|handoff| ServerMessage::Handoff {
                active: true,
                prompt: Some(handoff.prompt),
                request_id: Some(handoff.request_id),
            })
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Session")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("cdp_ws", &self.cdp_ws)
            .field("token", &"[redacted]")
            .field("pages", &self.pages.len())
            .field("agent_state", &self.agent_state)
            .field("ownership", &self.ownership)
            .field("frames", &self.frames.receiver_count())
            .finish()
    }
}

/// Session table. Tokens are derived from the gateway secret, never stored in
/// plaintext form anywhere else.
///
/// Each session also owns one [`StreamHandle`]; the supervisor behind it starts
/// the read-only CDP stream on the first viewer and stops it on the last.
///
/// The store also owns the [`OwnershipStore`]: a session's driver outlives any
/// single registration, so it lives with the session table rather than with the
/// registry connection that created it.
pub struct SessionStore {
    secret: Arc<[u8]>,
    sessions: RwLock<HashMap<String, Session>>,
    streams: RwLock<HashMap<String, StreamHandle>>,
    ownership: Arc<OwnershipStore>,
}

impl SessionStore {
    pub fn new(secret: impl Into<Arc<[u8]>>, ownership: Arc<OwnershipStore>) -> Self {
        Self {
            secret: secret.into(),
            sessions: RwLock::new(HashMap::new()),
            streams: RwLock::new(HashMap::new()),
            ownership,
        }
    }

    /// Inserts or refreshes a session.
    ///
    /// Existing sessions keep their broadcast sender, active tab and ownership
    /// driver, so an MCP reconnect does not silently detach open viewers or drop
    /// a human who is holding the page. The token is always re-derived from
    /// `(secret, id)`; the driver is told about the new endpoint, agent state and
    /// active page instead of being restarted.
    pub async fn register(
        &self,
        id: impl Into<String>,
        name: impl Into<String>,
        cdp_ws: impl Into<String>,
        pages: Vec<PageRef>,
        agent_state: AgentState,
    ) -> Session {
        let id = id.into();
        let name = name.into();
        let cdp_ws = cdp_ws.into();
        let token = token::derive_token(&self.secret, &id);
        let mut sessions = self.sessions.write().await;

        let mut state_changed = false;
        let mut created = false;
        let mut cdp_changed = false;
        let snapshot = match sessions.get_mut(&id) {
            Some(existing) => {
                state_changed = existing.agent_state != agent_state;
                cdp_changed = existing.cdp_ws != cdp_ws;
                existing.name = name;
                existing.cdp_ws = cdp_ws;
                existing.pages = pages;
                existing.agent_state = agent_state;
                existing.token = token;
                existing.ensure_active_page();
                if cdp_changed {
                    existing.ownership.cdp_url(existing.cdp_ws.clone());
                }
                existing.ownership.agent_state(agent_state);
                existing
                    .ownership
                    .page(existing.effective_active().map(str::to_owned));
                existing.clone()
            }
            None => {
                created = true;
                // The channel is created before the session so the ownership
                // driver can broadcast into it from its first event.
                let (tx, _receiver) = broadcast::channel(BROADCAST_CAPACITY);
                let page = pages.first().map(|page| page.target_id.clone());
                let ownership = self.ownership.spawn(
                    id.clone(),
                    name.clone(),
                    cdp_ws.clone(),
                    agent_state,
                    page,
                    tx.clone(),
                );
                let session = Session::new(
                    id.clone(),
                    name,
                    cdp_ws,
                    token,
                    pages,
                    agent_state,
                    tx,
                    ownership,
                );
                sessions.insert(id.clone(), session.clone());
                session
            }
        };

        drop(sessions);

        let mut streams = self.streams.write().await;
        match streams.get(&id) {
            Some(handle) if !created => {
                if cdp_changed {
                    handle.cdp_url(snapshot.cdp_ws.clone());
                }
                handle.pages(snapshot.pages.clone());
                handle.active(snapshot.active_page.clone());
            }
            _ => {
                streams.insert(id, stream::spawn(&snapshot));
            }
        }
        drop(streams);

        snapshot.broadcast(snapshot.tabs_message());
        if state_changed {
            snapshot.broadcast(ServerMessage::Agent { state: agent_state });
        }
        snapshot
    }

    pub async fn update_pages(&self, id: &str, pages: Vec<PageRef>) -> Option<Session> {
        let mut sessions = self.sessions.write().await;
        let session = sessions.get_mut(id)?;
        session.pages = pages;
        session.ensure_active_page();
        session
            .ownership
            .page(session.effective_active().map(str::to_owned));
        let snapshot = session.clone();
        drop(sessions);

        if let Some(handle) = self.streams.read().await.get(id) {
            handle.pages(snapshot.pages.clone());
            handle.active(snapshot.active_page.clone());
        }

        snapshot.broadcast(snapshot.tabs_message());
        Some(snapshot)
    }

    pub async fn set_state(&self, id: &str, state: AgentState) -> Option<Session> {
        let mut sessions = self.sessions.write().await;
        let session = sessions.get_mut(id)?;
        session.agent_state = state;
        session.ownership.agent_state(state);
        let snapshot = session.clone();
        drop(sessions);
        snapshot.broadcast(ServerMessage::Agent { state });
        Some(snapshot)
    }

    /// Updates the active tab for the session.
    ///
    /// The active page is session-wide: the screencast supervisor follows it,
    /// so every viewer must see the same tab strip or it would paint frames
    /// from a page its own strip still marks inactive. The new snapshot is
    /// broadcast to all viewers for that reason.
    pub async fn set_active_page(&self, id: &str, page_id: &str) -> Option<Session> {
        let mut sessions = self.sessions.write().await;
        let session = sessions.get_mut(id)?;
        if !session.set_active_page(page_id) {
            return None;
        }
        // The ownership driver follows the session's active page: while the
        // human holds the page, a switch closes the old controller and opens the
        // new one (README §6.2: switching swaps connections).
        session.ownership.page(Some(page_id.to_owned()));
        let snapshot = session.clone();
        drop(sessions);

        if let Some(handle) = self.streams.read().await.get(id) {
            handle.active(snapshot.active_page.clone());
        }
        snapshot.broadcast(snapshot.tabs_message());
        Some(snapshot)
    }

    /// Tells the stream supervisor and the ownership driver that one more
    /// monitor socket is watching.
    pub async fn viewer_joined(&self, id: &str) {
        let session = self.sessions.read().await.get(id).cloned();
        if let Some(session) = session {
            session.ownership.viewer_joined();
        }
        if let Some(handle) = self.streams.read().await.get(id) {
            handle.viewer_joined();
        }
    }

    /// Tells the stream supervisor and the ownership driver that one monitor
    /// socket is gone. The driver leaves `HumanHolds` when the last one goes.
    pub async fn viewer_left(&self, id: &str) {
        let session = self.sessions.read().await.get(id).cloned();
        if let Some(session) = session {
            session.ownership.viewer_left();
        }
        if let Some(handle) = self.streams.read().await.get(id) {
            handle.viewer_left();
        }
    }

    pub async fn unregister(&self, id: &str) -> Option<Session> {
        // Drop the supervisor first: its page connection must close before the
        // session disappears, so the next MCP connection starts from a clean
        // viewer state. The ownership driver is stopped after that; dropping it
        // closes the controller, which is what the MCP waits for.
        self.streams.write().await.remove(id);
        let removed = self.sessions.write().await.remove(id);
        self.ownership.remove(id);
        removed
    }

    pub async fn get(&self, id: &str) -> Option<Session> {
        self.sessions.read().await.get(id).cloned()
    }

    /// Resolves a monitor token. Every entry is compared in constant time and
    /// the scan does not stop at the first match, so the token does not become
    /// a timing oracle for session ids.
    pub async fn lookup_by_token(&self, candidate: &str) -> Option<Session> {
        let sessions = self.sessions.read().await;
        let mut found = None;
        for session in sessions.values() {
            if token::constant_time_eq(session.token.as_bytes(), candidate.as_bytes()) {
                found = Some(session.clone());
            }
        }
        found
    }

    /// All sessions, sorted by id for a stable page.
    pub async fn list(&self) -> Vec<Session> {
        let mut sessions: Vec<Session> = self.sessions.read().await.values().cloned().collect();
        sessions.sort_by(|left, right| left.id.cmp(&right.id));
        sessions
    }

    pub async fn len(&self) -> usize {
        self.sessions.read().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.sessions.read().await.is_empty()
    }
}

#[cfg(test)]
impl SessionStore {
    /// Session table wired to an ownership store nobody else listens to.
    ///
    /// For unit tests that never attach a controller: the driver still runs, so
    /// its state machine behaves normally, but registry messages have no reader.
    /// Tests that observe yield/reclaim traffic use [`Self::for_tests_with`] and
    /// share their registry fixture's handle.
    pub fn for_tests(secret: impl Into<Arc<[u8]>>) -> Self {
        Self::for_tests_with(secret, Arc::new(crate::registry::RegistryHandle::new()))
    }

    pub fn for_tests_with(
        secret: impl Into<Arc<[u8]>>,
        registry: Arc<crate::registry::RegistryHandle>,
    ) -> Self {
        Self::new(
            secret,
            Arc::new(OwnershipStore::new(
                crate::ownership::Settings::default(),
                registry,
            )),
        )
    }
}

impl Session {
    fn ensure_active_page(&mut self) {
        let still_present = self
            .active_page
            .as_deref()
            .is_some_and(|active| self.pages.iter().any(|page| page.target_id == active));
        if !still_present {
            self.active_page = None;
        }
        // Kitesurf's `/json/list` order is not stable. Without a pinned tab the
        // effective page follows `pages.first()`, so a reorder would swap the
        // screencast — and, while the human holds the page, close and reopen the
        // controller — on every poll. Pin the first page until it disappears.
        if self.active_page.is_none() {
            self.active_page = self.pages.first().map(|page| page.target_id.clone());
        }
    }
}

#[cfg(test)]
#[path = "session_test.rs"]
mod tests;
