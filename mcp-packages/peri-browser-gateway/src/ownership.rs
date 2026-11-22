//! Human-control ownership: the single writer of a page, handed between the MCP
//! and the gateway (README §6.2).
//!
//! States are exactly `AgentDriving`, `YieldPending` and `HumanHolds`. The MCP
//! holds the page controller in `AgentDriving`; the gateway holds it in
//! `HumanHolds`; `YieldPending` is the window between a request and the MCP's
//! acknowledgement, during which nobody writes. There is never a moment with two
//! controllers: Kitesurf refuses a second one (HTTP 409), and the driver opens
//! exactly one connection per session.
//!
//! The module has two halves:
//!
//! - [`Machine`]: a synchronous, timer-free state machine. Every transition is a
//!   pure function of `(state, event, now)` that returns [`Effect`]s, which
//!   makes the README §6.2 diagram testable without a browser or a clock.
//! - The driver ([`OwnershipStore::spawn`]): owns the controller connection, the
//!   registry handle and the session broadcast channel, feeds events into the
//!   machine and executes its effects.
//!
//! Timers are deadlines carried by the machine (`idle_deadline`,
//! `yield_deadline`, handoff deadline) and a single `Tick` event; the driver
//! sleeps until the next deadline. No task is spawned per timer, so a stale
//! timer can never fire into a later state.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures::future::BoxFuture;
use serde_json::Value;
use tokio::sync::{broadcast, mpsc};
use tokio::time::{sleep_until, Instant};
use tracing::{debug, info, warn};

use crate::cdp::{self, CdpError, CdpEvent, PageController, Viewport};
use crate::protocol::{
    AgentState, Caps, ConsoleEntry, GatewayMessage, HumanStatus, KeyEventKind, MouseButton,
    MouseEventKind, NetworkEntry, ServerMessage, YieldReason,
};
use crate::registry::RegistryHandle;

/// How many console entries and network requests the driver keeps.
pub const DIAGNOSTICS_CAPACITY: usize = 200;

/// How many visited documents a held page remembers for back/forward.
const VISIT_LIMIT: usize = 100;

/// A page-driven navigation is reported by the tab list before the document
/// commits, so the first read can still see the old URL. The hint is retried
/// at this interval, up to [`NAVIGATION_HINT_TRIES`] times, until the machine
/// sees the new document or Kitesurf's late frame event arrives.
const NAVIGATION_HINT_RETRY: Duration = Duration::from_millis(500);
const NAVIGATION_HINT_TRIES: usize = 12;

/// Execution-boundary timeouts (CLI/env, README §6.2 and §10).
#[derive(Debug, Clone, Copy)]
pub struct Settings {
    /// Agent idle time after which the gateway asks for the controller.
    pub idle_yield: Duration,
    /// Manual takeover wait before the viewer is told the yield is pending.
    pub yield_timeout: Duration,
    /// Human handoff wait before the MCP receives `timeout`.
    pub request_human_timeout: Duration,
    /// Whether an agent is connected to this session at all. `--dev` runs
    /// without an MCP peer, so nothing can answer a yield: the human takes the
    /// page directly and no idle timer fires (README §6.1, §11).
    pub has_peer: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            idle_yield: Duration::from_millis(crate::config::DEFAULT_IDLE_YIELD_MS),
            yield_timeout: Duration::from_millis(crate::config::DEFAULT_YIELD_TIMEOUT_MS),
            request_human_timeout: Duration::from_millis(
                crate::config::DEFAULT_REQUEST_HUMAN_TIMEOUT_MS,
            ),
            has_peer: true,
        }
    }
}

impl Settings {
    /// Timeouts from the resolved CLI/environment configuration (README §10).
    #[must_use]
    pub fn from_config(config: &crate::config::Config) -> Self {
        Self {
            idle_yield: Duration::from_millis(config.idle_yield_ms),
            yield_timeout: Duration::from_millis(config.yield_timeout_ms),
            request_human_timeout: Duration::from_millis(config.request_human_timeout_ms),
            has_peer: !config.dev,
        }
    }
}

/// Ownership of one session's page (README §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OwnershipState {
    /// The MCP holds the page controller; the monitor is read-only.
    #[default]
    AgentDriving,
    /// The gateway asked for the controller and waits for `yield_ok`.
    YieldPending,
    /// The gateway holds the controller; human input is forwarded to it.
    HumanHolds,
}

impl OwnershipState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentDriving => "agent_driving",
            Self::YieldPending => "yield_pending",
            Self::HumanHolds => "human_holds",
        }
    }
}

/// One input action forwarded to the controller while the human holds it.
#[derive(Debug, Clone, PartialEq)]
pub enum ControllerCommand {
    Mouse {
        event: MouseEventKind,
        x: f64,
        y: f64,
        button: Option<MouseButton>,
        click_count: u32,
        modifiers: u32,
    },
    Wheel {
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
    },
    Key {
        event: KeyEventKind,
        key: String,
        code: String,
        text: Option<String>,
        modifiers: u32,
    },
    Text(String),
    Navigate(String),
    History(i64),
}

/// Inputs to the state machine.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    AgentState(AgentState),
    ViewerJoined,
    ViewerLeft,
    Takeover,
    Release,
    /// The viewer hands the page to Chrome DevTools. Unlike [`Event::Release`]
    /// this is not a complaint about missing control: when the agent owns the
    /// page there is nothing of ours to close, so it stays silent.
    ReleaseForDevtools,
    /// The session's active page changed (tab switch, page closed, re-register).
    Page(Option<String>),
    /// The tab list reports this URL for the active page. Kitesurf's
    /// `Page.frameNavigated` can lag more than ten seconds behind the document,
    /// so the poll is the prompt signal that a page-driven navigation happened.
    PageUrl(String),
    /// A back/forward step loaded its document; the machine moves the index.
    HistoryArrived {
        delta: i64,
    },
    YieldOk(String),
    Reclaim(String),
    HumanRequest {
        request_id: String,
        page_id: String,
        prompt: String,
        /// The MCP's own bound, logged for diagnosis; the gateway waits its
        /// configured `request_human_timeout`.
        timeout_ms: u64,
    },
    Answer {
        request_id: String,
        accepted: bool,
    },
    /// A controller connection attempt finished; `Some` carries the error.
    ControllerOpened {
        page_id: String,
        error: Option<String>,
    },
    ControllerClosed(String),
    Navigation(cdp::NavigationState),
    /// The page committed a navigation nobody asked the gateway for: a link, a
    /// form or a script. Only the main frame reports one.
    FrameNavigated {
        url: String,
    },
    Console(ConsoleEntry),
    NetworkStarted {
        request_id: String,
        entry: NetworkEntry,
    },
    NetworkStatus {
        request_id: String,
        status: u16,
    },
    /// Human input from a viewer; only dispatched while the gateway writes.
    Input(ControllerCommand),
    /// The MCP re-registered with a new CDP endpoint. Transport state, not
    /// ownership: the driver updates it and the machine ignores it.
    CdpUrl(String),
    Tick,
}

/// Side effects the driver executes in order.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Effect {
    Yield {
        page_id: String,
        reason: YieldReason,
    },
    OpenController {
        page_id: String,
    },
    CloseController,
    Broadcast(ServerMessage),
    Reclaimed {
        page_id: String,
        ok: bool,
    },
    HumanResult {
        request_id: String,
        status: HumanStatus,
    },
    Dispatch(ControllerCommand),
    /// Steps the visit stack the machine kept for this page. It is resolved
    /// here, not by the controller, because Kitesurf's own history never learns
    /// about the navigations the gateway drives.
    History {
        delta: i64,
    },
    EnableDiagnostics,
    RefreshViewport,
    RefreshNavigation,
}

struct Handoff {
    request_id: String,
    prompt: String,
    /// Cleared once the human accepts: the timeout bounds "nobody answered",
    /// not "the human is still working".
    deadline: Option<Instant>,
    accepted: bool,
}

/// The synchronous ownership machine.
pub struct Machine {
    settings: Settings,
    session_id: String,
    name: String,
    state: OwnershipState,
    /// Page a yield is for, or the page the human holds.
    page: Option<String>,
    /// Page whose controller is currently open, when one is.
    held_page: Option<String>,
    agent: AgentState,
    viewers: usize,
    has_controller: bool,
    idle_deadline: Option<Instant>,
    yield_deadline: Option<Instant>,
    handoff: Option<Handoff>,
    navigation: cdp::NavigationState,
    /// URLs this page visited while the gateway watched, oldest first.
    visits: Vec<String>,
    /// Index of the document currently on screen inside [`Self::visits`].
    visit: usize,
    /// A document the tab list reported that no refresh has confirmed yet.
    hint: Option<String>,
    hint_deadline: Option<Instant>,
    hint_tries: usize,
    console: VecDeque<ConsoleEntry>,
    network_order: VecDeque<String>,
    network: HashMap<String, NetworkEntry>,
}

impl Machine {
    #[must_use]
    pub fn new(
        settings: Settings,
        session_id: impl Into<String>,
        name: impl Into<String>,
        agent: AgentState,
        page: Option<String>,
    ) -> Self {
        Self {
            settings,
            session_id: session_id.into(),
            name: name.into(),
            state: OwnershipState::AgentDriving,
            page,
            held_page: None,
            agent,
            viewers: 0,
            has_controller: false,
            idle_deadline: None,
            yield_deadline: None,
            handoff: None,
            navigation: cdp::NavigationState::default(),
            visits: Vec::new(),
            visit: 0,
            hint: None,
            hint_deadline: None,
            hint_tries: 0,
            console: VecDeque::new(),
            network_order: VecDeque::new(),
            network: HashMap::new(),
        }
    }

    #[must_use]
    pub fn state(&self) -> OwnershipState {
        self.state
    }

    #[must_use]
    pub fn page(&self) -> Option<&str> {
        self.page.as_deref()
    }

    #[must_use]
    pub fn has_controller(&self) -> bool {
        self.has_controller
    }

    #[must_use]
    pub fn is_interactive(&self) -> bool {
        self.state == OwnershipState::HumanHolds && self.has_controller
    }

    #[must_use]
    pub fn pending_handoff(&self) -> Option<&str> {
        self.handoff
            .as_ref()
            .map(|handoff| handoff.request_id.as_str())
    }

    #[must_use]
    pub fn handoff_accepted(&self) -> bool {
        self.handoff
            .as_ref()
            .is_some_and(|handoff| handoff.accepted)
    }

    /// Pending handoff as a viewer needs to see it, if any.
    #[must_use]
    pub fn handoff_view(&self) -> Option<PendingHandoff> {
        self.handoff.as_ref().map(|handoff| PendingHandoff {
            request_id: handoff.request_id.clone(),
            prompt: handoff.prompt.clone(),
        })
    }

    #[must_use]
    pub fn navigation(&self) -> &cdp::NavigationState {
        &self.navigation
    }

    #[must_use]
    pub fn console_entries(&self) -> Vec<ConsoleEntry> {
        self.console.iter().cloned().collect()
    }

    #[must_use]
    pub fn network_entries(&self) -> Vec<NetworkEntry> {
        self.network_order
            .iter()
            .filter_map(|request_id| self.network.get(request_id).cloned())
            .collect()
    }

    /// Earliest pending deadline; the driver sleeps until it.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        let handoff = self.handoff.as_ref().and_then(|handoff| handoff.deadline);
        [
            self.idle_deadline,
            self.yield_deadline,
            self.hint_deadline,
            handoff,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Applies one event and returns the effects to execute.
    ///
    /// `pub(crate)` rather than `pub`: [`Effect`] is driver-internal, and only
    /// the driver and this module's tests drive the machine.
    pub(crate) fn on(&mut self, event: Event, now: Instant) -> Vec<Effect> {
        match event {
            Event::AgentState(state) => self.on_agent_state(state, now),
            Event::ViewerJoined => {
                self.viewers = self.viewers.saturating_add(1);
                self.arm_idle(now);
                Vec::new()
            }
            Event::ViewerLeft => self.on_viewer_left(now),
            Event::Takeover => self.on_takeover(now),
            Event::Release => self.on_release(),
            Event::ReleaseForDevtools => self.on_release_for_devtools(),
            Event::Page(page) => self.on_page(page),
            Event::YieldOk(page_id) => self.on_yield_ok(page_id),
            Event::Reclaim(page_id) => self.on_reclaim(page_id),
            Event::HumanRequest {
                request_id,
                page_id,
                prompt,
                timeout_ms,
            } => self.on_human_request(request_id, page_id, prompt, timeout_ms, now),
            Event::Answer {
                request_id,
                accepted,
            } => self.on_answer(request_id, accepted, now),
            Event::ControllerOpened { page_id, error } => self.on_controller_opened(page_id, error),
            Event::ControllerClosed(reason) => self.on_controller_closed(reason),
            Event::Navigation(state) => {
                self.record_visit(&state.url);
                let mut state = state;
                // Kitesurf's own history never learns about the navigations the
                // gateway drives, so the buttons follow the stack kept here.
                state.can_go_back = self.visit > 0;
                state.can_go_forward = self.visit + 1 < self.visits.len();
                // The poll and the frame event can both notice one navigation;
                // only a real change is worth a message.
                let changed = state != self.navigation;
                if self.hint.as_deref() == Some(state.url.as_str()) {
                    self.clear_hint();
                }
                self.navigation = state;
                if self.state == OwnershipState::HumanHolds && changed {
                    vec![Effect::Broadcast(self.nav_message())]
                } else {
                    Vec::new()
                }
            }
            Event::FrameNavigated { url } => {
                // A frame event is the committed document, so it can be
                // remembered even if the read that follows fails.
                if self.is_interactive() {
                    self.record_visit(&url);
                }
                self.refresh_if_new(&url, now)
            }
            Event::PageUrl(url) => self.refresh_if_new(&url, now),
            Event::HistoryArrived { delta } => {
                // The document is there; move the index and read it back so the
                // title and the buttons describe what is on screen.
                if self.step_history(delta).is_some() {
                    vec![Effect::RefreshNavigation]
                } else {
                    Vec::new()
                }
            }
            Event::Console(entry) => self.on_console(entry),
            Event::NetworkStarted { request_id, entry } => {
                self.on_network_started(request_id, entry)
            }
            Event::NetworkStatus { request_id, status } => {
                self.on_network_status(request_id, status)
            }
            Event::Input(command) => {
                // A viewer that raced the handoff sends its input here; without
                // the controller there is nothing to write to, so it is dropped
                // (the viewer was told the page is read-only at send time).
                if !self.accepts_input() {
                    Vec::new()
                } else if let ControllerCommand::History(delta) = command {
                    vec![Effect::History { delta }]
                } else {
                    vec![Effect::Dispatch(command)]
                }
            }
            // Transport, owned by the driver, not by the state machine.
            Event::CdpUrl(_) => Vec::new(),
            Event::Tick => self.on_tick(now),
        }
    }

    fn hello(&self) -> ServerMessage {
        ServerMessage::Hello {
            session: self.session_id.clone(),
            name: self.name.clone(),
            agent: self.agent,
            interactive: self.is_interactive(),
            caps: Caps {
                new_tab: true,
                diagnostics: true,
            },
        }
    }

    fn nav_message(&self) -> ServerMessage {
        ServerMessage::Nav {
            page_id: self.page.clone().unwrap_or_default(),
            url: self.navigation.url.clone(),
            title: self.navigation.title.clone(),
            can_go_back: self.navigation.can_go_back,
            can_go_forward: self.navigation.can_go_forward,
        }
    }

    fn diagnostics_page(&self) -> String {
        self.held_page
            .clone()
            .or_else(|| self.page.clone())
            .unwrap_or_default()
    }

    /// Idle yield only makes sense while a monitor is open: the point is that a
    /// human can type without pressing takeover. With nobody watching, taking
    /// the controller would only add a reclaim round trip to the next agent call.
    fn arm_idle(&mut self, now: Instant) {
        if self.state != OwnershipState::AgentDriving
            || self.agent != AgentState::Idle
            || self.viewers == 0
            || self.page.is_none()
            || self.handoff.is_some()
            || !self.settings.has_peer
        {
            return;
        }
        self.idle_deadline = Some(now + self.settings.idle_yield);
        debug!(
            session = %self.session_id,
            idle_ms = self.settings.idle_yield.as_millis() as u64,
            "idle yield armed"
        );
    }

    fn on_agent_state(&mut self, agent: AgentState, now: Instant) -> Vec<Effect> {
        if self.agent == agent {
            return Vec::new();
        }
        let previous = self.agent;
        self.agent = agent;
        debug!(
            session = %self.session_id,
            from = previous.as_str(),
            to = agent.as_str(),
            "agent state changed"
        );
        match agent {
            AgentState::Driving => {
                self.idle_deadline = None;
                if self.state == OwnershipState::HumanHolds {
                    // The MCP must send `reclaim` before it reconnects. A bare
                    // driving report while the gateway still writes is a protocol
                    // error, reported instead of silently dropping the controller.
                    warn!(
                        session = %self.session_id,
                        "MCP reports driving while the gateway holds the page"
                    );
                }
            }
            AgentState::Idle => self.arm_idle(now),
            AgentState::WaitingForHuman => self.idle_deadline = None,
        }
        Vec::new()
    }

    fn on_viewer_left(&mut self, _now: Instant) -> Vec<Effect> {
        self.viewers = self.viewers.saturating_sub(1);
        if self.viewers > 0 {
            return Vec::new();
        }
        self.idle_deadline = None;
        match self.state {
            OwnershipState::HumanHolds => {
                // README §6.6: a human that disconnects while holding loses the
                // page immediately; the MCP reconnects on its next page call.
                self.leave_human_holds("no monitor is left", true)
            }
            OwnershipState::YieldPending => {
                // Nobody is left to receive the handover: stop asking for it.
                // A late `yield_ok` is answered with `reclaimed`.
                self.state = OwnershipState::AgentDriving;
                self.yield_deadline = None;
                Vec::new()
            }
            OwnershipState::AgentDriving => Vec::new(),
        }
    }

    fn on_takeover(&mut self, now: Instant) -> Vec<Effect> {
        let Some(page) = self.page.clone() else {
            return vec![Effect::Broadcast(ServerMessage::Error {
                code: "unavailable".to_owned(),
                message: "this session has no page to take over".to_owned(),
            })];
        };
        match self.state {
            OwnershipState::HumanHolds => Vec::new(),
            OwnershipState::YieldPending => {
                if !self.settings.has_peer {
                    // The controller open is already in flight; a second request
                    // must not ask a peer that does not exist and arm a timeout
                    // that would report `yield_pending`.
                    debug!(
                        session = %self.session_id,
                        page = %page,
                        "ignoring a repeated takeover while the controller opens"
                    );
                    return Vec::new();
                }
                // A retry (the MCP may have been mid-call when the first request
                // arrived) re-sends the same request and re-arms the report.
                self.yield_deadline = Some(now + self.settings.yield_timeout);
                vec![Effect::Yield {
                    page_id: page,
                    reason: YieldReason::Takeover,
                }]
            }
            OwnershipState::AgentDriving => {
                self.state = OwnershipState::YieldPending;
                if !self.settings.has_peer {
                    // Nobody can answer the yield, and the page has no writer, so
                    // the human takes it directly instead of waiting out a timeout
                    // that would only end in `yield_pending`.
                    self.yield_deadline = None;
                    info!(
                        session = %self.session_id,
                        page = %page,
                        "takeover granted without a yield: no agent is connected"
                    );
                    return vec![Effect::OpenController { page_id: page }];
                }
                self.yield_deadline = Some(now + self.settings.yield_timeout);
                info!(session = %self.session_id, page = %page, "takeover requested");
                vec![Effect::Yield {
                    page_id: page,
                    reason: YieldReason::Takeover,
                }]
            }
        }
    }

    fn on_release(&mut self) -> Vec<Effect> {
        match self.state {
            OwnershipState::HumanHolds => {
                self.leave_human_holds("the human released the page", true)
            }
            OwnershipState::YieldPending => {
                // The MCP may still answer the yield; that late `yield_ok` is
                // answered with `reclaimed` (see `on_yield_ok`).
                self.state = OwnershipState::AgentDriving;
                self.yield_deadline = None;
                self.finish_handoff("the human withdrew before the yield", true)
            }
            OwnershipState::AgentDriving => vec![Effect::Broadcast(ServerMessage::Error {
                code: "read_only".to_owned(),
                message: "the agent holds this page; there is nothing to release".to_owned(),
            })],
        }
    }

    /// Handing the page to Chrome DevTools needs the gateway to stop writing,
    /// but it is not a complaint: an agent-owned page has no controller of
    /// ours to close, and the DevTools frontend is welcome to the slot.
    fn on_release_for_devtools(&mut self) -> Vec<Effect> {
        const REASON: &str = "the page was handed to Chrome DevTools";
        match self.state {
            // The page does not come back to the agent: DevTools writes to it
            // until the viewer takes over again.
            OwnershipState::HumanHolds => self.leave_human_holds(REASON, false),
            OwnershipState::YieldPending => {
                self.state = OwnershipState::AgentDriving;
                self.yield_deadline = None;
                self.finish_handoff(REASON, true)
            }
            OwnershipState::AgentDriving => Vec::new(),
        }
    }

    fn finish_handoff(&mut self, reason: &str, declined: bool) -> Vec<Effect> {
        let Some(handoff) = self.handoff.take() else {
            return Vec::new();
        };
        debug!(
            session = %self.session_id,
            request_id = %handoff.request_id,
            reason,
            "handoff resolved"
        );
        let status = if declined {
            HumanStatus::Declined
        } else {
            HumanStatus::Done
        };
        vec![
            Effect::HumanResult {
                request_id: handoff.request_id,
                status,
            },
            Effect::Broadcast(ServerMessage::Handoff {
                active: false,
                prompt: None,
                request_id: None,
            }),
        ]
    }

    fn on_yield_ok(&mut self, page_id: String) -> Vec<Effect> {
        match self.state {
            OwnershipState::HumanHolds => {
                if self.held_page.as_deref() != Some(page_id.as_str()) {
                    warn!(
                        session = %self.session_id,
                        page = %page_id,
                        held = ?self.held_page,
                        "ignoring a second yield acknowledgement"
                    );
                }
                Vec::new()
            }
            OwnershipState::YieldPending => {
                if self.page.as_deref() != Some(page_id.as_str()) {
                    warn!(
                        session = %self.session_id,
                        page = %page_id,
                        expected = ?self.page,
                        "yield acknowledged for a page this session no longer targets"
                    );
                    self.state = OwnershipState::AgentDriving;
                    self.yield_deadline = None;
                    return vec![Effect::Reclaimed { page_id, ok: true }];
                }
                self.yield_deadline = None;
                info!(session = %self.session_id, page = %page_id, "yield acknowledged; attaching controller");
                vec![Effect::OpenController { page_id }]
            }
            OwnershipState::AgentDriving => {
                // The human withdrew (Release) or the request timed out before
                // this acknowledgement. The page has no writer right now, so tell
                // the MCP it may reconnect instead of leaving it writerless.
                debug!(
                    session = %self.session_id,
                    page = %page_id,
                    "late yield acknowledgement; returning the page"
                );
                vec![Effect::Reclaimed { page_id, ok: true }]
            }
        }
    }

    fn on_reclaim(&mut self, page_id: String) -> Vec<Effect> {
        let mut effects = if self.state == OwnershipState::HumanHolds {
            self.leave_human_holds("the agent reclaimed the page", false)
        } else {
            self.state = OwnershipState::AgentDriving;
            self.idle_deadline = None;
            self.yield_deadline = None;
            self.finish_handoff("the agent reclaimed the page", true)
        };
        effects.push(Effect::Reclaimed { page_id, ok: true });
        effects
    }

    /// Asks for a fresh read when a document nobody asked the gateway for is
    /// on screen. The address bar, the title and the stack come from the state
    /// that read returns, so a hint that lands before the commit cannot invent
    /// a visit. The hint is kept until a read confirms it: the tab list updates
    /// while the old document is still committing.
    fn refresh_if_new(&mut self, url: &str, now: Instant) -> Vec<Effect> {
        // The gateway only writes in HumanHolds, which is also the only state
        // that keeps a controller (and therefore receives frame events).
        if !self.is_interactive() || url.is_empty() {
            return Vec::new();
        }
        if url == self.navigation.url {
            self.clear_hint();
            return Vec::new();
        }
        self.hint = Some(url.to_owned());
        self.hint_deadline = Some(now + NAVIGATION_HINT_RETRY);
        self.hint_tries = 0;
        vec![Effect::RefreshNavigation]
    }

    fn clear_hint(&mut self) {
        self.hint = None;
        self.hint_deadline = None;
        self.hint_tries = 0;
    }

    /// Re-reads while a hinted document has not shown up yet.
    fn retry_hint(&mut self, now: Instant) -> Option<Effect> {
        let hinted = self.hint.clone()?;
        if !self.is_interactive()
            || hinted == self.navigation.url
            || self.hint_tries >= NAVIGATION_HINT_TRIES
        {
            self.clear_hint();
            return None;
        }
        self.hint_tries += 1;
        self.hint_deadline = Some(now + NAVIGATION_HINT_RETRY);
        Some(Effect::RefreshNavigation)
    }

    /// Records the document on screen, dropping the entries a fresh navigation
    /// makes unreachable.
    fn record_visit(&mut self, url: &str) {
        if url.is_empty()
            || self
                .visits
                .get(self.visit)
                .is_some_and(|visit| visit == url)
        {
            return;
        }
        self.visits.truncate(self.visit + 1);
        self.visits.push(url.to_owned());
        self.visit = self.visits.len() - 1;
        if self.visits.len() > VISIT_LIMIT {
            self.visits.remove(0);
            self.visit = self.visit.saturating_sub(1);
        }
    }

    /// Moves through [`Self::visits`] and returns the URL to navigate to.
    /// The URL a history step would load. The index only moves once the
    /// document arrives: Kitesurf refuses commands for a while after a
    /// navigation, and a step that never loads must not shift the stack.
    fn peek_history(&self, delta: i64) -> Option<String> {
        self.history_target(delta)
            .and_then(|target| self.visits.get(target).cloned())
    }

    fn history_target(&self, delta: i64) -> Option<usize> {
        let target = i64::try_from(self.visit).ok()? + delta;
        let target = usize::try_from(target).ok()?;
        (target != self.visit && target < self.visits.len()).then_some(target)
    }

    fn step_history(&mut self, delta: i64) -> Option<String> {
        let target = self.history_target(delta)?;
        self.visit = target;
        self.visits.get(target).cloned()
    }

    fn on_page(&mut self, page: Option<String>) -> Vec<Effect> {
        if self.page == page {
            return Vec::new();
        }
        debug!(
            session = %self.session_id,
            from = ?self.page,
            to = ?page,
            "session page changed"
        );
        self.page = page.clone();
        // The visit stack belongs to the page that was on screen; a tab switch
        // starts a new one, and a hint about the old tab means nothing here.
        self.visits.clear();
        self.visit = 0;
        self.clear_hint();
        if self.state != OwnershipState::HumanHolds {
            return Vec::new();
        }
        // README §6.2: switching tabs while holding swaps connections, it does
        // not share one. Close the old controller before opening the new one.
        let mut effects = Vec::new();
        if page.is_some() {
            effects.push(Effect::CloseController);
            self.has_controller = false;
            self.held_page = None;
            self.clear_diagnostics();
        }
        match page {
            Some(page) => effects.push(Effect::OpenController { page_id: page }),
            // The held page disappeared: `held_page` is still set, so the
            // release reports the right page id to the MCP.
            None => effects.extend(self.leave_human_holds("the held page disappeared", true)),
        }
        effects
    }

    fn on_human_request(
        &mut self,
        request_id: String,
        page_id: String,
        prompt: String,
        timeout_ms: u64,
        now: Instant,
    ) -> Vec<Effect> {
        if self.handoff.is_some() {
            warn!(
                session = %self.session_id,
                request_id = %request_id,
                "ignoring a second handoff request; one is already pending"
            );
            return Vec::new();
        }
        info!(
            session = %self.session_id,
            request_id = %request_id,
            page = %page_id,
            requested_ms = timeout_ms,
            gateway_ms = self.settings.request_human_timeout.as_millis() as u64,
            "human handoff requested"
        );
        let mut effects = vec![Effect::Broadcast(ServerMessage::Handoff {
            active: true,
            prompt: Some(prompt.clone()),
            request_id: Some(request_id.clone()),
        })];
        self.handoff = Some(Handoff {
            request_id,
            prompt,
            deadline: Some(now + self.settings.request_human_timeout),
            accepted: false,
        });
        if self.page.as_deref() != Some(page_id.as_str())
            && self.state != OwnershipState::HumanHolds
        {
            self.page = Some(page_id.clone());
        }
        match self.state {
            OwnershipState::HumanHolds => {}
            OwnershipState::YieldPending => {
                self.yield_deadline = Some(now + self.settings.yield_timeout);
                effects.push(Effect::Yield {
                    page_id,
                    reason: YieldReason::RequestHuman,
                });
            }
            OwnershipState::AgentDriving => {
                self.state = OwnershipState::YieldPending;
                self.yield_deadline = Some(now + self.settings.yield_timeout);
                effects.push(Effect::Yield {
                    page_id,
                    reason: YieldReason::RequestHuman,
                });
            }
        }
        effects
    }

    fn on_answer(&mut self, request_id: String, accepted: bool, now: Instant) -> Vec<Effect> {
        let Some(handoff) = self.handoff.as_ref() else {
            warn!(
                session = %self.session_id,
                request_id = %request_id,
                "answer without a pending handoff"
            );
            return vec![Effect::Broadcast(ServerMessage::Error {
                code: "no_handoff".to_owned(),
                message: "no handoff is waiting for an answer".to_owned(),
            })];
        };
        if handoff.request_id != request_id {
            warn!(
                session = %self.session_id,
                request_id = %request_id,
                pending = %handoff.request_id,
                "ignoring an answer for a different handoff"
            );
            return Vec::new();
        }

        if accepted {
            // "I'll take it": the same yield handshake as a manual takeover,
            // tied to this request. No HumanResult yet: the human is working.
            if let Some(handoff) = self.handoff.as_mut() {
                handoff.accepted = true;
                handoff.deadline = None;
            }
            match self.state {
                OwnershipState::HumanHolds => vec![Effect::RefreshNavigation],
                OwnershipState::AgentDriving | OwnershipState::YieldPending => {
                    let Some(page) = self.page.clone() else {
                        return Vec::new();
                    };
                    self.state = OwnershipState::YieldPending;
                    self.yield_deadline = Some(now + self.settings.yield_timeout);
                    vec![Effect::Yield {
                        page_id: page,
                        reason: YieldReason::RequestHuman,
                    }]
                }
            }
        } else {
            // "Done" (or a decline before taking it): resolve the handoff and
            // hand the page back to the agent.
            let handoff = self.handoff.take().expect("checked above");
            let status = if handoff.accepted || self.state == OwnershipState::HumanHolds {
                HumanStatus::Done
            } else {
                HumanStatus::Declined
            };
            let mut effects = vec![
                Effect::HumanResult {
                    request_id: handoff.request_id,
                    status,
                },
                Effect::Broadcast(ServerMessage::Handoff {
                    active: false,
                    prompt: None,
                    request_id: None,
                }),
            ];
            if self.state == OwnershipState::HumanHolds {
                effects.extend(self.leave_human_holds("the human finished the handoff", true));
            } else {
                self.state = OwnershipState::AgentDriving;
                self.yield_deadline = None;
                self.idle_deadline = None;
            }
            effects
        }
    }

    fn on_controller_opened(&mut self, page_id: String, error: Option<String>) -> Vec<Effect> {
        if let Some(error) = error {
            warn!(session = %self.session_id, page = %page_id, error = %error, "controller attach failed");
            let mut effects = vec![Effect::Broadcast(ServerMessage::Error {
                code: "controller_unavailable".to_owned(),
                message: format!("cannot take the page: {error}"),
            })];
            self.has_controller = false;
            if self.state == OwnershipState::HumanHolds {
                // A failed tab switch left the human without a writer: give the
                // page back so the agent can reconnect.
                effects.extend(self.leave_human_holds("the new page could not be taken", true));
            } else {
                // The MCP yielded and closed its page connection; the gateway
                // could not attach. Nobody writes the page right now, so the MCP
                // is told the handoff failed (`ok: false`) and may reconnect on
                // its next page call.
                self.state = OwnershipState::AgentDriving;
                self.yield_deadline = None;
                self.idle_deadline = None;
                effects.push(Effect::Reclaimed { page_id, ok: false });
            }
            return effects;
        }

        if !matches!(
            self.state,
            OwnershipState::YieldPending | OwnershipState::HumanHolds
        ) {
            // The human gave up while the connection was being opened.
            return vec![Effect::CloseController];
        }
        if self.page.as_deref() != Some(page_id.as_str()) {
            // A tab switch landed while connecting: this controller is for the
            // page the session no longer shows.
            return vec![Effect::CloseController];
        }
        self.has_controller = true;
        self.held_page = Some(page_id.clone());
        self.state = OwnershipState::HumanHolds;
        self.yield_deadline = None;
        info!(session = %self.session_id, page = %page_id, "human holds the page");
        vec![
            Effect::EnableDiagnostics,
            Effect::RefreshViewport,
            Effect::RefreshNavigation,
            Effect::Broadcast(self.hello()),
        ]
    }

    fn on_controller_closed(&mut self, reason: String) -> Vec<Effect> {
        if !self.has_controller {
            return Vec::new();
        }
        warn!(session = %self.session_id, reason = %reason, "controller connection lost");
        let mut effects = vec![Effect::Broadcast(ServerMessage::Error {
            code: "controller_lost".to_owned(),
            message: format!("the page controller closed ({reason}); the page is read-only again"),
        })];
        // The MCP yielded this page to us, so it is writerless now: tell it that
        // it may reconnect instead of waiting for a `reclaimed` that never comes.
        effects.extend(self.leave_human_holds("the controller socket closed", true));
        effects
    }

    fn on_tick(&mut self, now: Instant) -> Vec<Effect> {
        let mut effects = Vec::new();

        if self.hint_deadline.is_some_and(|deadline| now >= deadline) {
            self.hint_deadline = None;
            if let Some(effect) = self.retry_hint(now) {
                effects.push(effect);
            }
        }

        if self.idle_deadline.is_some_and(|deadline| now >= deadline) {
            self.idle_deadline = None;
            if let Some(page) = self.page.clone() {
                if self.state == OwnershipState::AgentDriving
                    && self.agent == AgentState::Idle
                    && self.viewers > 0
                {
                    self.state = OwnershipState::YieldPending;
                    self.yield_deadline = Some(now + self.settings.yield_timeout);
                    info!(session = %self.session_id, page = %page, "idle yield requested");
                    effects.push(Effect::Yield {
                        page_id: page,
                        reason: YieldReason::Idle,
                    });
                }
            }
        }

        if self.yield_deadline.is_some_and(|deadline| now >= deadline) {
            self.yield_deadline = None;
            if self.state == OwnershipState::YieldPending {
                // README §6.2: report `yield_pending` back and stay pending. The
                // gateway never invents HumanHolds before the MCP closes its page
                // connection; a second controller would be refused anyway.
                effects.push(Effect::Broadcast(ServerMessage::Error {
                    code: "yield_pending".to_owned(),
                    message: "the agent is still finishing; the page will be handed over when its call ends".to_owned(),
                }));
            }
        }

        if self
            .handoff
            .as_ref()
            .and_then(|handoff| handoff.deadline)
            .is_some_and(|deadline| now >= deadline)
        {
            let handoff = self.handoff.take().expect("checked above");
            warn!(session = %self.session_id, request_id = %handoff.request_id, "handoff timed out");
            effects.push(Effect::HumanResult {
                request_id: handoff.request_id,
                status: HumanStatus::Timeout,
            });
            effects.push(Effect::Broadcast(ServerMessage::Handoff {
                active: false,
                prompt: None,
                request_id: None,
            }));
            if self.state == OwnershipState::HumanHolds {
                effects.extend(self.leave_human_holds("the handoff timed out", true));
            } else {
                self.state = OwnershipState::AgentDriving;
                self.yield_deadline = None;
            }
        }

        effects
    }

    fn on_console(&mut self, entry: ConsoleEntry) -> Vec<Effect> {
        if self.state != OwnershipState::HumanHolds {
            return Vec::new();
        }
        if self.console.len() == DIAGNOSTICS_CAPACITY {
            self.console.pop_front();
        }
        self.console.push_back(entry.clone());
        vec![Effect::Broadcast(ServerMessage::Console {
            page_id: self.diagnostics_page(),
            entries: vec![entry],
        })]
    }

    fn on_network_started(&mut self, request_id: String, entry: NetworkEntry) -> Vec<Effect> {
        if self.state != OwnershipState::HumanHolds {
            return Vec::new();
        }
        if !self.network.contains_key(&request_id) {
            if self.network_order.len() == DIAGNOSTICS_CAPACITY {
                if let Some(oldest) = self.network_order.pop_front() {
                    self.network.remove(&oldest);
                }
            }
            self.network_order.push_back(request_id.clone());
        }
        self.network.insert(request_id, entry.clone());
        vec![Effect::Broadcast(ServerMessage::Network {
            page_id: self.diagnostics_page(),
            entries: vec![entry],
        })]
    }

    fn on_network_status(&mut self, request_id: String, status: u16) -> Vec<Effect> {
        if self.state != OwnershipState::HumanHolds {
            return Vec::new();
        }
        let Some(entry) = self.network.get_mut(&request_id) else {
            return Vec::new();
        };
        entry.status = Some(status);
        let updated = entry.clone();
        vec![Effect::Broadcast(ServerMessage::Network {
            page_id: self.diagnostics_page(),
            entries: vec![updated],
        })]
    }

    fn clear_diagnostics(&mut self) {
        self.console.clear();
        self.network.clear();
        self.network_order.clear();
    }

    /// Leaves `HumanHolds`: closes the controller, clears diagnostics, tells the
    /// viewers they are read-only again and (optionally) tells the MCP it may
    /// reconnect.
    fn leave_human_holds(&mut self, reason: &str, notify_mcp: bool) -> Vec<Effect> {
        let page = self
            .held_page
            .clone()
            .or_else(|| self.page.clone())
            .unwrap_or_default();
        let page_id = if page.is_empty() { None } else { Some(page) };
        let mut effects = Vec::new();
        if self.has_controller {
            effects.push(Effect::CloseController);
            self.has_controller = false;
        }
        self.held_page = None;
        self.state = OwnershipState::AgentDriving;
        self.idle_deadline = None;
        self.yield_deadline = None;
        self.clear_hint();
        self.clear_diagnostics();
        if let Some(page_id) = page_id.clone() {
            // Empty entries clear the drawer; the read-only `hello` explains why.
            effects.push(Effect::Broadcast(ServerMessage::Console {
                page_id: page_id.clone(),
                entries: Vec::new(),
            }));
            effects.push(Effect::Broadcast(ServerMessage::Network {
                page_id,
                entries: Vec::new(),
            }));
        }
        effects.push(Effect::Broadcast(self.hello()));
        if notify_mcp {
            if let Some(page_id) = page_id {
                effects.push(Effect::Reclaimed { page_id, ok: true });
            }
        }
        if let Some(handoff) = self.handoff.take() {
            let status = if handoff.accepted {
                HumanStatus::Done
            } else {
                HumanStatus::Declined
            };
            effects.push(Effect::HumanResult {
                request_id: handoff.request_id,
                status,
            });
            effects.push(Effect::Broadcast(ServerMessage::Handoff {
                active: false,
                prompt: None,
                request_id: None,
            }));
        }
        info!(session = %self.session_id, reason, "left human ownership");
        effects
    }

    /// Human activity is only meaningful while the gateway holds the writer.
    #[must_use]
    pub fn accepts_input(&self) -> bool {
        self.is_interactive()
    }
}

/// Builds a [`ConsoleEntry`] from `Runtime.consoleAPICalled`.
#[must_use]
pub fn console_entry(params: &Value, now_ms: u64) -> Option<ConsoleEntry> {
    let level = params.get("type")?.as_str()?;
    let text = params
        .get("args")
        .and_then(Value::as_array)
        .map(|args| {
            args.iter()
                .map(console_argument)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    Some(ConsoleEntry {
        level: level.to_owned(),
        text,
        ts: now_ms,
    })
}

fn console_argument(value: &Value) -> String {
    match value.get("value") {
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => value
            .get("description")
            .and_then(Value::as_str)
            .or_else(|| value.get("type").and_then(Value::as_str))
            .unwrap_or_default()
            .to_owned(),
    }
}

/// Builds a [`ConsoleEntry`] from `Runtime.exceptionThrown`.
#[must_use]
pub fn exception_entry(params: &Value, now_ms: u64) -> Option<ConsoleEntry> {
    let details = params.get("exceptionDetails")?;
    let text = details
        .get("exception")
        .and_then(|exception| exception.get("description"))
        .and_then(Value::as_str)
        .or_else(|| details.get("text").and_then(Value::as_str))
        .unwrap_or("uncaught exception");
    Some(ConsoleEntry {
        level: "error".to_owned(),
        text: text.to_owned(),
        ts: now_ms,
    })
}

/// Builds a [`NetworkEntry`] from `Network.requestWillBeSent`.
#[must_use]
pub fn network_request(params: &Value, now_ms: u64) -> Option<(String, NetworkEntry)> {
    let request_id = params.get("requestId")?.as_str()?.to_owned();
    let request = params.get("request")?;
    let method = request.get("method")?.as_str()?.to_owned();
    let url = request.get("url")?.as_str()?.to_owned();
    Some((
        request_id,
        NetworkEntry {
            method,
            url,
            status: None,
            ts: now_ms,
        },
    ))
}

/// Reads the status from `Network.responseReceived`.
#[must_use]
pub fn network_response(params: &Value) -> Option<(String, u16)> {
    let request_id = params.get("requestId")?.as_str()?.to_owned();
    let status = u16::try_from(params.get("response")?.get("status")?.as_i64()?).ok()?;
    Some((request_id, status))
}

/// Maps a controller CDP event to a diagnostics [`Event`].
#[must_use]
pub fn diagnostic_event(event: &CdpEvent, now_ms: u64) -> Option<Event> {
    match event.method.as_str() {
        "Runtime.consoleAPICalled" => console_entry(&event.params, now_ms).map(Event::Console),
        "Runtime.exceptionThrown" => exception_entry(&event.params, now_ms).map(Event::Console),
        "Network.requestWillBeSent" => network_request(&event.params, now_ms)
            .map(|(request_id, entry)| Event::NetworkStarted { request_id, entry }),
        "Network.responseReceived" => network_response(&event.params)
            .map(|(request_id, status)| Event::NetworkStatus { request_id, status }),
        "Page.frameNavigated" => frame_navigated(&event.params),
        _ => None,
    }
}

/// Main-frame navigations only: iframe swaps are not address-bar material.
fn frame_navigated(params: &Value) -> Option<Event> {
    let frame = params.get("frame")?;
    if frame.get("parentId").is_some() {
        return None;
    }
    let url = frame.get("url")?.as_str()?;
    Some(Event::FrameNavigated {
        url: url.to_owned(),
    })
}

/// Cheap view of the ownership state for request handlers.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub state: OwnershipState,
    pub page_id: Option<String>,
    pub has_controller: bool,
    pub viewport: Option<Viewport>,
    /// Set while the MCP is waiting for a human answer; the viewer needs it to
    /// count as an answerer and a reconnecting monitor needs it for the banner.
    pub handoff: Option<PendingHandoff>,
}

/// One human handoff waiting for an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingHandoff {
    pub request_id: String,
    pub prompt: String,
}

impl Snapshot {
    #[must_use]
    pub fn interactive(&self) -> bool {
        self.state == OwnershipState::HumanHolds && self.has_controller
    }
}

/// Fire-and-forget handle to one session's ownership driver.
#[derive(Clone)]
pub struct OwnershipHandle {
    events: mpsc::UnboundedSender<Event>,
    snapshot: Arc<RwLock<Snapshot>>,
}

impl OwnershipHandle {
    fn send(&self, event: Event) {
        if self.events.send(event).is_err() {
            debug!("ownership driver is gone; dropping event");
        }
    }

    /// Current state, readable without waiting for the driver.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot
            .read()
            .expect("ownership snapshot poisoned")
            .clone()
    }

    pub fn takeover(&self) {
        self.send(Event::Takeover);
    }

    pub fn release(&self) {
        self.send(Event::Release);
    }

    pub fn release_for_devtools(&self) {
        self.send(Event::ReleaseForDevtools);
    }

    pub fn answer(&self, request_id: impl Into<String>, accepted: bool) {
        self.send(Event::Answer {
            request_id: request_id.into(),
            accepted,
        });
    }

    pub fn input(&self, command: ControllerCommand) {
        self.send(Event::Input(command));
    }

    pub fn agent_state(&self, state: AgentState) {
        self.send(Event::AgentState(state));
    }

    pub fn page(&self, page: Option<String>) {
        self.send(Event::Page(page));
    }

    /// The tab list's view of the active page's URL. Sent on every page-list
    /// change; the machine ignores repeats of the URL it already reports.
    pub(crate) fn page_url(&self, url: impl Into<String>) {
        self.send(Event::PageUrl(url.into()));
    }

    /// Updates the CDP endpoint used for the next controller attach.
    pub fn cdp_url(&self, url: impl Into<String>) {
        self.send(Event::CdpUrl(url.into()));
    }

    pub fn yield_ok(&self, page_id: impl Into<String>) {
        self.send(Event::YieldOk(page_id.into()));
    }

    pub fn reclaim(&self, page_id: impl Into<String>) {
        self.send(Event::Reclaim(page_id.into()));
    }

    pub fn human_request(
        &self,
        request_id: impl Into<String>,
        page_id: impl Into<String>,
        prompt: impl Into<String>,
        timeout_ms: u64,
    ) {
        self.send(Event::HumanRequest {
            request_id: request_id.into(),
            page_id: page_id.into(),
            prompt: prompt.into(),
            timeout_ms,
        });
    }

    pub(crate) fn viewer_joined(&self) {
        self.send(Event::ViewerJoined);
    }

    pub(crate) fn viewer_left(&self) {
        self.send(Event::ViewerLeft);
    }
}

impl std::fmt::Debug for OwnershipHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnershipHandle")
            .field("snapshot", &self.snapshot())
            .finish_non_exhaustive()
    }
}

/// Opens a writable page connection. Boxed so tests can inject a fake.
pub type Connector = Arc<
    dyn Fn(String, String) -> BoxFuture<'static, Result<Box<dyn PageController>, CdpError>>
        + Send
        + Sync,
>;

fn cdp_connector() -> Connector {
    Arc::new(|cdp_url, page_id| {
        Box::pin(async move {
            let controller = cdp::Controller::connect(&cdp_url, &page_id).await?;
            Ok(Box::new(controller) as Box<dyn PageController>)
        })
    })
}

/// Per-session ownership drivers.
pub struct OwnershipStore {
    settings: Settings,
    registry: Arc<RegistryHandle>,
    connector: Connector,
    handles: RwLock<HashMap<String, OwnershipHandle>>,
}

impl OwnershipStore {
    #[must_use]
    pub fn new(settings: Settings, registry: Arc<RegistryHandle>) -> Self {
        Self::with_connector(settings, registry, cdp_connector())
    }

    /// Test seam: replaces the controller transport.
    #[must_use]
    pub fn with_connector(
        settings: Settings,
        registry: Arc<RegistryHandle>,
        connector: Connector,
    ) -> Self {
        Self {
            settings,
            registry,
            connector,
            handles: RwLock::new(HashMap::new()),
        }
    }

    /// Starts the driver for a freshly registered session.
    #[must_use]
    pub fn spawn(
        &self,
        session_id: String,
        name: String,
        cdp_url: String,
        agent: AgentState,
        page: Option<String>,
        messages: broadcast::Sender<ServerMessage>,
    ) -> OwnershipHandle {
        let (events, receiver) = mpsc::unbounded_channel();
        let snapshot = Arc::new(RwLock::new(Snapshot {
            state: OwnershipState::AgentDriving,
            page_id: page.clone(),
            has_controller: false,
            viewport: None,
            handoff: None,
        }));
        let handle = OwnershipHandle {
            events,
            snapshot: snapshot.clone(),
        };
        let machine = Machine::new(self.settings, session_id.clone(), name, agent, page);
        let connector = self.connector.clone();
        let registry = self.registry.clone();
        tokio::spawn(run(
            machine, receiver, cdp_url, messages, snapshot, connector, registry,
        ));
        self.handles
            .write()
            .expect("ownership store poisoned")
            .insert(session_id, handle.clone());
        handle
    }

    #[must_use]
    pub fn get(&self, session_id: &str) -> Option<OwnershipHandle> {
        self.handles
            .read()
            .expect("ownership store poisoned")
            .get(session_id)
            .cloned()
    }

    /// Stops the driver, which closes any controller it holds.
    pub fn remove(&self, session_id: &str) {
        self.handles
            .write()
            .expect("ownership store poisoned")
            .remove(session_id);
    }
}

impl std::fmt::Debug for OwnershipStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnershipStore")
            .field("settings", &self.settings)
            .field(
                "sessions",
                &self.handles.read().expect("ownership store poisoned").len(),
            )
            .finish_non_exhaustive()
    }
}

async fn run(
    mut machine: Machine,
    mut events: mpsc::UnboundedReceiver<Event>,
    mut cdp_url: String,
    messages: broadcast::Sender<ServerMessage>,
    snapshot: Arc<RwLock<Snapshot>>,
    connector: Connector,
    registry: Arc<RegistryHandle>,
) {
    let session_id = machine.session_id.clone();
    let mut controller: Option<Box<dyn PageController>> = None;
    let mut viewport: Option<Viewport> = None;
    refresh_snapshot(&snapshot, &machine, viewport);

    loop {
        let deadline = machine.next_deadline();
        let now_ms = unix_millis();
        tokio::select! {
            event = events.recv() => {
                let Some(event) = event else { break };
                let effects = match event {
                    Event::CdpUrl(url) => {
                        cdp_url = url;
                        Vec::new()
                    }
                    other => machine.on(other, Instant::now()),
                };
                pump(effects, &mut machine, &mut controller, &mut viewport, &cdp_url, &messages, &connector, &registry).await;
            }
            () = sleep_until_deadline(deadline) => {
                let effects = machine.on(Event::Tick, Instant::now());
                pump(effects, &mut machine, &mut controller, &mut viewport, &cdp_url, &messages, &connector, &registry).await;
            }
            event = next_controller_event(&mut controller), if controller.is_some() => {
                match event {
                    Some(event) => {
                        if let Some(event) = diagnostic_event(&event, now_ms) {
                            let effects = machine.on(event, Instant::now());
                            pump(effects, &mut machine, &mut controller, &mut viewport, &cdp_url, &messages, &connector, &registry).await;
                        }
                    }
                    None => {
                        controller = None;
                        viewport = None;
                        let effects = machine.on(Event::ControllerClosed("the CDP socket closed".to_owned()), Instant::now());
                        pump(effects, &mut machine, &mut controller, &mut viewport, &cdp_url, &messages, &connector, &registry).await;
                    }
                }
            }
        }
        refresh_snapshot(&snapshot, &machine, viewport);
    }
    debug!(session = %session_id, "ownership driver stopped");
}

/// Executes effects, feeding their follow-up events back into the machine until
/// the machine is quiescent (a controller attach, for example, produces
/// `ControllerOpened`, which in turn produces diagnostics and a broadcast).
#[allow(clippy::too_many_arguments)]
async fn pump(
    effects: Vec<Effect>,
    machine: &mut Machine,
    controller: &mut Option<Box<dyn PageController>>,
    viewport: &mut Option<Viewport>,
    cdp_url: &str,
    messages: &broadcast::Sender<ServerMessage>,
    connector: &Connector,
    registry: &RegistryHandle,
) {
    let mut pending = effects;
    loop {
        let follow_up = apply(
            pending, machine, controller, viewport, cdp_url, messages, connector, registry,
        )
        .await;
        if follow_up.is_empty() {
            return;
        }
        let mut next = Vec::new();
        for event in follow_up {
            next.extend(machine.on(event, Instant::now()));
        }
        pending = next;
    }
}

async fn sleep_until_deadline(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

async fn next_controller_event(
    controller: &mut Option<Box<dyn PageController>>,
) -> Option<CdpEvent> {
    match controller {
        Some(controller) => controller.next_event().await,
        None => std::future::pending().await,
    }
}

fn refresh_snapshot(
    snapshot: &Arc<RwLock<Snapshot>>,
    machine: &Machine,
    viewport: Option<Viewport>,
) {
    let mut guard = snapshot.write().expect("ownership snapshot poisoned");
    *guard = Snapshot {
        state: machine.state(),
        page_id: machine.page().map(str::to_owned),
        has_controller: machine.has_controller(),
        viewport,
        handoff: machine.handoff_view(),
    };
}

#[allow(clippy::too_many_arguments)]
async fn apply(
    effects: Vec<Effect>,
    machine: &mut Machine,
    controller: &mut Option<Box<dyn PageController>>,
    viewport: &mut Option<Viewport>,
    cdp_url: &str,
    messages: &broadcast::Sender<ServerMessage>,
    connector: &Connector,
    registry: &RegistryHandle,
) -> Vec<Event> {
    let mut follow_up = Vec::new();
    let session_id = machine.session_id.clone();

    for effect in effects {
        match effect {
            Effect::Yield { page_id, reason } => {
                let message = GatewayMessage::YieldRequest {
                    session_id: session_id.clone(),
                    page_id,
                    reason,
                    timeout_ms: machine.settings.yield_timeout.as_millis() as u64,
                };
                if let Err(error) = registry.send(&session_id, message).await {
                    warn!(session = %session_id, error = %error, "yield request could not be delivered");
                    // The viewer that asked for the page must learn why nothing
                    // happened; the state machine stays where it was and the
                    // next takeover retries.
                    let _ = messages.send(ServerMessage::Error {
                        code: "unavailable".to_owned(),
                        message: "no agent is connected to hand this page over".to_owned(),
                    });
                }
            }
            Effect::OpenController { page_id } => {
                match connector(cdp_url.to_owned(), page_id.clone()).await {
                    Ok(connection) => {
                        info!(session = %session_id, page = %page_id, endpoint = %connection.endpoint(), "controller attached");
                        *controller = Some(connection);
                        follow_up.push(Event::ControllerOpened {
                            page_id,
                            error: None,
                        });
                    }
                    Err(error) => {
                        warn!(session = %session_id, page = %page_id, error = %error, "controller attach failed");
                        follow_up.push(Event::ControllerOpened {
                            page_id,
                            error: Some(error.to_string()),
                        });
                    }
                }
            }
            Effect::CloseController => {
                if controller.take().is_some() {
                    info!(session = %session_id, "controller closed");
                }
                *viewport = None;
            }
            Effect::Broadcast(message) => {
                let _ = messages.send(message);
            }
            Effect::Reclaimed { page_id, ok } => {
                let message = GatewayMessage::Reclaimed {
                    session_id: session_id.clone(),
                    page_id,
                    ok,
                };
                if let Err(error) = registry.send(&session_id, message).await {
                    warn!(session = %session_id, error = %error, "reclaimed could not be delivered");
                }
            }
            Effect::HumanResult { request_id, status } => {
                let navigation = machine.navigation().clone();
                let message = GatewayMessage::HumanResult {
                    session_id: session_id.clone(),
                    request_id,
                    status,
                    url: navigation.url,
                    title: navigation.title,
                };
                if let Err(error) = registry.send(&session_id, message).await {
                    warn!(session = %session_id, error = %error, "human result could not be delivered");
                }
            }
            Effect::History { delta } => {
                if let Some(url) = machine.peek_history(delta) {
                    if let Some(connection) = controller.as_deref() {
                        match cdp::navigate(connection, &url).await {
                            Ok(_) => follow_up.push(Event::HistoryArrived { delta }),
                            Err(error) => {
                                warn!(session = %session_id, error = %error, "history step failed");
                            }
                        }
                    }
                }
            }
            Effect::Dispatch(command) => {
                if let Some(connection) = controller.as_deref() {
                    dispatch(connection, command, &session_id, &mut follow_up).await;
                }
            }
            Effect::EnableDiagnostics => {
                if let Some(connection) = controller.as_deref() {
                    if let Err(error) = cdp::enable_diagnostics(connection).await {
                        warn!(session = %session_id, error = %error, "cannot enable diagnostics");
                    }
                }
            }
            Effect::RefreshViewport => {
                if let Some(connection) = controller.as_deref() {
                    match cdp::layout_metrics(connection).await {
                        Ok(metrics) => *viewport = metrics,
                        Err(error) => {
                            debug!(session = %session_id, error = %error, "layout metrics unavailable")
                        }
                    }
                }
            }
            Effect::RefreshNavigation => {
                if let Some(connection) = controller.as_deref() {
                    match cdp::navigation_state(connection).await {
                        Ok(state) => follow_up.push(Event::Navigation(state)),
                        Err(error) => {
                            debug!(session = %session_id, error = %error, "navigation state unavailable")
                        }
                    }
                }
            }
        }
    }
    follow_up
}

async fn dispatch(
    controller: &dyn PageController,
    command: ControllerCommand,
    session_id: &str,
    follow_up: &mut Vec<Event>,
) {
    let result = match command {
        ControllerCommand::Mouse {
            event,
            x,
            y,
            button,
            click_count,
            modifiers,
        } => cdp::dispatch_mouse(controller, event, x, y, button, click_count, modifiers)
            .await
            .map(|_| None),
        ControllerCommand::Wheel {
            x,
            y,
            delta_x,
            delta_y,
        } => cdp::dispatch_wheel(controller, x, y, delta_x, delta_y)
            .await
            .map(|_| None),
        ControllerCommand::Key {
            event,
            key,
            code,
            text,
            modifiers,
        } => cdp::dispatch_key(controller, event, &key, &code, text.as_deref(), modifiers)
            .await
            .map(|_| None),
        ControllerCommand::Text(text) => cdp::insert_text(controller, &text).await.map(|_| None),
        ControllerCommand::Navigate(url) => cdp::navigate(controller, &url).await.map(Some),
        ControllerCommand::History(delta) => {
            // The machine resolves history against its own visit stack; a step
            // that reaches the controller has been routed around it.
            warn!(session = %session_id, delta, "history step was not resolved by the machine");
            return;
        }
    };
    match result {
        Ok(Some(state)) => follow_up.push(Event::Navigation(state)),
        Ok(None) => {}
        Err(error) => warn!(session = %session_id, error = %error, "controller input failed"),
    }
}

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "ownership_test.rs"]
mod tests;
