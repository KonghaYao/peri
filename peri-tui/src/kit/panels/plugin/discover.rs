//! Canonical Discover state: query, request ownership, results, and selected identity.

use super::{DiscoverDetailAction, PluginSearchResultItem, SearchState};
use crate::components::textarea::TextAreaState;
use crate::kit::atoms::{ACTIVE_SESSION_ID, BRIDGE_RESET_COUNTER};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct SearchSession {
    pub id: String,
    reset: u64,
}

impl SearchSession {
    pub fn current() -> Self {
        Self {
            id: ACTIVE_SESSION_ID.state().read().clone(),
            reset: BRIDGE_RESET_COUNTER.get(),
        }
    }
}

#[derive(Clone)]
pub(super) struct SearchTicket {
    generation: u64,
    pub session: SearchSession,
    pub query: String,
    pub cancelled: CancellationToken,
}

pub(super) struct DiscoverState {
    pub editor: TextAreaState,
    pub focused: bool,
    pub status: SearchState,
    pub selected: usize,
    pub detail: Option<PluginSearchResultItem>,
    pub detail_action: usize,
    session: SearchSession,
    generation: u64,
    pending: Option<CancellationToken>,
    // None is local browsing; Some(empty) is an authoritative empty search result.
    results: Option<Vec<PluginSearchResultItem>>,
}

impl Default for DiscoverState {
    fn default() -> Self {
        Self {
            editor: TextAreaState::default(),
            focused: false,
            status: SearchState::Idle,
            selected: 0,
            detail: None,
            detail_action: 0,
            session: SearchSession::default(),
            generation: 0,
            pending: None,
            results: None,
        }
    }
}

impl Drop for DiscoverState {
    fn drop(&mut self) {
        self.cancel_pending();
    }
}

impl DiscoverState {
    fn cancel_pending(&mut self) {
        if let Some(pending) = self.pending.take() {
            pending.cancel();
        }
    }

    fn invalidate(&mut self) {
        self.cancel_pending();
        self.generation = self
            .generation
            .checked_add(1)
            .expect("plugin search generation exhausted");
        self.status = SearchState::Idle;
        self.results = None;
        self.selected = 0;
        self.close_detail();
    }

    pub fn reset_session(&mut self, session: SearchSession) {
        if self.session != session {
            self.close();
            self.session = session;
        }
    }

    pub fn close(&mut self) {
        self.invalidate();
        self.editor = TextAreaState::default();
        self.focused = false;
    }

    pub fn edit(&mut self, action: impl FnOnce(&mut TextAreaState)) {
        self.invalidate();
        action(&mut self.editor);
        self.focused = true;
    }

    pub fn begin_search(&mut self, session: SearchSession) -> Option<SearchTicket> {
        if self.editor.text.is_empty() {
            return None;
        }
        // Session changes are normally projected before user input; reject a
        // stale editor instead of submitting it under a new session.
        if self.session != session {
            self.reset_session(session);
            return None;
        }
        self.invalidate();
        self.status = SearchState::Loading;
        self.focused = true;
        let cancelled = CancellationToken::new();
        self.pending = Some(cancelled.clone());
        Some(SearchTicket {
            generation: self.generation,
            session: self.session.clone(),
            query: self.editor.text.clone(),
            cancelled,
        })
    }

    pub fn complete(
        &mut self,
        ticket: &SearchTicket,
        result: Result<Vec<PluginSearchResultItem>, String>,
    ) -> bool {
        if ticket.cancelled.is_cancelled()
            || ticket.generation != self.generation
            || ticket.session != self.session
        {
            return false;
        }
        self.pending = None;
        match result {
            Ok(items) => {
                self.results = Some(items);
                self.status = SearchState::Idle;
            }
            Err(error) => {
                self.results = Some(vec![]);
                self.status = SearchState::Error(error);
            }
        }
        self.selected = 0;
        true
    }

    pub fn visible_items<'a>(
        &'a self,
        local: &'a [PluginSearchResultItem],
    ) -> Vec<&'a PluginSearchResultItem> {
        if let Some(results) = &self.results {
            return results.iter().collect();
        }
        let query = self.editor.text.to_lowercase();
        local
            .iter()
            .filter(|item| {
                query.is_empty()
                    || item.name.to_lowercase().contains(&query)
                    || item.description.to_lowercase().contains(&query)
                    || item.marketplace.to_lowercase().contains(&query)
            })
            .collect()
    }

    pub fn open_selected(&mut self, index: usize, local: &[PluginSearchResultItem]) {
        if !matches!(self.status, SearchState::Idle) {
            return;
        }
        let item = self
            .visible_items(local)
            .get(index)
            .map(|item| (*item).clone());
        if let Some(item) = item {
            self.selected = index;
            self.detail = Some(item);
            self.detail_action = 0;
            self.focused = false;
        }
    }

    pub fn close_detail(&mut self) {
        self.detail = None;
        self.detail_action = 0;
    }

    pub fn detail_selection(&self) -> Option<(PluginSearchResultItem, DiscoverDetailAction)> {
        Some((
            self.detail.clone()?,
            *DiscoverDetailAction::ALL.get(self.detail_action)?,
        ))
    }
}
