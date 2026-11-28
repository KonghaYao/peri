//! Session log state: loaded set, filter, search, selection and polling.
//!
//! The state keeps the **loaded** set only. All counts and searches are
//! derived from it on demand, which keeps the UI honest about the scope. After
//! every response the loaded set is ordered newest activity first (`logical_id`
//! order from the gateway is not a time order); sessions with equal timestamps
//! keep the order the gateway returned. Display pages are a client-side slice of
//! the filtered loaded set: the gateway is asked for more rows, never for a
//! cursor.

use std::time::Instant;

use peri_tui_components::list_nav::{clamp_selection, next_selection, previous_selection};

use crate::api::Session;
use crate::poll::PollState;
use crate::text_field::TextField;
use crate::view_model::{self, StatusBucket, StatusCounts};

/// Rows requested by the first fetch and by every fetch that extends the set.
pub const FETCH_STEP: u32 = 50;
/// Largest set the dashboard requests.
pub const MAX_SESSIONS: u32 = 500;
/// Rows shown on one display page.
pub const PAGE_SIZE: usize = 10;

/// Everything the session log page needs.
#[derive(Debug)]
pub struct SessionLogState {
    pub(crate) sessions: Vec<Session>,
    pub(crate) limit: u32,
    pub(crate) filter: StatusBucket,
    pub(crate) search: TextField,
    pub(crate) selected: Option<String>,
    /// Current display page, 0-based; clamped whenever it is read.
    pub(crate) page: usize,
    /// Keyboard row cursor inside the current display page.
    pub(crate) row_cursor: usize,
    /// Whether the gateway reported more sessions than the loaded set holds.
    pub(crate) has_more: bool,
    /// Page a fetch was raised for; cleared once that page exists.
    pub(crate) pending_page: Option<usize>,
    pub(crate) loading: bool,
    pub(crate) stale: bool,
    pub(crate) error: Option<String>,
    pub(crate) ever_loaded: bool,
    pub(crate) poll: PollState,
    pub(crate) last_request: Option<Instant>,
    /// A refresh or page fetch arrived while a request was in flight.
    pub(crate) reload_wanted: bool,
}

impl Default for SessionLogState {
    fn default() -> Self {
        Self {
            sessions: Vec::new(),
            limit: FETCH_STEP,
            filter: StatusBucket::All,
            search: TextField::new(""),
            selected: None,
            page: 0,
            row_cursor: 0,
            has_more: false,
            pending_page: None,
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

impl SessionLogState {
    /// Empty state for a fresh connection.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Loaded sessions, newest activity first.
    ///
    /// Ordered by `last_active_at` descending, then `created_at` descending;
    /// ties keep the order the gateway returned (stable sort).
    #[must_use]
    pub fn sessions(&self) -> &[Session] {
        &self.sessions
    }

    /// Active status bucket.
    #[must_use]
    pub fn filter(&self) -> StatusBucket {
        self.filter
    }

    /// Search box.
    #[must_use]
    pub fn search(&self) -> &TextField {
        &self.search
    }

    /// Selected session id, preserved across refreshes while it exists.
    #[must_use]
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// Whether a request is in flight or nothing has completed yet.
    #[must_use]
    pub fn loading(&self) -> bool {
        self.loading
    }

    /// Whether the shown data is older than the last failed refresh.
    #[must_use]
    pub fn stale(&self) -> bool {
        self.stale
    }

    /// Last list error, already safe to display.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Whether at least one list request has completed.
    #[must_use]
    pub fn ever_loaded(&self) -> bool {
        self.ever_loaded
    }

    /// Current requested list size.
    #[must_use]
    pub fn limit(&self) -> u32 {
        self.limit
    }

    /// Whether the gateway reported more sessions than the loaded set holds.
    #[must_use]
    pub fn has_more(&self) -> bool {
        self.has_more
    }

    /// Whether the pager may raise the limit for another fetch.
    #[must_use]
    pub fn can_fetch_more(&self) -> bool {
        self.has_more && self.limit < MAX_SESSIONS
    }

    /// Page a raised fetch is waiting for.
    #[must_use]
    pub fn pending_page(&self) -> Option<usize> {
        self.pending_page
    }

    /// Current display page, 0-based and clamped to the loaded pages.
    #[must_use]
    pub fn page(&self) -> usize {
        self.page.min(self.page_count().saturating_sub(1))
    }

    /// Number of display pages; an empty set is one page.
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.visible_len().div_ceil(PAGE_SIZE).max(1)
    }

    /// Number of rows on the current display page.
    #[must_use]
    pub fn page_len(&self) -> usize {
        self.page_indices().len()
    }

    /// Indexes into [`Self::sessions`] shown on the current display page.
    #[must_use]
    pub fn page_indices(&self) -> Vec<usize> {
        let visible = self.visible_indices();
        let start = self.page().saturating_mul(PAGE_SIZE).min(visible.len());
        let end = start.saturating_add(PAGE_SIZE).min(visible.len());
        visible[start..end].to_vec()
    }

    /// Number of loaded sessions.
    #[must_use]
    pub fn loaded(&self) -> usize {
        self.sessions.len()
    }

    /// Counts per bucket over the loaded set.
    #[must_use]
    pub fn counts(&self) -> StatusCounts {
        StatusCounts::of(&self.sessions)
    }

    /// Indexes into [`Self::sessions`] that pass the filter and search.
    #[must_use]
    pub fn visible_indices(&self) -> Vec<usize> {
        view_model::filtered_indices(&self.sessions, self.filter, self.search.value())
    }

    /// Number of visible rows.
    #[must_use]
    pub fn visible_len(&self) -> usize {
        self.visible_indices().len()
    }

    /// Keyboard row cursor, clamped to the current display page.
    #[must_use]
    pub fn row_cursor(&self) -> usize {
        self.row_cursor
    }

    /// Session id under the keyboard cursor.
    #[must_use]
    pub fn cursor_session_id(&self) -> Option<String> {
        self.page_indices()
            .get(self.row_cursor)
            .and_then(|index| self.sessions.get(*index))
            .map(|session| session.id.clone())
    }

    /// Whether the filter and search are both neutral.
    #[must_use]
    pub fn filters_clear(&self) -> bool {
        self.filter == StatusBucket::All && self.search.is_blank()
    }

    /// Apply a chip filter and return to the first display page.
    pub(crate) fn set_filter(&mut self, filter: StatusBucket) {
        self.filter = filter;
        self.reset_page();
    }

    /// Show `page`, clamped to the loaded pages, and move the cursor to its top.
    pub(crate) fn goto_page(&mut self, page: usize) {
        self.page = page.min(self.page_count().saturating_sub(1));
        self.row_cursor = 0;
    }

    /// Return to the first display page (filter and search changes).
    pub(crate) fn reset_page(&mut self) {
        self.page = 0;
        self.pending_page = None;
        self.row_cursor = 0;
    }

    /// Move the keyboard cursor by `delta`, clamped to the current page.
    pub(crate) fn move_cursor(&mut self, delta: i32) {
        let length = self.page_len();
        if length == 0 {
            self.row_cursor = 0;
            return;
        }
        let mut row = clamp_selection(self.row_cursor, length);
        // The page length bounds every meaningful step; avoid looping through
        // the whole i32 range if a caller passes a large delta.
        let steps = delta
            .unsigned_abs()
            .min(u32::try_from(length).unwrap_or(u32::MAX));
        for _ in 0..steps {
            row = if delta < 0 {
                previous_selection(row)
            } else {
                next_selection(row, length)
            };
        }
        self.row_cursor = row;
    }

    /// Clamp the cursor after the current page changed.
    pub(crate) fn clamp_cursor(&mut self) {
        self.row_cursor = clamp_selection(self.row_cursor, self.page_len());
    }

    /// Remember the selected session id.
    pub(crate) fn select(&mut self, id: &str) {
        self.selected = Some(id.to_owned());
    }

    /// Start one list request, returning its generation.
    ///
    /// Returns `None` while a same-kind request is in flight or while polling is
    /// paused; callers set [`Self::reload_wanted`] so the request is retried
    /// once the current one finishes.
    pub(crate) fn start_request(&mut self, now: Instant) -> Option<u64> {
        let generation = self.poll.begin()?;
        self.last_request = Some(now);
        self.loading = true;
        Some(generation)
    }

    /// Replace the loaded set after a successful response.
    ///
    /// The set is sorted newest activity first before it becomes visible:
    /// `last_active_at` descending, then `created_at` descending. The sort is
    /// stable, so equal timestamps keep the gateway's order. A raised fetch
    /// stores the page it was waiting for and moves there once the response
    /// actually contains it; otherwise the wait continues while another fetch
    /// may still extend the set.
    pub(crate) fn apply_page(&mut self, mut sessions: Vec<Session>, has_more: bool) {
        sessions.sort_by(|a, b| {
            b.last_active_at
                .cmp(&a.last_active_at)
                .then_with(|| b.created_at.cmp(&a.created_at))
        });
        let previous = self.selected.clone();
        self.sessions = sessions;
        self.has_more = has_more;
        self.selected = previous.filter(|id| self.sessions.iter().any(|s| &s.id == id));
        self.loading = false;
        self.stale = false;
        self.error = None;
        self.ever_loaded = true;
        if let Some(pending) = self.pending_page {
            if pending < self.page_count() {
                self.pending_page = None;
                self.page = pending;
                self.row_cursor = 0;
            } else if !has_more {
                // Nothing left to fetch, so the wait can never be satisfied.
                self.pending_page = None;
            }
        }
        self.page = self.page();
        self.clamp_cursor();
    }

    /// Keep the last good set and mark it stale after a failed request.
    pub(crate) fn apply_error(&mut self, message: String) {
        self.loading = false;
        self.stale = !self.sessions.is_empty();
        self.error = Some(message);
        self.pending_page = None;
    }

    /// Reset the whole view (sign out, re-authentication, new connection).
    pub(crate) fn reset(&mut self) {
        *self = Self::new();
    }
}

#[cfg(test)]
#[path = "session_log_test.rs"]
mod tests;
