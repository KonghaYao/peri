//! Core metrics, Agent metrics and Request log state, polling and results.
//!
//! Each page owns one [`PollState`]; a page change, window change or manual
//! refresh invalidates the generation, so a response that belongs to the old
//! window/page can never overwrite the current one. Requests never overlap:
//! while one is in flight the next is recorded as [`reload_wanted`] and starts
//! after the response releases the slot.
//!
//! Core metrics and the request log are OpenObserve-backed; a missing
//! `[observability]` section is shown as a normal configuration state. Agent
//! metrics comes from the gateway and loads without OpenObserve.

use std::time::{Duration, Instant};

use crate::api::{AgentMetrics, ApiError};
use crate::metrics::{
    CoreMetricsSnapshot, RequestLogPage, RequestLogRow, TimeWindow, AGENT_METRICS_PAGE_SIZE,
};
use crate::poll::PollState;
use crate::ui::text;
use crate::worker::ObservabilityOutcome;

use super::types::{ActionId, Effect, Focus, Intent, Page, Screen};
use super::App;

/// Core-metrics poll interval while the page is visible.
pub const CORE_METRICS_INTERVAL: Duration = Duration::from_secs(15);
/// Agent-metrics poll interval while the page is visible.
pub const AGENT_METRICS_INTERVAL: Duration = Duration::from_secs(30);
/// Request-log poll interval while the page is visible.
pub const REQUEST_LOG_INTERVAL: Duration = Duration::from_secs(15);

/// Core-metrics page state.
#[derive(Debug)]
pub struct CoreMetricsState {
    pub(crate) window: TimeWindow,
    pub(crate) snapshot: Option<CoreMetricsSnapshot>,
    pub(crate) loading: bool,
    pub(crate) stale: bool,
    pub(crate) error: Option<String>,
    pub(crate) not_configured: bool,
    pub(crate) ever_loaded: bool,
    pub(crate) poll: PollState,
    pub(crate) last_request: Option<Instant>,
    pub(crate) reload_wanted: bool,
}

impl Default for CoreMetricsState {
    fn default() -> Self {
        Self {
            window: TimeWindow::FifteenMinutes,
            snapshot: None,
            loading: true,
            stale: false,
            error: None,
            not_configured: false,
            ever_loaded: false,
            poll: PollState::new(),
            last_request: None,
            reload_wanted: false,
        }
    }
}

impl CoreMetricsState {
    /// Empty state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Selected window.
    #[must_use]
    pub fn window(&self) -> TimeWindow {
        self.window
    }

    /// Last successful snapshot.
    #[must_use]
    pub fn snapshot(&self) -> Option<&CoreMetricsSnapshot> {
        self.snapshot.as_ref()
    }

    /// Whether the first request has not completed yet.
    #[must_use]
    pub fn loading(&self) -> bool {
        self.loading
    }

    /// Whether the shown snapshot predates the last failed refresh.
    #[must_use]
    pub fn stale(&self) -> bool {
        self.stale
    }

    /// Last error, already safe to display.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Whether OpenObserve is missing from the saved configuration.
    #[must_use]
    pub fn not_configured(&self) -> bool {
        self.not_configured
    }

    /// Whether at least one request has completed.
    #[must_use]
    pub fn ever_loaded(&self) -> bool {
        self.ever_loaded
    }

    fn mark_not_configured(&mut self) {
        self.not_configured = true;
        self.loading = false;
        self.error = None;
        self.stale = false;
        self.reload_wanted = false;
    }

    fn reset_for_window(&mut self, window: TimeWindow) {
        self.window = window;
        self.snapshot = None;
        self.loading = true;
        self.stale = false;
        self.error = None;
        self.ever_loaded = false;
        self.last_request = None;
    }
}

/// Agent-metrics page state.
#[derive(Debug)]
pub struct AgentMetricsState {
    pub(crate) window: TimeWindow,
    pub(crate) metrics: Option<AgentMetrics>,
    pub(crate) page: usize,
    pub(crate) loading: bool,
    pub(crate) stale: bool,
    pub(crate) error: Option<String>,
    pub(crate) ever_loaded: bool,
    pub(crate) poll: PollState,
    pub(crate) last_request: Option<Instant>,
    pub(crate) reload_wanted: bool,
}

impl Default for AgentMetricsState {
    fn default() -> Self {
        Self {
            window: TimeWindow::FifteenMinutes,
            metrics: None,
            page: 0,
            loading: true,
            stale: false,
            error: None,
            ever_loaded: false,
            poll: PollState::new(),
            last_request: None,
            reload_wanted: false,
        }
    }
}

impl AgentMetricsState {
    /// Empty state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Selected window.
    #[must_use]
    pub fn window(&self) -> TimeWindow {
        self.window
    }

    /// Loaded response, when one has arrived.
    #[must_use]
    pub fn metrics(&self) -> Option<&AgentMetrics> {
        self.metrics.as_ref()
    }

    /// Current display page, clamped to the loaded pages.
    #[must_use]
    pub fn page(&self) -> usize {
        self.page.min(self.page_count().saturating_sub(1))
    }

    /// Number of display pages; an empty response is one page.
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.metrics
            .as_ref()
            .map_or(0, |metrics| metrics.sessions.len())
            .div_ceil(AGENT_METRICS_PAGE_SIZE)
            .max(1)
    }

    /// Rows on the current display page.
    #[must_use]
    pub fn page_rows(&self) -> &[crate::api::AgentMetricsSession] {
        let Some(metrics) = self.metrics.as_ref() else {
            return &[];
        };
        let start = self.page().saturating_mul(AGENT_METRICS_PAGE_SIZE);
        let end = start
            .saturating_add(AGENT_METRICS_PAGE_SIZE)
            .min(metrics.sessions.len());
        &metrics.sessions[start.min(end)..end]
    }

    /// Totals for the window.
    #[must_use]
    pub fn totals(&self) -> Option<&crate::api::AgentMetricsTotals> {
        self.metrics.as_ref().map(|metrics| &metrics.totals)
    }

    /// Number of sessions in the response.
    #[must_use]
    pub fn loaded(&self) -> usize {
        self.metrics
            .as_ref()
            .map_or(0, |metrics| metrics.sessions.len())
    }

    /// Whether the window held more rows than one gateway scan reads.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.metrics
            .as_ref()
            .is_some_and(|metrics| metrics.truncated)
    }

    /// Whether the first request has not completed yet.
    #[must_use]
    pub fn loading(&self) -> bool {
        self.loading
    }

    /// Whether the shown response predates the last failed refresh.
    #[must_use]
    pub fn stale(&self) -> bool {
        self.stale
    }

    /// Last error, already safe to display.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Whether at least one request has completed.
    #[must_use]
    pub fn ever_loaded(&self) -> bool {
        self.ever_loaded
    }

    fn reset_for_window(&mut self, window: TimeWindow) {
        self.window = window;
        self.metrics = None;
        self.page = 0;
        self.loading = true;
        self.stale = false;
        self.error = None;
        self.ever_loaded = false;
        self.last_request = None;
    }
}

/// Request-log page state.
#[derive(Debug)]
pub struct RequestLogState {
    pub(crate) window: TimeWindow,
    pub(crate) page: usize,
    pub(crate) applied_page: usize,
    pub(crate) rows: Vec<RequestLogRow>,
    pub(crate) has_next: bool,
    pub(crate) loading: bool,
    pub(crate) stale: bool,
    pub(crate) error: Option<String>,
    pub(crate) not_configured: bool,
    pub(crate) ever_loaded: bool,
    pub(crate) poll: PollState,
    pub(crate) last_request: Option<Instant>,
    pub(crate) reload_wanted: bool,
}

impl Default for RequestLogState {
    fn default() -> Self {
        Self {
            window: TimeWindow::FifteenMinutes,
            page: 0,
            applied_page: 0,
            rows: Vec::new(),
            has_next: false,
            loading: true,
            stale: false,
            error: None,
            not_configured: false,
            ever_loaded: false,
            poll: PollState::new(),
            last_request: None,
            reload_wanted: false,
        }
    }
}

impl RequestLogState {
    /// Empty state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Selected window.
    #[must_use]
    pub fn window(&self) -> TimeWindow {
        self.window
    }

    /// Requested page, 0-based.
    #[must_use]
    pub fn page(&self) -> usize {
        self.page
    }

    /// Page the displayed rows belong to.
    #[must_use]
    pub fn applied_page(&self) -> usize {
        self.applied_page
    }

    /// Whether the requested page has not been applied yet.
    #[must_use]
    pub fn pending_page(&self) -> bool {
        self.page != self.applied_page
    }

    /// Rows on the displayed page.
    #[must_use]
    pub fn rows(&self) -> &[RequestLogRow] {
        &self.rows
    }

    /// Whether an older page exists.
    #[must_use]
    pub fn has_next(&self) -> bool {
        self.has_next
    }

    /// Whether a previous page exists.
    #[must_use]
    pub fn has_previous(&self) -> bool {
        self.page > 0
    }

    /// Whether the next page may be requested.
    #[must_use]
    pub fn can_next(&self) -> bool {
        self.has_next && !self.pending_page()
    }

    /// Whether the first request has not completed yet.
    #[must_use]
    pub fn loading(&self) -> bool {
        self.loading
    }

    /// Whether the shown rows predate the last failed refresh.
    #[must_use]
    pub fn stale(&self) -> bool {
        self.stale
    }

    /// Last error, already safe to display.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Whether OpenObserve is missing from the saved configuration.
    #[must_use]
    pub fn not_configured(&self) -> bool {
        self.not_configured
    }

    /// Whether at least one request has completed.
    #[must_use]
    pub fn ever_loaded(&self) -> bool {
        self.ever_loaded
    }

    fn mark_not_configured(&mut self) {
        self.not_configured = true;
        self.loading = false;
        self.error = None;
        self.stale = false;
        self.reload_wanted = false;
        self.rows.clear();
        self.ever_loaded = false;
        self.has_next = false;
        self.page = 0;
        self.applied_page = 0;
    }

    fn reset_for_window(&mut self, window: TimeWindow) {
        self.window = window;
        self.page = 0;
        self.applied_page = 0;
        self.rows.clear();
        self.has_next = false;
        self.loading = true;
        self.stale = false;
        self.error = None;
        self.ever_loaded = false;
        self.last_request = None;
    }
}

impl App {
    // -----------------------------------------------------------------------
    // Scheduling
    // -----------------------------------------------------------------------

    /// Metrics-page scheduling. Returns effects when a request is due.
    pub fn metrics_due(&mut self, now: Instant) -> Vec<Effect> {
        if self.screen != Screen::Main {
            return Vec::new();
        }
        match self.page {
            Page::CoreMetrics => self.core_metrics_due(now),
            Page::AgentMetrics => self.agent_metrics_due(now),
            Page::RequestLog => self.request_log_due(now),
            _ => Vec::new(),
        }
    }

    fn core_metrics_due(&mut self, now: Instant) -> Vec<Effect> {
        if self.config.is_none() || self.page != Page::CoreMetrics {
            return Vec::new();
        }
        if self.overlay.is_some() || self.paused {
            self.core_metrics.poll.pause();
            return Vec::new();
        }
        self.core_metrics.poll.resume();
        if self.core_metrics.poll.is_in_flight() {
            return Vec::new();
        }
        if !self.observability_configured() {
            self.core_metrics.mark_not_configured();
            return Vec::new();
        }
        if self.core_metrics.not_configured {
            // Configuration was added since the last visit; do not wait a full
            // interval for the first query.
            self.core_metrics.not_configured = false;
            self.core_metrics.last_request = None;
        }
        if self.core_metrics.reload_wanted {
            self.core_metrics.reload_wanted = false;
            return self.request_core_metrics(now).into_iter().collect();
        }
        let interval = self.core_metrics.poll.delay(CORE_METRICS_INTERVAL);
        if self
            .core_metrics
            .last_request
            .is_some_and(|last| now.saturating_duration_since(last) < interval)
        {
            return Vec::new();
        }
        self.request_core_metrics(now).into_iter().collect()
    }

    fn agent_metrics_due(&mut self, now: Instant) -> Vec<Effect> {
        if self.config.is_none() || self.page != Page::AgentMetrics {
            return Vec::new();
        }
        if self.overlay.is_some() || self.paused {
            self.agent_metrics.poll.pause();
            return Vec::new();
        }
        self.agent_metrics.poll.resume();
        if self.agent_metrics.poll.is_in_flight() {
            return Vec::new();
        }
        if self.agent_metrics.reload_wanted {
            self.agent_metrics.reload_wanted = false;
            return self.request_agent_metrics(now).into_iter().collect();
        }
        let interval = self.agent_metrics.poll.delay(AGENT_METRICS_INTERVAL);
        if self
            .agent_metrics
            .last_request
            .is_some_and(|last| now.saturating_duration_since(last) < interval)
        {
            return Vec::new();
        }
        self.request_agent_metrics(now).into_iter().collect()
    }

    fn request_log_due(&mut self, now: Instant) -> Vec<Effect> {
        if self.config.is_none() || self.page != Page::RequestLog {
            return Vec::new();
        }
        if self.overlay.is_some() || self.paused {
            self.request_log.poll.pause();
            return Vec::new();
        }
        self.request_log.poll.resume();
        if self.request_log.poll.is_in_flight() {
            return Vec::new();
        }
        if !self.observability_configured() {
            self.request_log.mark_not_configured();
            return Vec::new();
        }
        if self.request_log.not_configured {
            self.request_log.not_configured = false;
            self.request_log.last_request = None;
        }
        if self.request_log.reload_wanted {
            self.request_log.reload_wanted = false;
            return self.request_request_log(now).into_iter().collect();
        }
        let interval = self.request_log.poll.delay(REQUEST_LOG_INTERVAL);
        if self
            .request_log
            .last_request
            .is_some_and(|last| now.saturating_duration_since(last) < interval)
        {
            return Vec::new();
        }
        self.request_request_log(now).into_iter().collect()
    }

    fn request_core_metrics(&mut self, now: Instant) -> Option<Effect> {
        let Some(generation) = self.core_metrics.poll.begin() else {
            self.core_metrics.reload_wanted = true;
            return None;
        };
        self.core_metrics.last_request = Some(now);
        self.core_metrics.loading = self.core_metrics.snapshot.is_none();
        Some(Effect::LoadCoreMetrics {
            epoch: self.epoch,
            generation,
            window: self.core_metrics.window,
        })
    }

    fn request_agent_metrics(&mut self, now: Instant) -> Option<Effect> {
        let Some(generation) = self.agent_metrics.poll.begin() else {
            self.agent_metrics.reload_wanted = true;
            return None;
        };
        self.agent_metrics.last_request = Some(now);
        self.agent_metrics.loading = self.agent_metrics.metrics.is_none();
        Some(Effect::LoadAgentMetrics {
            epoch: self.epoch,
            generation,
            window: self.agent_metrics.window,
        })
    }

    fn request_request_log(&mut self, now: Instant) -> Option<Effect> {
        let Some(generation) = self.request_log.poll.begin() else {
            self.request_log.reload_wanted = true;
            return None;
        };
        self.request_log.last_request = Some(now);
        self.request_log.loading = self.request_log.rows.is_empty();
        Some(Effect::LoadRequestLog {
            epoch: self.epoch,
            generation,
            window: self.request_log.window,
            page: self.request_log.page,
        })
    }

    // -----------------------------------------------------------------------
    // Actions
    // -----------------------------------------------------------------------

    /// Refetch Core metrics now (`r` or the Refresh action).
    pub(super) fn refresh_core_metrics_now(&mut self) -> Vec<Effect> {
        if self.screen != Screen::Main || self.page != Page::CoreMetrics {
            return Vec::new();
        }
        if !self.observability_configured() {
            self.core_metrics.mark_not_configured();
            return Vec::new();
        }
        self.core_metrics.poll.reset_backoff();
        self.core_metrics.poll.invalidate();
        self.core_metrics.reload_wanted = true;
        if self.core_metrics.poll.is_in_flight() {
            return Vec::new();
        }
        self.request_core_metrics(Instant::now())
            .into_iter()
            .collect()
    }

    /// Refetch Agent metrics now.
    pub(super) fn refresh_agent_metrics_now(&mut self) -> Vec<Effect> {
        if self.screen != Screen::Main || self.page != Page::AgentMetrics {
            return Vec::new();
        }
        self.agent_metrics.poll.reset_backoff();
        self.agent_metrics.poll.invalidate();
        self.agent_metrics.reload_wanted = true;
        if self.agent_metrics.poll.is_in_flight() {
            return Vec::new();
        }
        self.request_agent_metrics(Instant::now())
            .into_iter()
            .collect()
    }

    /// Refetch the current request-log page now.
    pub(super) fn refresh_request_log_now(&mut self) -> Vec<Effect> {
        if self.screen != Screen::Main || self.page != Page::RequestLog {
            return Vec::new();
        }
        if !self.observability_configured() {
            self.request_log.mark_not_configured();
            return Vec::new();
        }
        self.request_log.poll.reset_backoff();
        self.request_log.poll.invalidate();
        self.request_log.reload_wanted = true;
        if self.request_log.poll.is_in_flight() {
            return Vec::new();
        }
        self.request_request_log(Instant::now())
            .into_iter()
            .collect()
    }

    /// Apply a window selection to the active metrics page.
    pub(super) fn set_metrics_window(&mut self, window: TimeWindow) -> Vec<Effect> {
        if self.screen != Screen::Main {
            return Vec::new();
        }
        match self.page {
            Page::CoreMetrics => {
                if self.core_metrics.window == window {
                    return Vec::new();
                }
                self.core_metrics.reset_for_window(window);
                self.core_metrics.poll.reset_backoff();
                self.core_metrics.poll.invalidate();
                self.core_metrics.reload_wanted = true;
                self.core_metrics_due(Instant::now())
            }
            Page::AgentMetrics => {
                if self.agent_metrics.window == window {
                    return Vec::new();
                }
                self.agent_metrics.reset_for_window(window);
                self.agent_metrics.poll.reset_backoff();
                self.agent_metrics.poll.invalidate();
                self.agent_metrics.reload_wanted = true;
                self.agent_metrics_due(Instant::now())
            }
            Page::RequestLog => {
                if self.request_log.window == window {
                    return Vec::new();
                }
                self.request_log.reset_for_window(window);
                self.request_log.poll.reset_backoff();
                self.request_log.poll.invalidate();
                self.request_log.reload_wanted = true;
                self.request_log_due(Instant::now())
            }
            _ => Vec::new(),
        }
    }

    /// Show the previous display page on Agent metrics or Request log.
    pub(super) fn metrics_page_prev(&mut self) -> Vec<Effect> {
        if self.screen != Screen::Main {
            return Vec::new();
        }
        match self.page {
            Page::AgentMetrics => {
                let page = self.agent_metrics.page();
                if page > 0 {
                    self.agent_metrics.page = page - 1;
                    self.focus = Focus::Content;
                }
                Vec::new()
            }
            Page::RequestLog => {
                if self.request_log.page() == 0 {
                    return Vec::new();
                }
                self.request_log.page -= 1;
                self.focus = Focus::Content;
                self.request_log.poll.reset_backoff();
                self.request_log.poll.invalidate();
                self.request_log.reload_wanted = true;
                if self.request_log.poll.is_in_flight() {
                    return Vec::new();
                }
                self.request_request_log(Instant::now())
                    .into_iter()
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// Show the next display page on Agent metrics or Request log.
    pub(super) fn metrics_page_next(&mut self) -> Vec<Effect> {
        if self.screen != Screen::Main {
            return Vec::new();
        }
        match self.page {
            Page::AgentMetrics => {
                let page = self.agent_metrics.page();
                if page + 1 < self.agent_metrics.page_count() {
                    self.agent_metrics.page = page + 1;
                    self.focus = Focus::Content;
                }
                Vec::new()
            }
            Page::RequestLog => {
                if !self.request_log.can_next() {
                    return Vec::new();
                }
                self.request_log.page += 1;
                self.focus = Focus::Content;
                self.request_log.poll.reset_backoff();
                self.request_log.poll.invalidate();
                self.request_log.reload_wanted = true;
                if self.request_log.poll.is_in_flight() {
                    return Vec::new();
                }
                self.request_request_log(Instant::now())
                    .into_iter()
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    // -----------------------------------------------------------------------
    // Worker results
    // -----------------------------------------------------------------------

    pub(super) fn handle_core_metrics_loaded(
        &mut self,
        epoch: u64,
        generation: u64,
        window: TimeWindow,
        result: ObservabilityOutcome<CoreMetricsSnapshot>,
    ) -> Vec<Effect> {
        if epoch != self.epoch {
            return Vec::new();
        }
        if generation != self.core_metrics.poll.generation() || window != self.core_metrics.window {
            self.core_metrics.poll.complete(generation);
            self.core_metrics.reload_wanted = true;
            return self.kick_core_metrics();
        }
        match result {
            ObservabilityOutcome::Ready(snapshot) => {
                self.core_metrics.poll.complete(generation);
                self.core_metrics.snapshot = Some(snapshot);
                self.core_metrics.loading = false;
                self.core_metrics.stale = false;
                self.core_metrics.error = None;
                self.core_metrics.not_configured = false;
                self.core_metrics.ever_loaded = true;
                self.core_metrics.reload_wanted = false;
                self.kick_core_metrics()
            }
            ObservabilityOutcome::NotConfigured => {
                self.core_metrics.poll.complete(generation);
                self.core_metrics.mark_not_configured();
                Vec::new()
            }
            ObservabilityOutcome::Failed(error) => {
                self.core_metrics.poll.fail(generation);
                self.core_metrics.loading = false;
                self.core_metrics.stale = self.core_metrics.snapshot.is_some();
                self.core_metrics.error =
                    Some(text::core_metrics_error(&text::observability_error(&error)));
                self.kick_core_metrics()
            }
        }
    }

    pub(super) fn handle_agent_metrics_loaded(
        &mut self,
        epoch: u64,
        generation: u64,
        window: TimeWindow,
        result: Result<AgentMetrics, ApiError>,
    ) -> Vec<Effect> {
        if epoch != self.epoch {
            return Vec::new();
        }
        if generation != self.agent_metrics.poll.generation() || window != self.agent_metrics.window
        {
            self.agent_metrics.poll.complete(generation);
            self.agent_metrics.reload_wanted = true;
            return self.kick_agent_metrics();
        }
        match result {
            Ok(metrics) => {
                self.agent_metrics.poll.complete(generation);
                self.agent_metrics.page = self.agent_metrics.page.min(
                    metrics
                        .sessions
                        .len()
                        .div_ceil(AGENT_METRICS_PAGE_SIZE)
                        .saturating_sub(1),
                );
                self.agent_metrics.metrics = Some(metrics);
                self.agent_metrics.loading = false;
                self.agent_metrics.stale = false;
                self.agent_metrics.error = None;
                self.agent_metrics.ever_loaded = true;
                self.agent_metrics.reload_wanted = false;
                self.kick_agent_metrics()
            }
            Err(error) => {
                if error.is_unauthorized() {
                    self.agent_metrics.poll.fail(generation);
                    self.agent_metrics.poll.pause();
                    self.paused = true;
                    self.agent_metrics.loading = false;
                    self.agent_metrics.stale = self.agent_metrics.metrics.is_some();
                    self.agent_metrics.error =
                        Some(text::agent_metrics_error(&text::api_error(&error)));
                    self.apply_unauthorized(&error);
                    return Vec::new();
                }
                self.agent_metrics.poll.fail(generation);
                self.agent_metrics.loading = false;
                self.agent_metrics.stale = self.agent_metrics.metrics.is_some();
                self.agent_metrics.error =
                    Some(text::agent_metrics_error(&text::api_error(&error)));
                self.kick_agent_metrics()
            }
        }
    }

    pub(super) fn handle_request_log_loaded(
        &mut self,
        epoch: u64,
        generation: u64,
        window: TimeWindow,
        page: usize,
        result: ObservabilityOutcome<RequestLogPage>,
    ) -> Vec<Effect> {
        if epoch != self.epoch {
            return Vec::new();
        }
        if generation != self.request_log.poll.generation()
            || window != self.request_log.window
            || page != self.request_log.page
        {
            self.request_log.poll.complete(generation);
            self.request_log.reload_wanted = true;
            return self.kick_request_log();
        }
        match result {
            ObservabilityOutcome::Ready(loaded) => {
                self.request_log.poll.complete(generation);
                self.request_log.applied_page = loaded.page;
                self.request_log.rows = loaded.rows;
                self.request_log.has_next = loaded.has_next;
                self.request_log.loading = false;
                self.request_log.stale = false;
                self.request_log.error = None;
                self.request_log.not_configured = false;
                self.request_log.ever_loaded = true;
                self.request_log.reload_wanted = false;
                self.kick_request_log()
            }
            ObservabilityOutcome::NotConfigured => {
                self.request_log.poll.complete(generation);
                self.request_log.mark_not_configured();
                Vec::new()
            }
            ObservabilityOutcome::Failed(error) => {
                self.request_log.poll.fail(generation);
                self.request_log.loading = false;
                self.request_log.stale = !self.request_log.rows.is_empty();
                self.request_log.error =
                    Some(text::request_log_error(&text::observability_error(&error)));
                // Keep the page indicator aligned with the rows that are shown.
                self.request_log.page = self.request_log.applied_page;
                self.kick_request_log()
            }
        }
    }

    fn kick_core_metrics(&mut self) -> Vec<Effect> {
        if !self.core_metrics.reload_wanted
            || self.paused
            || self.overlay.is_some()
            || self.screen != Screen::Main
            || self.page != Page::CoreMetrics
        {
            return Vec::new();
        }
        self.core_metrics.reload_wanted = false;
        self.request_core_metrics(Instant::now())
            .into_iter()
            .collect()
    }

    fn kick_agent_metrics(&mut self) -> Vec<Effect> {
        if !self.agent_metrics.reload_wanted
            || self.paused
            || self.overlay.is_some()
            || self.screen != Screen::Main
            || self.page != Page::AgentMetrics
        {
            return Vec::new();
        }
        self.agent_metrics.reload_wanted = false;
        self.request_agent_metrics(Instant::now())
            .into_iter()
            .collect()
    }

    fn kick_request_log(&mut self) -> Vec<Effect> {
        if !self.request_log.reload_wanted
            || self.paused
            || self.overlay.is_some()
            || self.screen != Screen::Main
            || self.page != Page::RequestLog
        {
            return Vec::new();
        }
        self.request_log.reload_wanted = false;
        self.request_request_log(Instant::now())
            .into_iter()
            .collect()
    }

    /// Dispatch `r`/Refresh for the active metrics page.
    pub(super) fn refresh_metrics_page(&mut self) -> Vec<Effect> {
        match self.page {
            Page::CoreMetrics => self.refresh_core_metrics_now(),
            Page::AgentMetrics => self.refresh_agent_metrics_now(),
            Page::RequestLog => self.refresh_request_log_now(),
            _ => Vec::new(),
        }
    }

    /// Dispatch an action id shared by the metrics pages.
    pub(super) fn metrics_action(&mut self, action: ActionId) -> Vec<Effect> {
        match action {
            ActionId::RefreshCoreMetrics => self.refresh_core_metrics_now(),
            ActionId::RefreshAgentMetrics => self.refresh_agent_metrics_now(),
            ActionId::RefreshRequestLog => self.refresh_request_log_now(),
            _ => Vec::new(),
        }
    }

    /// Drop every metrics page after a connection change.
    pub(super) fn reset_metrics_routes(&mut self) {
        self.core_metrics = CoreMetricsState::new();
        self.agent_metrics = AgentMetricsState::new();
        self.request_log = RequestLogState::new();
    }

    /// Apply the window intent on the active metrics page.
    pub(super) fn apply_metrics_intent(&mut self, intent: Intent) -> Vec<Effect> {
        match intent {
            Intent::SetMetricsWindow(window) => self.set_metrics_window(window),
            Intent::MetricsPagePrev => self.metrics_page_prev(),
            Intent::MetricsPageNext => self.metrics_page_next(),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
#[path = "metrics_test.rs"]
mod tests;
