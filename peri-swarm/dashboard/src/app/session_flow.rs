//! Session-log polling, create submission and detail navigation.
//!
//! The list keeps exactly one request in flight at a time. A refresh or page
//! fetch that arrives while a request is running invalidates its generation and
//! records [`SessionLogState::reload_wanted`], so the response that is still on
//! the wire can be discarded without overlapping requests.

use std::time::Instant;

use super::types::{ActionId, DetailTab, Effect, FieldId, Focus, Page, Screen};
use super::App;
use crate::api::{ApiError, CreatedSession, IdempotencyKey};
use crate::poll;
use crate::ui::text;

impl App {
    // -----------------------------------------------------------------------
    // Scheduling
    // -----------------------------------------------------------------------

    /// Session-list scheduling. Returns an effect when a fetch is due.
    pub fn session_list_due(&mut self, now: Instant) -> Option<Effect> {
        if self.config.is_none() || self.screen != Screen::Main || self.page != Page::SessionLog {
            return None;
        }
        if self.overlay.is_some() {
            return None;
        }
        if self.paused {
            self.session_log.poll.pause();
            return None;
        }
        self.session_log.poll.resume();
        if self.session_log.poll.is_in_flight() {
            return None;
        }
        if self.session_log.reload_wanted {
            self.session_log.reload_wanted = false;
            return self.request_session_list(now);
        }
        let interval = self
            .session_log
            .poll
            .delay(poll::list_interval(self.session_log.loaded()));
        if let Some(last) = self.session_log.last_request {
            if now.saturating_duration_since(last) < interval {
                return None;
            }
        }
        self.request_session_list(now)
    }

    /// Refresh health and the active page (`r`).
    pub(super) fn refresh_all(&mut self) -> Vec<Effect> {
        if self.screen == Screen::SessionDetail {
            let mut effects = self.request_health();
            effects.extend(self.refresh_detail_now());
            return effects;
        }
        let mut effects = self.request_health();
        effects.extend(self.refresh_sessions_now());
        effects
    }

    /// Refetch the log with its current limit.
    pub(super) fn refresh_sessions_now(&mut self) -> Vec<Effect> {
        if self.config.is_none() || self.screen != Screen::Main || self.page != Page::SessionLog {
            return Vec::new();
        }
        self.session_log.poll.reset_backoff();
        self.session_log.poll.invalidate();
        if self.session_log.poll.is_in_flight() {
            self.session_log.reload_wanted = true;
            return Vec::new();
        }
        let now = Instant::now();
        self.request_session_list(now).into_iter().collect()
    }

    // -----------------------------------------------------------------------
    // Display pages
    // -----------------------------------------------------------------------

    /// Show the previous display page.
    pub(super) fn show_page_prev(&mut self) -> Vec<Effect> {
        let page = self.session_log.page();
        if page == 0 {
            return Vec::new();
        }
        self.session_log.goto_page(page - 1);
        self.focus = Focus::Content;
        Vec::new()
    }

    /// Show the next display page, fetching more rows when none are loaded yet.
    ///
    /// A page that is already loaded moves immediately. On the last loaded page
    /// the set is extended by one fetch step and the pager waits for the page to
    /// arrive; a completed set or a reached cap does nothing.
    pub(super) fn show_page_next(&mut self) -> Vec<Effect> {
        if self.config.is_none() || self.screen != Screen::Main || self.page != Page::SessionLog {
            return Vec::new();
        }
        let current = self.session_log.page();
        if current + 1 < self.session_log.page_count() {
            self.session_log.goto_page(current + 1);
            self.focus = Focus::Content;
            return Vec::new();
        }
        if !self.session_log.can_fetch_more() || self.session_log.pending_page().is_some() {
            return Vec::new();
        }
        let raised = self
            .session_log
            .limit
            .saturating_add(super::session_log::FETCH_STEP);
        self.session_log.limit = raised.min(super::session_log::MAX_SESSIONS);
        self.session_log.pending_page = Some(current + 1);
        self.focus = Focus::Content;
        self.session_log.poll.reset_backoff();
        self.session_log.poll.invalidate();
        if self.session_log.poll.is_in_flight() {
            // The in-flight response was requested with the old limit; wait for
            // it to finish, then refetch with the raised one.
            self.session_log.reload_wanted = true;
            return Vec::new();
        }
        let now = Instant::now();
        self.request_session_list(now).into_iter().collect()
    }

    /// Show `page` when it is already loaded.
    pub(super) fn goto_session_page(&mut self, page: usize) -> Vec<Effect> {
        if page >= self.session_log.page_count() {
            return Vec::new();
        }
        self.session_log.goto_page(page);
        self.focus = Focus::Content;
        Vec::new()
    }

    fn request_session_list(&mut self, now: Instant) -> Option<Effect> {
        let Some(generation) = self.session_log.start_request(now) else {
            self.session_log.reload_wanted = true;
            return None;
        };
        Some(Effect::ListSessions {
            epoch: self.epoch,
            generation,
            limit: self.session_log.limit,
        })
    }

    // -----------------------------------------------------------------------
    // Navigation
    // -----------------------------------------------------------------------

    /// Open one session's detail page.
    pub(super) fn open_session(&mut self, id: &str) {
        self.session_log.select(id);
        self.detail.reset_for(id);
        self.detail.loading = true;
        self.detail.reload_wanted = true;
        self.detail_tab = DetailTab::Timeline;
        self.screen = Screen::SessionDetail;
        self.focus = Focus::Content;
        self.content_scroll = 0;
        // The first timeline frame treats a zero-height previous layout as the
        // bottom, so an overflowing timeline opens on its newest items.
        self.content_max = 0;
    }

    /// Open a fresh create form.
    pub(super) fn open_create(&mut self) {
        self.create = super::CreateFormState::default();
        self.screen = Screen::SessionCreate;
        self.focus = Focus::Field(FieldId::CreateLogicalId);
        self.content_scroll = 0;
    }

    /// Move focus after a metadata row was removed.
    pub(super) fn focus_after_row_removal(&mut self) {
        match self.focus {
            Focus::Field(
                FieldId::CreateMetadataKey(index) | FieldId::CreateMetadataValue(index),
            ) => {
                let length = self.create.metadata.len();
                if length == 0 {
                    self.focus = Focus::Action(ActionId::AddMetadata);
                } else if index >= length {
                    self.focus = Focus::Field(FieldId::CreateMetadataValue(length - 1));
                }
            }
            Focus::Field(FieldId::EditMetadataKey(index) | FieldId::EditMetadataValue(index)) => {
                let length = self.edit.metadata.len();
                if length == 0 {
                    self.focus = Focus::Action(ActionId::AddMetadata);
                } else if index >= length {
                    self.focus = Focus::Field(FieldId::EditMetadataValue(length - 1));
                }
            }
            _ => {}
        }
    }

    /// Cycle the detail tab selection.
    pub(super) fn cycle_detail_tab(&mut self, delta: i32) {
        let tabs = DetailTab::ALL;
        let length = tabs.len();
        let index = tabs
            .iter()
            .position(|tab| *tab == self.detail_tab)
            .unwrap_or(0);
        let next = if delta < 0 {
            index.checked_sub(1).unwrap_or(length - 1)
        } else {
            (index + 1) % length
        };
        self.detail_tab = tabs[next];
    }

    // -----------------------------------------------------------------------
    // Create submission
    // -----------------------------------------------------------------------

    /// Validate the create form and queue one request.
    pub(super) fn submit_create(&mut self) -> Vec<Effect> {
        if self.screen != Screen::SessionCreate || self.create.submitting {
            return Vec::new();
        }
        let Some(spec) = self.create.build_spec() else {
            return Vec::new();
        };
        self.create.form_error = None;
        self.create.submitting = true;
        vec![Effect::CreateSession {
            epoch: self.epoch,
            spec,
            key: IdempotencyKey::new(),
        }]
    }

    // -----------------------------------------------------------------------
    // Worker results
    // -----------------------------------------------------------------------

    pub(super) fn handle_sessions_loaded(
        &mut self,
        epoch: u64,
        generation: u64,
        limit: u32,
        result: Result<crate::api::SessionPage, ApiError>,
    ) -> Vec<Effect> {
        if epoch != self.epoch {
            return Vec::new();
        }
        let current =
            generation == self.session_log.poll.generation() && limit == self.session_log.limit;
        if !current {
            // Release the in-flight slot without touching the current data.
            self.session_log.poll.complete(generation);
            self.session_log.reload_wanted = true;
            return self.kick_reload();
        }
        match result {
            Ok(page) => {
                self.session_log.poll.complete(generation);
                self.session_log.apply_page(page.data, page.has_more);
                self.session_log.reload_wanted = false;
                Vec::new()
            }
            Err(error) => {
                if error.is_unauthorized() {
                    self.session_log.poll.fail(generation);
                    self.session_log.poll.pause();
                    self.paused = true;
                    self.session_log
                        .apply_error(text::list_error(error.message()));
                    self.apply_unauthorized(&error);
                    return Vec::new();
                }
                self.session_log.poll.fail(generation);
                let detail = text::api_error(&error);
                self.session_log.apply_error(text::list_error(&detail));
                Vec::new()
            }
        }
    }

    pub(super) fn handle_session_created(
        &mut self,
        epoch: u64,
        result: Result<Box<CreatedSession>, ApiError>,
    ) -> Vec<Effect> {
        if epoch != self.epoch {
            return Vec::new();
        }
        self.create.submitting = false;
        match result {
            Ok(created) => {
                let id = created.session.id;
                self.create = super::CreateFormState::default();
                self.session_log.reload_wanted = true;
                self.open_session(&id);
                Vec::new()
            }
            Err(error) => {
                if error.is_conflict() {
                    let detail = text::api_error(&error);
                    self.create.logical_id_error = Some(text::identity_conflict(&detail));
                } else if error.is_unauthorized() {
                    self.apply_unauthorized(&error);
                    self.create.form_error = Some(text::create_failed(error.message()));
                } else {
                    let detail = text::api_error(&error);
                    self.create.form_error = Some(text::create_failed(&detail));
                }
                Vec::new()
            }
        }
    }

    /// Start a queued reload once the previous request released its slot.
    fn kick_reload(&mut self) -> Vec<Effect> {
        if !self.session_log.reload_wanted
            || self.paused
            || self.overlay.is_some()
            || self.screen != Screen::Main
            || self.page != Page::SessionLog
        {
            return Vec::new();
        }
        self.session_log.reload_wanted = false;
        let now = Instant::now();
        self.request_session_list(now).into_iter().collect()
    }
}
