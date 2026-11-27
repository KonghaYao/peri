//! Cursor and ticket changes belong to the page whose rows the bridge publishes.

use super::window_consumer::{WindowInFlight, WindowInFlightKind, WindowPagingState};
use crate::kit::history_paging::{WindowPageCursor, WindowPageResponse};

/// Held by the matching reservation until the bridge has admitted and published its rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ReceivedPageBoundary {
    before: Option<String>,
    after: Option<String>,
    read_ticket: String,
    resident_has_newer: bool,
}

impl ReceivedPageBoundary {
    pub(super) fn from_response(
        state: &WindowPagingState,
        current: &WindowInFlight,
        response: &WindowPageResponse,
    ) -> Self {
        let mut boundary = Self {
            before: state.before_cursor.clone(),
            after: state.after_cursor.clone(),
            read_ticket: response.read_ticket.clone(),
            resident_has_newer: false,
        };
        // A page fetched into a gap describes the interior, not either outer edge.
        if current.context.gap.is_none() {
            match &current.kind {
                WindowInFlightKind::Open { .. } => {
                    boundary.before = response.before_cursor.clone();
                    boundary.after = response.after_cursor.clone();
                }
                WindowInFlightKind::Page {
                    cursor: WindowPageCursor::Before(_),
                    ..
                } => {
                    boundary.before = response.before_cursor.clone();
                }
                WindowInFlightKind::Page {
                    cursor: WindowPageCursor::After(_),
                    ..
                } => {
                    boundary.after = response.after_cursor.clone();
                }
                WindowInFlightKind::Seek { .. } => {
                    if response.before_cursor.is_some() {
                        boundary.before = response.before_cursor.clone();
                    }
                    if response.after_cursor.is_some() {
                        boundary.after = response.after_cursor.clone();
                    }
                }
            }
        }
        boundary
    }

    pub(super) fn stage_resident_edges(&mut self, before: Option<String>, after: Option<String>) {
        self.resident_has_newer = after.is_some();
        self.before = before;
        self.after = after;
    }

    pub(super) fn commit(self, state: &mut WindowPagingState) {
        if self.resident_has_newer {
            state.newer_exhausted = false;
        }
        state.before_cursor = self.before;
        state.after_cursor = self.after;
        state.read_ticket = Some(self.read_ticket);
    }
}
