//! Session log state: loaded page, filter, search, selection and polling.
//!
//! The state keeps the **loaded** page only. All counts and searches are
//! derived from it on demand, which keeps the UI honest about the scope and
//! avoids duplicating the gateway's ordering rules.

use std::time::Instant;

use crate::api::Session;
use crate::poll::PollState;
use crate::text_field::TextField;
use crate::view_model::{self, StatusBucket, StatusCounts};

/// Rows requested by the first page and by every `Load more`.
pub const LIST_PAGE_SIZE: u32 = 50;
/// Largest page the dashboard requests.
pub const LIST_MAX: u32 = 500;

/// Everything the session log page needs.
#[derive(Debug)]
pub struct SessionLogState {
    pub(crate) sessions: Vec<Session>,
    pub(crate) limit: u32,
    pub(crate) filter: StatusBucket,
    pub(crate) search: TextField,
    pub(crate) selected: Option<String>,
    pub(crate) row_cursor: usize,
    pub(crate) loading: bool,
    pub(crate) stale: bool,
    pub(crate) error: Option<String>,
    pub(crate) ever_loaded: bool,
    pub(crate) poll: PollState,
    pub(crate) last_request: Option<Instant>,
    /// A refresh or load-more arrived while a request was in flight.
    pub(crate) reload_wanted: bool,
}

impl Default for SessionLogState {
    fn default() -> Self {
        Self {
            sessions: Vec::new(),
            limit: LIST_PAGE_SIZE,
            filter: StatusBucket::All,
            search: TextField::new(""),
            selected: None,
            row_cursor: 0,
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

    /// Loaded sessions, in gateway order (`logical_id` desc).
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

    /// Current requested page size.
    #[must_use]
    pub fn limit(&self) -> u32 {
        self.limit
    }

    /// Whether `Load more` can raise the limit further.
    #[must_use]
    pub fn can_load_more(&self) -> bool {
        self.limit < LIST_MAX
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

    /// Keyboard row cursor, clamped to the visible rows.
    #[must_use]
    pub fn row_cursor(&self) -> usize {
        self.row_cursor
    }

    /// Session id under the keyboard cursor.
    #[must_use]
    pub fn cursor_session_id(&self) -> Option<String> {
        let visible = self.visible_indices();
        visible
            .get(self.row_cursor)
            .map(|index| self.sessions[*index].id.clone())
    }

    /// Whether the filter and search are both neutral.
    #[must_use]
    pub fn filters_clear(&self) -> bool {
        self.filter == StatusBucket::All && self.search.is_blank()
    }

    /// Apply a chip filter and move the cursor to the top.
    pub(crate) fn set_filter(&mut self, filter: StatusBucket) {
        self.filter = filter;
        self.row_cursor = 0;
    }

    /// Move the keyboard cursor by `delta`, clamped to the visible rows.
    pub(crate) fn move_cursor(&mut self, delta: i32) {
        let length = self.visible_len();
        if length == 0 {
            self.row_cursor = 0;
            return;
        }
        let last = i32::try_from(length - 1).unwrap_or(i32::MAX);
        let next = i32::try_from(self.row_cursor)
            .unwrap_or(0)
            .saturating_add(delta)
            .clamp(0, last);
        self.row_cursor = usize::try_from(next).unwrap_or(0);
    }

    /// Clamp the cursor after the visible set changed.
    pub(crate) fn clamp_cursor(&mut self) {
        let length = self.visible_len();
        self.row_cursor = self.row_cursor.min(length.saturating_sub(1));
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

    /// Replace the loaded page after a successful response.
    pub(crate) fn apply_page(&mut self, sessions: Vec<Session>) {
        let previous = self.selected.clone();
        self.sessions = sessions;
        self.selected = previous.filter(|id| self.sessions.iter().any(|s| &s.id == id));
        self.loading = false;
        self.stale = false;
        self.error = None;
        self.ever_loaded = true;
        self.clamp_cursor();
    }

    /// Keep the last good page and mark it stale after a failed request.
    pub(crate) fn apply_error(&mut self, message: String) {
        self.loading = false;
        self.stale = !self.sessions.is_empty();
        self.error = Some(message);
    }

    /// Reset the whole view (sign out, re-authentication, new connection).
    pub(crate) fn reset(&mut self) {
        *self = Self::new();
    }
}

#[cfg(test)]
#[path = "session_log_test.rs"]
mod tests;
