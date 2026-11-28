//! Read-only SQL query page state and its single manual request.
//!
//! The page never polls: only Run, Prev and Next start a request. Each request
//! carries a generation; a new run, a page change or a window change
//! invalidates earlier generations, so an old response can never overwrite the
//! results of a newer action. A missing `[observability]` section is a normal
//! configuration state and sends no request at all.

use std::time::{SystemTime, UNIX_EPOCH};

use peri_tui_components::textarea::TextAreaState;
use serde_json::Value;

use crate::metrics::TimeWindow;
use crate::observability::SearchPage;
use crate::query;
use crate::ui::text;
use crate::worker::ObservabilityOutcome;

use super::types::{Effect, Page, Screen};
use super::App;

/// Query page state.
#[derive(Debug)]
pub struct QueryState {
    pub(crate) sql: TextAreaState,
    pub(crate) window: TimeWindow,
    pub(crate) page: usize,
    pub(crate) applied_page: usize,
    pub(crate) columns: Vec<String>,
    pub(crate) rows: Vec<Value>,
    pub(crate) loading: bool,
    pub(crate) in_flight: bool,
    pub(crate) generation: u64,
    pub(crate) executed_sql: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) validation_error: Option<String>,
    pub(crate) not_configured: bool,
    pub(crate) ever_loaded: bool,
}

impl Default for QueryState {
    fn default() -> Self {
        Self {
            sql: TextAreaState::default(),
            window: TimeWindow::OneHour,
            page: 0,
            applied_page: 0,
            columns: Vec::new(),
            rows: Vec::new(),
            loading: false,
            in_flight: false,
            generation: 0,
            executed_sql: None,
            error: None,
            validation_error: None,
            not_configured: false,
            ever_loaded: false,
        }
    }
}

impl QueryState {
    /// Empty state with the default one-hour window.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// SQL editor.
    #[must_use]
    pub fn sql(&self) -> &TextAreaState {
        &self.sql
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

    /// Result columns, server order first.
    #[must_use]
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// Result rows of the displayed page.
    #[must_use]
    pub fn rows(&self) -> &[Value] {
        &self.rows
    }

    /// Whether a request is running.
    #[must_use]
    pub fn loading(&self) -> bool {
        self.loading
    }

    /// Whether the worker owns an unanswered request.
    #[must_use]
    pub fn in_flight(&self) -> bool {
        self.in_flight
    }

    /// Last query failure, already safe to display.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Local validation failure, cleared as soon as the editor changes.
    #[must_use]
    pub fn validation_error(&self) -> Option<&str> {
        self.validation_error.as_deref()
    }

    /// Whether OpenObserve is missing from the saved configuration.
    #[must_use]
    pub fn not_configured(&self) -> bool {
        self.not_configured
    }

    /// Whether at least one run succeeded.
    #[must_use]
    pub fn ever_loaded(&self) -> bool {
        self.ever_loaded
    }

    /// Whether an earlier result page exists.
    #[must_use]
    pub fn has_previous(&self) -> bool {
        self.applied_page > 0
    }

    /// Whether a further result page may exist.
    ///
    /// OpenObserve's `total` is page-scoped, so Next relies on the current page
    /// being full and the row cap not being reached yet.
    #[must_use]
    pub fn can_next(&self) -> bool {
        !self.in_flight && query::has_next_page(self.applied_page, self.rows.len())
    }

    /// Clear every loaded page but keep the editor, window and generation.
    fn reset_results(&mut self) {
        self.page = 0;
        self.applied_page = 0;
        self.columns.clear();
        self.rows.clear();
        self.executed_sql = None;
        self.ever_loaded = false;
    }

    fn mark_not_configured(&mut self) {
        self.reset_results();
        self.not_configured = true;
        self.loading = false;
        self.in_flight = false;
        self.error = None;
        self.validation_error = None;
        self.generation = self.generation.wrapping_add(1);
    }
}

impl App {
    /// Keep the configured/not-configured state in sync while the page is open.
    ///
    /// This never sends a query; it only mirrors the saved configuration and
    /// discards stale results when the connection disappears.
    pub(crate) fn query_config_sync(&mut self) -> Vec<Effect> {
        if self.screen != Screen::Main || self.page != Page::Query {
            return Vec::new();
        }
        if self.observability_configured() {
            self.query.not_configured = false;
        } else if !self.query.not_configured {
            self.query.mark_not_configured();
        }
        Vec::new()
    }

    /// Run the statement in the editor (`Run` or Ctrl-Enter).
    ///
    /// A Run always starts at the first result page: the editor may hold a new
    /// statement, so an old page offset would be meaningless.
    pub(super) fn run_query(&mut self) -> Vec<Effect> {
        if self.screen != Screen::Main || self.page != Page::Query || self.query.in_flight {
            return Vec::new();
        }
        let sql = self.query.sql.text.clone();
        if let Err(error) = query::validate_sql(&sql) {
            self.query.validation_error = Some(text::sql_error(error).to_owned());
            self.query.error = None;
            return Vec::new();
        }
        self.query.validation_error = None;
        if !self.observability_configured() {
            self.query.mark_not_configured();
            return Vec::new();
        }
        self.start_query(sql, 0)
    }

    /// Show the previous result page.
    ///
    /// Paging reuses the statement that produced the displayed page, so edits
    /// in the editor only take effect on the next Run.
    pub(super) fn query_page_prev(&mut self) -> Vec<Effect> {
        if self.screen != Screen::Main || self.page != Page::Query || self.query.in_flight {
            return Vec::new();
        }
        if self.query.applied_page == 0 {
            return Vec::new();
        }
        let Some(sql) = self.query.executed_sql.clone() else {
            return Vec::new();
        };
        if query::validate_sql(&sql).is_err() {
            return Vec::new();
        }
        if !self.observability_configured() {
            self.query.mark_not_configured();
            return Vec::new();
        }
        self.start_query(sql, self.query.applied_page - 1)
    }

    /// Show the next result page.
    pub(super) fn query_page_next(&mut self) -> Vec<Effect> {
        if self.screen != Screen::Main || self.page != Page::Query || self.query.in_flight {
            return Vec::new();
        }
        if !self.query.can_next() {
            return Vec::new();
        }
        let Some(sql) = self.query.executed_sql.clone() else {
            return Vec::new();
        };
        if query::validate_sql(&sql).is_err() {
            return Vec::new();
        }
        if !self.observability_configured() {
            self.query.mark_not_configured();
            return Vec::new();
        }
        self.start_query(sql, self.query.applied_page + 1)
    }

    /// Select a window for the Query page.
    ///
    /// The loaded page belongs to the old window, so it is discarded and the
    /// in-flight generation is invalidated. A query runs only on the next Run.
    pub(super) fn set_query_window(&mut self, window: TimeWindow) -> Vec<Effect> {
        if self.screen != Screen::Main || self.page != Page::Query {
            return Vec::new();
        }
        if self.query.window == window {
            return Vec::new();
        }
        self.query.window = window;
        self.query.generation = self.query.generation.wrapping_add(1);
        self.query.reset_results();
        self.query.loading = false;
        self.query.in_flight = false;
        self.query.error = None;
        self.query.validation_error = None;
        Vec::new()
    }

    /// Drop a local validation message once the editor changes.
    pub(super) fn clear_query_validation(&mut self) {
        self.query.validation_error = None;
    }

    fn start_query(&mut self, sql: String, page: usize) -> Vec<Effect> {
        let end_us = unix_micros_now();
        let start_us = end_us.saturating_sub(self.query.window.micros());
        self.query.generation = self.query.generation.wrapping_add(1);
        self.query.page = page;
        self.query.loading = true;
        self.query.in_flight = true;
        self.query.error = None;
        self.query.validation_error = None;
        self.query.not_configured = false;
        self.query.executed_sql = Some(sql.clone());
        vec![Effect::RunQuery {
            epoch: self.epoch,
            generation: self.query.generation,
            sql,
            start_us,
            end_us,
            from: query::page_offset(page),
            size: query::QUERY_PAGE_SIZE as u64,
        }]
    }

    /// Apply one finished query.
    ///
    /// Responses from a replaced generation are discarded without touching the
    /// current state, so a window change or a newer run always wins.
    pub(super) fn handle_query_loaded(
        &mut self,
        epoch: u64,
        generation: u64,
        result: ObservabilityOutcome<SearchPage>,
    ) -> Vec<Effect> {
        if epoch != self.epoch || generation != self.query.generation {
            return Vec::new();
        }
        self.query.in_flight = false;
        self.query.loading = false;
        match result {
            ObservabilityOutcome::Ready(page) => {
                // The request asks for one page; a server that returns more
                // rows cannot widen the table past the page size.
                let mut hits = page.hits;
                hits.truncate(query::QUERY_PAGE_SIZE);
                self.query.columns = query::resolve_columns(&page.columns, &hits);
                self.query.rows = hits;
                self.query.applied_page = self.query.page;
                self.query.ever_loaded = true;
                self.query.error = None;
                self.query.not_configured = false;
            }
            ObservabilityOutcome::NotConfigured => self.query.mark_not_configured(),
            ObservabilityOutcome::Failed(error) => {
                self.query.reset_results();
                self.query.error = Some(text::query_error(&text::observability_error(&error)));
            }
        }
        Vec::new()
    }
}

/// Current Unix time in microseconds.
fn unix_micros_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "query_test.rs"]
mod tests;
