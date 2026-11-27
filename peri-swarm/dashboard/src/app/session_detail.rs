//! Session-detail state: snapshot, timeline, turns, composer and approvals.
//!
//! The detail page keeps the **last good** snapshot and marks it stale when a
//! refresh fails. Timeline pages are kept in chronological display order; the
//! gateway returns them newest first, so every applied page is reversed once.
//! The page only ever holds what the gateway returned: no optimistic items.

use std::collections::BTreeSet;
use std::time::Instant;

use peri_tui_components::textarea::TextAreaState;
use serde_json::Value;

use crate::api::{ApprovalDecision, IdempotencyKey, ItemsPage, Session, Turn, Turns};
use crate::poll::PollState;
use crate::ui::text;

/// Rows requested by the first timeline page and by every `Load earlier`.
pub const TIMELINE_PAGE_SIZE: u32 = 50;
/// Rows requested by the first turns page and by every `Load more`.
pub const TURNS_PAGE_SIZE: u32 = 50;
/// Hard cap on timeline items kept in memory.
pub const TIMELINE_MAX: usize = 500;

// ---------------------------------------------------------------------------
// Required actions
// ---------------------------------------------------------------------------

/// One pending `mcp_approval_request` from `required_actions`.
#[derive(Clone, Debug, PartialEq)]
pub struct ApprovalEntry {
    /// Opaque public approval id (`appr_...`).
    pub request_id: String,
    /// Turn that owns the tool call.
    pub turn_id: String,
    /// Declared MCP server label; empty means it could not be attributed.
    pub server_label: String,
    /// Wire tool name, when reported.
    pub tool: Option<String>,
    /// Tool arguments exactly as reported.
    pub arguments: Value,
    /// Presentation message from the gateway.
    pub message: Option<String>,
}

/// One parsed `required_actions` entry.
#[derive(Clone, Debug, PartialEq)]
pub enum RequiredAction {
    /// A pending MCP approval this dashboard can answer.
    Approval(ApprovalEntry),
    /// Any other action type: shown read-only with an explanation.
    Unsupported {
        /// Action type reported by the gateway.
        kind: String,
    },
}

/// Parse the `required_actions` array of a session snapshot.
#[must_use]
pub fn parse_required_actions(actions: &[Value]) -> Vec<RequiredAction> {
    actions
        .iter()
        .map(|action| {
            let kind = action
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if kind != "mcp_approval_request" {
                return RequiredAction::Unsupported {
                    kind: if kind.is_empty() {
                        text::ACTION_UNKNOWN.to_owned()
                    } else {
                        kind.to_owned()
                    },
                };
            }
            let request = action.get("request");
            RequiredAction::Approval(ApprovalEntry {
                request_id: string_field(action, "request_id").unwrap_or_default(),
                turn_id: string_field(action, "turn_id").unwrap_or_default(),
                server_label: request
                    .and_then(|value| string_field(value, "server_label"))
                    .unwrap_or_default(),
                tool: request.and_then(|value| string_field(value, "name")),
                arguments: request
                    .and_then(|value| value.get("arguments"))
                    .cloned()
                    .unwrap_or(Value::Null),
                message: request.and_then(|value| string_field(value, "message")),
            })
        })
        .collect()
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

// ---------------------------------------------------------------------------
// Timeline items
// ---------------------------------------------------------------------------

/// Which official item an entry came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimelineKind {
    /// A user message.
    UserMessage,
    /// An assistant message.
    AssistantMessage,
    /// Reasoning summary.
    Reasoning,
    /// A tool call.
    ToolCall,
    /// A tool call result.
    ToolOutput,
    /// Any projection this build does not render specially.
    Other,
}

/// One official item shown in the timeline.
#[derive(Clone, Debug)]
pub struct TimelineEntry {
    /// Stable id used for expansion state across refreshes.
    pub id: String,
    /// Owning turn.
    pub turn_id: String,
    /// Official item kind.
    pub kind: TimelineKind,
    /// Message / reasoning / output text.
    pub text: String,
    /// Tool name, for calls.
    pub tool: Option<String>,
    /// Pretty-printed tool arguments, for calls.
    pub arguments: Option<String>,
    /// Tool call id, for calls and outputs.
    pub call_id: Option<String>,
    /// Item status reported by the gateway.
    pub status: Option<String>,
    /// Failure text, for outputs.
    pub error: Option<String>,
    /// Item type label, for kinds this build does not specialize.
    pub label: String,
}

impl TimelineEntry {
    /// Whether this entry is an expandable tool block.
    #[must_use]
    pub fn is_tool_block(&self) -> bool {
        matches!(self.kind, TimelineKind::ToolCall | TimelineKind::ToolOutput)
    }

    /// Whether this entry has collapsible detail.
    #[must_use]
    pub fn has_details(&self) -> bool {
        match self.kind {
            TimelineKind::ToolCall => self
                .arguments
                .as_deref()
                .is_some_and(|arguments| !arguments.is_empty()),
            TimelineKind::ToolOutput => self.error.is_some() || !self.text.is_empty(),
            _ => false,
        }
    }
}

/// Project the official items `data` array into display entries.
///
/// The order is preserved (the caller reverses a `desc` page once).
#[must_use]
pub fn timeline_entries(data: &[Value]) -> Vec<TimelineEntry> {
    data.iter()
        .enumerate()
        .map(|(index, item)| timeline_entry(item, index))
        .collect()
}

fn timeline_entry(item: &Value, index: usize) -> TimelineEntry {
    let kind_text = item.get("type").and_then(Value::as_str).unwrap_or_default();
    let turn_id = string_field(item, "turn_id").unwrap_or_default();
    let call_id = string_field(item, "call_id");
    let id = string_field(item, "id")
        .or_else(|| call_id.as_ref().map(|call| format!("call:{call}")))
        .unwrap_or_else(|| format!("{turn_id}:{index}"));
    let status = string_field(item, "status");
    let mut entry = TimelineEntry {
        id,
        turn_id,
        kind: TimelineKind::Other,
        text: String::new(),
        tool: None,
        arguments: None,
        call_id,
        status,
        error: None,
        label: kind_text.to_owned(),
    };
    match kind_text {
        "message" => {
            let role = string_field(item, "role").unwrap_or_default();
            entry.kind = if role == "user" {
                TimelineKind::UserMessage
            } else {
                TimelineKind::AssistantMessage
            };
            entry.text = content_text(item.get("content"));
        }
        "reasoning" => {
            entry.kind = TimelineKind::Reasoning;
            entry.text = content_text(item.get("summary"));
        }
        "function_call" => {
            entry.kind = TimelineKind::ToolCall;
            entry.tool = string_field(item, "name");
            entry.arguments = item.get("arguments").map(pretty_json);
        }
        "function_call_output" => {
            entry.kind = TimelineKind::ToolOutput;
            entry.text = string_field(item, "output").unwrap_or_default();
            entry.error = string_field(item, "error");
        }
        _ => {}
    }
    entry
}

/// Join the text of an official content array (`input_text` / `output_text` /
/// `summary_text` blocks).
fn content_text(content: Option<&Value>) -> String {
    let Some(Value::Array(blocks)) = content else {
        return String::new();
    };
    let parts: Vec<&str> = blocks
        .iter()
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect();
    parts.join("\n\n")
}

/// Pretty-print JSON without ever failing the render.
#[must_use]
pub fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| text::DASH.to_owned())
}

// ---------------------------------------------------------------------------
// Timeline state
// ---------------------------------------------------------------------------

/// Loaded timeline plus its view window and expansion state.
#[derive(Debug)]
pub struct TimelineState {
    pub(crate) entries: Vec<TimelineEntry>,
    pub(crate) oldest_cursor: Option<String>,
    pub(crate) has_more: bool,
    pub(crate) older_loaded: bool,
    pub(crate) expanded: BTreeSet<String>,
    pub(crate) loading_more: bool,
    /// Generation that owns the in-flight `Load earlier` request.
    pub(crate) loading_generation: Option<u64>,
    pub(crate) error: Option<String>,
    /// The page viewport is at the bottom (following the newest items).
    pub(crate) at_bottom: bool,
    /// New items arrived while the view was scrolled up.
    pub(crate) unseen: bool,
    pub(crate) ever_loaded: bool,
}

impl Default for TimelineState {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            oldest_cursor: None,
            has_more: false,
            older_loaded: false,
            expanded: BTreeSet::new(),
            loading_more: false,
            loading_generation: None,
            error: None,
            at_bottom: true,
            unseen: false,
            ever_loaded: false,
        }
    }
}

impl TimelineState {
    /// Loaded entries in chronological display order.
    #[must_use]
    pub fn entries(&self) -> &[TimelineEntry] {
        &self.entries
    }

    /// Number of loaded entries.
    #[must_use]
    pub fn loaded(&self) -> usize {
        self.entries.len()
    }

    /// Whether an expandable block is open.
    #[must_use]
    pub fn is_expanded(&self, id: &str) -> bool {
        self.expanded.contains(id)
    }

    /// Whether more pages exist.
    #[must_use]
    pub fn has_more(&self) -> bool {
        self.has_more
    }

    /// Whether an older page is in flight.
    #[must_use]
    pub fn loading_more(&self) -> bool {
        self.loading_more
    }

    /// Last page failure.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Whether the timeline follows the newest items.
    #[must_use]
    pub fn following(&self) -> bool {
        self.at_bottom
    }

    /// Record whether the page viewport sits at the bottom.
    pub(crate) fn set_following(&mut self, at_bottom: bool) {
        self.at_bottom = at_bottom;
        if at_bottom {
            self.unseen = false;
        }
    }

    /// Whether new items arrived while the view was scrolled up.
    #[must_use]
    pub fn unseen(&self) -> bool {
        self.unseen
    }

    /// Whether at least one page has completed.
    #[must_use]
    pub fn ever_loaded(&self) -> bool {
        self.ever_loaded
    }

    /// Whether `Load earlier` can start a request.
    #[must_use]
    pub fn can_load_earlier(&self) -> bool {
        self.has_more
            && !self.loading_more
            && self
                .entries
                .len()
                .saturating_add(TIMELINE_PAGE_SIZE as usize)
                <= TIMELINE_MAX
    }

    /// Whether the page cursor is held but the 500-item cap blocks another page.
    #[must_use]
    pub fn cap_reached(&self) -> bool {
        self.has_more
            && !self.loading_more
            && self
                .entries
                .len()
                .saturating_add(TIMELINE_PAGE_SIZE as usize)
                > TIMELINE_MAX
    }

    /// Return to the newest items.
    pub(crate) fn jump_to_latest(&mut self) {
        self.at_bottom = true;
        self.unseen = false;
    }

    /// Toggle one tool block.
    pub(crate) fn toggle_block(&mut self, id: &str) {
        if self.expanded.contains(id) {
            self.expanded.remove(id);
        } else {
            self.expanded.insert(id.to_owned());
        }
    }

    /// Start one `Load earlier` request, returning the cursor to use.
    pub(crate) fn begin_older(&mut self, generation: u64) -> Option<String> {
        if !self.can_load_earlier() {
            return None;
        }
        self.loading_more = true;
        self.loading_generation = Some(generation);
        self.error = None;
        self.oldest_cursor.clone()
    }

    /// Clear the in-flight page marker without applying a response.
    pub(crate) fn cancel_page(&mut self) {
        self.loading_more = false;
        self.loading_generation = None;
    }

    /// Drop a page marker that belongs to an invalidated generation.
    ///
    /// A manual refresh (or pause/resume) bumps the detail generation; the
    /// response for an older `Load earlier` is discarded on arrival, so the
    /// marker must be released here instead of waiting for a reply that will
    /// never be applied.
    pub(crate) fn reconcile_page(&mut self, generation: u64) {
        if self.loading_more && self.loading_generation != Some(generation) {
            self.cancel_page();
        }
    }

    /// Apply the newest page. Entries are merged by id so a refresh cannot
    /// duplicate or reorder already loaded history.
    pub(crate) fn apply_first_page(&mut self, page: &ItemsPage) {
        let mut page_entries = timeline_entries(&page.data);
        page_entries.reverse();
        let newest_before = self.entries.last().map(|entry| entry.id.clone());
        if self.entries.is_empty() {
            self.entries = page_entries;
        } else {
            let mut appended = false;
            for entry in page_entries {
                match self.entries.iter_mut().find(|known| known.id == entry.id) {
                    Some(known) => *known = entry,
                    None => {
                        self.entries.push(entry);
                        appended = true;
                    }
                }
            }
            let newest_after = self.entries.last().map(|entry| entry.id.clone());
            if appended && newest_before != newest_after && !self.following() {
                self.unseen = true;
            }
        }
        // The polling merge can append newer items indefinitely; keep the
        // newest 500 so the same cap as `Load earlier` applies on both paths.
        if self.entries.len() > TIMELINE_MAX {
            let excess = self.entries.len() - TIMELINE_MAX;
            self.entries.drain(0..excess);
        }
        if !self.older_loaded {
            self.oldest_cursor = page.next_cursor_text();
            self.has_more = page.has_more;
        }
        self.loading_more = false;
        self.loading_generation = None;
        self.error = None;
        self.ever_loaded = true;
    }

    /// Prepend one older page.
    pub(crate) fn apply_older_page(&mut self, page: &ItemsPage) {
        let mut older = timeline_entries(&page.data);
        older.reverse();
        let mut merged: Vec<TimelineEntry> = Vec::with_capacity(older.len() + self.entries.len());
        for entry in older {
            if !self.entries.iter().any(|known| known.id == entry.id)
                && !merged.iter().any(|known| known.id == entry.id)
            {
                merged.push(entry);
            }
        }
        self.entries.splice(0..0, merged);
        if self.entries.len() > TIMELINE_MAX {
            let excess = self.entries.len() - TIMELINE_MAX;
            self.entries.drain(0..excess);
        }
        self.oldest_cursor = page.next_cursor_text();
        self.has_more = page.has_more;
        self.older_loaded = true;
        self.loading_more = false;
        self.loading_generation = None;
        self.error = None;
    }

    /// Record a failed `Load earlier`.
    pub(crate) fn fail_older(&mut self, message: String) {
        self.loading_more = false;
        self.loading_generation = None;
        self.error = Some(message);
    }
}

// ---------------------------------------------------------------------------
// Turns state
// ---------------------------------------------------------------------------

/// Loaded turns plus cursor paging.
#[derive(Debug, Default)]
pub struct TurnsState {
    pub(crate) rows: Vec<Turn>,
    pub(crate) next_cursor: Option<String>,
    pub(crate) has_more: bool,
    pub(crate) appended: bool,
    pub(crate) loading_more: bool,
    /// Generation that owns the in-flight `Load more` request.
    pub(crate) loading_generation: Option<u64>,
    pub(crate) error: Option<String>,
    pub(crate) ever_loaded: bool,
}

impl TurnsState {
    /// Loaded turns in gateway order.
    #[must_use]
    pub fn rows(&self) -> &[Turn] {
        &self.rows
    }

    /// Whether more turns exist.
    #[must_use]
    pub fn has_more(&self) -> bool {
        self.has_more
    }

    /// Whether a page is in flight.
    #[must_use]
    pub fn loading_more(&self) -> bool {
        self.loading_more
    }

    /// Last page failure.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Whether at least one page completed.
    #[must_use]
    pub fn ever_loaded(&self) -> bool {
        self.ever_loaded
    }

    /// Whether `Load more` can start a request.
    #[must_use]
    pub fn can_load_more(&self) -> bool {
        self.has_more && !self.loading_more
    }

    /// Start one `Load more` request, returning the cursor to use.
    pub(crate) fn begin_more(&mut self, generation: u64) -> Option<String> {
        if !self.can_load_more() {
            return None;
        }
        let cursor = self.next_cursor.clone()?;
        self.loading_more = true;
        self.loading_generation = Some(generation);
        self.error = None;
        Some(cursor)
    }

    /// Clear the in-flight page marker without applying a response.
    pub(crate) fn cancel_page(&mut self) {
        self.loading_more = false;
        self.loading_generation = None;
    }

    /// Drop a page marker that belongs to an invalidated generation.
    pub(crate) fn reconcile_page(&mut self, generation: u64) {
        if self.loading_more && self.loading_generation != Some(generation) {
            self.cancel_page();
        }
    }

    /// Apply the newest page, preserving rows from already loaded pages.
    pub(crate) fn apply_first_page(&mut self, page: &Turns) {
        let known: BTreeSet<&str> = page.data.iter().map(|turn| turn.id.as_str()).collect();
        let mut merged = page.data.clone();
        for row in &self.rows {
            if !known.contains(row.id.as_str()) {
                merged.push(row.clone());
            }
        }
        self.rows = merged;
        if !self.appended {
            self.next_cursor = page.next_cursor.clone();
            self.has_more = page.has_more;
        }
        self.loading_more = false;
        self.loading_generation = None;
        self.error = None;
        self.ever_loaded = true;
    }

    /// Append one page.
    pub(crate) fn apply_more(&mut self, page: &Turns) {
        for row in &page.data {
            if !self.rows.iter().any(|known| known.id == row.id) {
                self.rows.push(row.clone());
            }
        }
        self.next_cursor = page.next_cursor.clone();
        self.has_more = page.has_more;
        self.appended = true;
        self.loading_more = false;
        self.loading_generation = None;
        self.error = None;
    }

    /// Record a failed page.
    pub(crate) fn fail(&mut self, message: String) {
        self.loading_more = false;
        self.loading_generation = None;
        self.error = Some(message);
    }
}

// ---------------------------------------------------------------------------
// Mutation bookkeeping
// ---------------------------------------------------------------------------

/// One composer submission; the key is reused for a retry of the same text.
#[derive(Clone, Debug)]
pub(crate) struct SendState {
    pub(crate) text: String,
    pub(crate) key: IdempotencyKey,
    pub(crate) in_flight: bool,
}

/// One approval answer; the key is reused for a retry of the same decision.
#[derive(Clone, Debug)]
pub(crate) struct ApprovalState {
    pub(crate) request_id: String,
    pub(crate) decision: ApprovalDecision,
    pub(crate) key: IdempotencyKey,
}

/// A mutation failure shown on the page with an optional cancel affordance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionError {
    /// Safe message, already localized.
    pub message: String,
    /// Whether the error means a running turn must be cancelled first.
    pub offer_cancel: bool,
}

/// Everything the detail page needs.
#[derive(Debug)]
pub struct SessionDetailState {
    pub(crate) id: Option<String>,
    pub(crate) session: Option<Session>,
    pub(crate) loading: bool,
    pub(crate) stale: bool,
    pub(crate) error: Option<String>,
    pub(crate) action_error: Option<ActionError>,
    pub(crate) poll: PollState,
    pub(crate) group_generation: u64,
    pub(crate) outstanding: u32,
    pub(crate) group_failed: bool,
    pub(crate) last_request: Option<Instant>,
    pub(crate) reload_wanted: bool,
    pub(crate) timeline: TimelineState,
    pub(crate) turns: TurnsState,
    pub(crate) composer: TextAreaState,
    pub(crate) composer_error: Option<String>,
    pub(crate) send: Option<SendState>,
    pub(crate) approval: Option<ApprovalState>,
    pub(crate) approval_error: Option<String>,
    /// Approval answers accepted but not yet reflected by a refreshed snapshot.
    pub(crate) answered: BTreeSet<String>,
}

impl Default for SessionDetailState {
    fn default() -> Self {
        let mut composer = TextAreaState::default();
        composer.placeholder = text::COMPOSER_PLACEHOLDER.to_owned();
        Self {
            id: None,
            session: None,
            loading: false,
            stale: false,
            error: None,
            action_error: None,
            poll: PollState::new(),
            group_generation: 0,
            outstanding: 0,
            group_failed: false,
            last_request: None,
            reload_wanted: false,
            timeline: TimelineState::default(),
            turns: TurnsState::default(),
            composer,
            composer_error: None,
            send: None,
            approval: None,
            approval_error: None,
            answered: BTreeSet::new(),
        }
    }
}

impl SessionDetailState {
    /// A fresh state bound to `id`.
    #[must_use]
    pub fn for_session(id: &str) -> Self {
        Self {
            id: Some(id.to_owned()),
            ..Self::default()
        }
    }

    /// Session id this state belongs to.
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// Last good snapshot.
    #[must_use]
    pub fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }

    /// Whether the first snapshot is still loading.
    #[must_use]
    pub fn loading(&self) -> bool {
        self.loading
    }

    /// Whether the snapshot is older than the last failed refresh.
    #[must_use]
    pub fn stale(&self) -> bool {
        self.stale
    }

    /// Last snapshot failure.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Last mutation failure.
    #[must_use]
    pub fn action_error(&self) -> Option<&ActionError> {
        self.action_error.as_ref()
    }

    /// Timeline state.
    #[must_use]
    pub fn timeline(&self) -> &TimelineState {
        &self.timeline
    }

    /// Turns state.
    #[must_use]
    pub fn turns(&self) -> &TurnsState {
        &self.turns
    }

    /// Composer state.
    #[must_use]
    pub fn composer(&self) -> &TextAreaState {
        &self.composer
    }

    /// Inline composer error.
    #[must_use]
    pub fn composer_error(&self) -> Option<&str> {
        self.composer_error.as_deref()
    }

    /// Inline approval error.
    #[must_use]
    pub fn approval_error(&self) -> Option<&str> {
        self.approval_error.as_deref()
    }

    /// Parse the snapshot's `required_actions`.
    #[must_use]
    pub fn required_actions(&self) -> Vec<RequiredAction> {
        self.session
            .as_ref()
            .map(|session| parse_required_actions(&session.required_actions))
            .unwrap_or_default()
    }

    /// Pending approvals this dashboard can answer.
    #[must_use]
    pub fn approvals(&self) -> Vec<ApprovalEntry> {
        self.required_actions()
            .into_iter()
            .filter_map(|action| match action {
                RequiredAction::Approval(entry) => {
                    if self.answered.contains(&entry.request_id) {
                        None
                    } else {
                        Some(entry)
                    }
                }
                RequiredAction::Unsupported { .. } => None,
            })
            .collect()
    }

    /// First answerable approval id, for the keyboard fallback.
    #[must_use]
    pub fn first_approval_id(&self) -> Option<String> {
        self.approvals()
            .first()
            .map(|entry| entry.request_id.clone())
    }

    /// Session status (`idle` before the first snapshot).
    #[must_use]
    pub fn status(&self) -> &str {
        self.session
            .as_ref()
            .map_or("", |session| session.status.as_str())
    }

    /// Whether a turn is open.
    #[must_use]
    pub fn is_running(&self) -> bool {
        matches!(self.status(), "in_progress" | "requires_action")
    }

    /// Whether approvals block input.
    #[must_use]
    pub fn requires_action(&self) -> bool {
        self.status() == "requires_action"
    }

    /// Whether the composer must not send.
    #[must_use]
    pub fn composer_disabled(&self) -> bool {
        self.requires_action()
    }

    /// Whether an approval answer is in flight.
    #[must_use]
    pub fn approval_in_flight(&self) -> bool {
        self.approval.is_some()
    }

    /// Whether the composer submission is in flight.
    #[must_use]
    pub fn send_in_flight(&self) -> bool {
        self.send.as_ref().is_some_and(|send| send.in_flight)
    }

    /// Replace the whole state when the page opens a different session.
    pub(crate) fn reset_for(&mut self, id: &str) {
        let composer = std::mem::take(&mut self.composer);
        *self = Self::for_session(id);
        self.composer = composer;
        self.composer.text.clear();
        self.composer.cursor = 0;
    }

    /// Record whether the page viewport currently shows the newest item.
    pub(crate) fn set_following(&mut self, at_bottom: bool) {
        self.timeline.set_following(at_bottom);
    }

    /// Release any in-flight `Load earlier` / `Load more` markers.
    ///
    /// Called when a refresh invalidates the generation: the stale responses
    /// are discarded, so leaving the markers set would block `detail_due`
    /// forever.
    pub(crate) fn cancel_pagination(&mut self) {
        self.timeline.cancel_page();
        self.turns.cancel_page();
    }

    /// Release page markers whose generation is no longer current.
    ///
    /// This is the safety net for invalidations that happen outside a manual
    /// refresh (pause/resume), and for responses that arrive after the marker
    /// was already released.
    pub(crate) fn reconcile_pagination(&mut self) {
        let generation = self.poll.generation();
        self.timeline.reconcile_page(generation);
        self.turns.reconcile_page(generation);
    }
}

#[cfg(test)]
#[path = "session_detail_test.rs"]
mod tests;
