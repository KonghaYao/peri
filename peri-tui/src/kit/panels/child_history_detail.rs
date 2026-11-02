//! Read-only child timeline. A page or body chunk replaces its predecessor.
use std::sync::Arc;

use peri_acp::transport::types::AcpError;
use peri_acp_types::history_window::{HistoryBodyChunk, HistoryWindowError};
use ratatui_kit::prelude::State;
use ratatui_kit::ratatui::text::Line;
use serde_json::Value;
use unicode_width::UnicodeWidthChar;

use crate::application::operations::{CompletionGate, OperationScope, OperationTicket};
use crate::i18n;
use crate::kit::atoms::{
    ACP_CLIENT_HANDLE, ACTIVE_SESSION_ID, BRIDGE_RESET_COUNTER, SELECTED_SUBAGENT_ID,
};
use crate::kit::history_paging::{
    HistoryBodyRequest, WindowBodyDescriptor, WindowDirection, WindowOpenRequest, WindowPageCursor,
    WindowPageRequest, WindowPageResponse, WindowRequest, WindowRequestAnchor,
    decode_window_page_response,
};
use crate::kit::history_window::{HistorySource, WindowIdentity};

const PAGE_ITEMS: u16 = 20;
const PAGE_BYTES: u32 = 256 * 1024;
const TEXT_BYTES: usize = 512 * 1024;
const CHUNK_BYTES: u32 = 64 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Scope {
    parent: OperationScope,
    selected: String,
    child: String,
}

impl Scope {
    pub(super) fn current(selected: &str, child: &str) -> Self {
        Self {
            parent: OperationScope::new(
                ACTIVE_SESSION_ID.state().read().clone(),
                BRIDGE_RESET_COUNTER.get(),
            ),
            selected: selected.into(),
            child: child.into(),
        }
    }

    fn still_current(&self) -> bool {
        self == &Self::current(
            SELECTED_SUBAGENT_ID
                .state()
                .read()
                .as_deref()
                .unwrap_or_default(),
            &self.child,
        )
    }
}

#[derive(Default)]
pub(super) struct ChildHistoryView {
    gate: CompletionGate<Scope>,
    pub(super) enabled: bool,
    page: Option<Page>,
    body: Option<Body>,
    error: Option<String>,
}

struct Page {
    identity: WindowIdentity,
    committed_tail: Option<String>,
    ticket: String,
    before: Option<String>,
    after: Option<String>,
    text: Arc<str>,
    bodies: Vec<WindowBodyDescriptor>,
    selected_body: usize,
}

struct Body {
    request: HistoryBodyRequest,
    text: Arc<str>,
    next: Option<u64>,
}

#[derive(Clone)]
enum Request {
    Page(WindowRequest),
    Body(HistoryBodyRequest),
}

enum ReadFailure {
    Remote(AcpError),
    Disconnected,
}

impl ReadFailure {
    fn invalidates_snapshot(&self) -> bool {
        let Self::Remote(error) = self else {
            return false;
        };
        let Some(reason) = error
            .data
            .as_ref()
            .and_then(|data| data.get("reason"))
            .and_then(Value::as_str)
        else {
            return false;
        };
        // Match the shared wire contract, never the error's human-readable message.
        [
            HistoryWindowError::SessionNotFound,
            HistoryWindowError::SessionClosing,
            HistoryWindowError::MutationInProgress,
            HistoryWindowError::NotNegotiated,
            HistoryWindowError::CrossSession {
                expected: String::new(),
                actual: String::new(),
            },
            HistoryWindowError::WindowInvalidated {
                detail: String::new(),
            },
            HistoryWindowError::ReadTicketInvalid {
                detail: String::new(),
            },
            HistoryWindowError::ReadTicketExpired,
            HistoryWindowError::SnapshotMismatch {
                detail: String::new(),
            },
            HistoryWindowError::AnchorUnavailable {
                detail: String::new(),
            },
        ]
        .iter()
        .any(|kind| error.code == i64::from(kind.code()) && reason == kind.reason())
    }

    fn message(&self) -> String {
        match self {
            Self::Remote(error) => format!(
                "ACP error [{}]: {}",
                error.code,
                bounded_error(&error.message)
            ),
            Self::Disconnected => i18n::tr("thread-history-disconnected"),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Action {
    Open,
    Older,
    Newer,
    Body,
    NextChunk,
    RestartBody,
}

impl ChildHistoryView {
    pub(super) fn reset(&mut self, scope: Scope) {
        if self.gate.reset_scope(scope) {
            self.clear_content();
            self.enabled = false;
        }
    }

    pub(super) fn scope_for(&self, selected: &str, child: Option<&str>) -> Scope {
        // Keep a previously validated locator when its temporary preview expires.
        let child = child
            .or_else(|| {
                (self.gate.scope().selected == selected).then_some(self.gate.scope().child.as_str())
            })
            .unwrap_or_default();
        Scope::current(selected, child)
    }

    pub(super) fn matches(&self, scope: &Scope) -> bool {
        self.gate.scope_matches(scope)
    }

    pub(super) fn close(&mut self) {
        self.gate.cancel();
        self.enabled = false;
        self.clear_content();
    }

    fn clear_content(&mut self) {
        self.page = None;
        self.body = None;
        self.error = None;
    }

    pub(super) fn back_to_page(&mut self) {
        self.gate.cancel();
        self.body = None;
        self.error = None;
    }

    pub(super) fn select_body(&mut self, backwards: bool) {
        if self.gate.is_pending() || self.body.is_some() {
            return;
        }
        if let Some(page) = &mut self.page {
            let count = page.bodies.len();
            if count != 0 {
                page.selected_body = if backwards {
                    (page.selected_body + count - 1) % count
                } else {
                    (page.selected_body + 1) % count
                };
            }
        }
    }

    fn begin(&mut self, action: Action) -> Option<(OperationTicket<Scope>, Request)> {
        if self.gate.is_pending()
            || self.gate.scope().parent.session_id().is_empty()
            || self.gate.scope().child.is_empty()
        {
            return None;
        }
        let request = match action {
            Action::Open => Request::Page(WindowRequest::Open(WindowOpenRequest {
                anchor: WindowRequestAnchor::Latest,
                direction: WindowDirection::Before,
                limit: PAGE_ITEMS,
                before_limit: None,
                after_limit: None,
                byte_budget: PAGE_BYTES,
            })),
            Action::Older | Action::Newer => {
                let page = self.page.as_ref()?;
                let cursor = match action {
                    Action::Older => WindowPageCursor::Before(page.before.clone()?),
                    _ => WindowPageCursor::After(page.after.clone()?),
                };
                Request::Page(WindowRequest::Page(WindowPageRequest {
                    snapshot_token: page.identity.snapshot_token.clone(),
                    read_ticket: page.ticket.clone(),
                    cursor,
                    limit: PAGE_ITEMS,
                }))
            }
            Action::Body => {
                let page = self.page.as_ref()?;
                let body = page.bodies.get(page.selected_body)?;
                Request::Body(HistoryBodyRequest {
                    entry_id: body.entry_id.clone(),
                    source: HistorySource::Own,
                    snapshot_token: body
                        .snapshot_token
                        .clone()
                        .unwrap_or_else(|| page.identity.snapshot_token.clone()),
                    read_ticket: body.read_ticket.clone(),
                    offset: 0,
                    max_bytes: CHUNK_BYTES,
                })
            }
            Action::NextChunk | Action::RestartBody => {
                let body = self.body.as_ref()?;
                let offset = match action {
                    Action::NextChunk => body.next?,
                    _ => 0,
                };
                if offset == body.request.offset {
                    return None;
                }
                Request::Body(HistoryBodyRequest {
                    offset,
                    ..body.request.clone()
                })
            }
        };
        self.enabled = true;
        self.error = None;
        let ticket = self.gate.begin(self.gate.scope().clone());
        Some((ticket, request))
    }

    fn finish(
        &mut self,
        ticket: &OperationTicket<Scope>,
        request: &Request,
        raw: Result<Value, ReadFailure>,
    ) -> bool {
        if !self.gate.accept(ticket) {
            return false;
        }
        let raw = match raw {
            Ok(raw) => raw,
            Err(error) => {
                if error.invalidates_snapshot() {
                    self.clear_content();
                }
                self.error = Some(bounded_error(&error.message()));
                return true;
            }
        };
        let mut invalidate = false;
        let result = (|| {
            if raw.get("scope").and_then(Value::as_str) != Some("child")
                || raw.get("parentSessionId").and_then(Value::as_str)
                    != Some(ticket.scope().parent.session_id())
            {
                return Err(i18n::tr("subagent-history-invalid-response"));
            }
            if raw
                .get("rewindInvalidation")
                .and_then(|value| value.get("reopenRequired"))
                .and_then(Value::as_bool)
                == Some(true)
            {
                invalidate = true;
                return Err(i18n::tr("subagent-history-invalid-response"));
            }
            let committed_tail = match raw.get("committedTail") {
                Some(Value::Null) => None,
                Some(Value::String(tail)) => Some(tail.clone()),
                _ => return Err(i18n::tr("subagent-history-invalid-response")),
            };
            match request {
                Request::Page(request) => {
                    let response = decode_window_page_response(&raw)
                        .ok_or_else(|| i18n::tr("subagent-history-invalid-response"))?;
                    let prefix = format!("{}:", ticket.scope().child);
                    if response.session_id != ticket.scope().child
                        || response.items.len() > usize::from(PAGE_ITEMS)
                        || response.body_descriptors.len() > usize::from(PAGE_ITEMS)
                        || response
                            .items
                            .iter()
                            .any(|item| !item.entry_id.starts_with(&prefix))
                        || response
                            .body_descriptors
                            .iter()
                            .any(|body| !body.entry_id.starts_with(&prefix))
                    {
                        return Err(i18n::tr("subagent-history-invalid-response"));
                    }
                    if matches!(request, WindowRequest::Page(_))
                        && self
                            .page
                            .as_ref()
                            .is_none_or(|page| page.committed_tail != committed_tail)
                    {
                        invalidate = true;
                        return Err(i18n::tr("subagent-history-invalid-response"));
                    }
                    if let WindowRequest::Page(expected) = request
                        && (response.identity.snapshot_token != expected.snapshot_token
                            || response.read_ticket != expected.read_ticket
                            || self
                                .page
                                .as_ref()
                                .is_none_or(|page| page.identity != response.identity))
                    {
                        return Err(i18n::tr("subagent-history-invalid-response"));
                    }
                    self.page = Some(Page::from_response(response)?);
                    self.body = None;
                }
                Request::Body(request) => {
                    if raw.get("childThreadId").and_then(Value::as_str)
                        != Some(ticket.scope().child.as_str())
                        || raw.get("snapshotToken").and_then(Value::as_str)
                            != Some(request.snapshot_token.as_str())
                        || raw.get("format").and_then(Value::as_str) != Some("display_text")
                        || raw.get("source").and_then(Value::as_str) != Some("own")
                        || raw
                            .get("bytes")
                            .and_then(Value::as_array)
                            .is_none_or(|bytes| bytes.len() > request.max_bytes as usize)
                    {
                        return Err(i18n::tr("subagent-history-invalid-response"));
                    }
                    if self
                        .page
                        .as_ref()
                        .is_none_or(|page| page.committed_tail != committed_tail)
                    {
                        invalidate = true;
                        return Err(i18n::tr("subagent-history-invalid-response"));
                    }
                    let chunk: HistoryBodyChunk = serde_json::from_value(raw)
                        .map_err(|_| i18n::tr("subagent-history-invalid-response"))?;
                    chunk
                        .validate(&request.entry_id, request.offset, request.max_bytes)
                        .map_err(|_| i18n::tr("subagent-history-invalid-response"))?;
                    let text = String::from_utf8(chunk.bytes)
                        .map_err(|_| i18n::tr("subagent-history-invalid-response"))?;
                    self.body = Some(Body {
                        request: request.clone(),
                        text: text.into(),
                        next: chunk.next_offset,
                    });
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            if invalidate {
                self.clear_content();
            }
            self.error = Some(bounded_error(&error));
        }
        true
    }

    pub(super) fn presentation(&self, width: usize) -> (Vec<Line<'static>>, Arc<str>) {
        let mut lines = Vec::new();
        wrap(&i18n::tr("subagent-history-title"), width, &mut lines);
        wrap(
            &i18n::tr(if self.body.is_some() {
                "subagent-history-body-keys"
            } else {
                "subagent-history-page-keys"
            }),
            width,
            &mut lines,
        );
        if self.gate.is_pending() {
            wrap(&i18n::tr("subagent-history-loading"), width, &mut lines);
        }
        if let Some(error) = &self.error {
            wrap(error, width, &mut lines);
        }
        if let Some(body) = &self.body {
            wrap(
                &format!("{} [{}]", body.request.entry_id, body.request.offset),
                width,
                &mut lines,
            );
            return (lines, body.text.clone());
        } else if let Some(page) = &self.page {
            for (index, body) in page.bodies.iter().enumerate() {
                let marker = if index == page.selected_body {
                    ">"
                } else {
                    " "
                };
                wrap(
                    &format!(
                        "{marker} {}: {}",
                        i18n::tr("subagent-history-large-entry"),
                        body.entry_id
                    ),
                    width,
                    &mut lines,
                );
            }
            if page.text.is_empty() && page.bodies.is_empty() {
                wrap(&i18n::tr("subagent-history-empty-page"), width, &mut lines);
            } else {
                return (lines, page.text.clone());
            }
        }
        (lines, Arc::from(""))
    }
}

impl Page {
    fn from_response(response: WindowPageResponse) -> Result<Self, String> {
        if response
            .items
            .iter()
            .any(|item| item.source != HistorySource::Own)
            || response.body_descriptors.iter().any(|body| {
                body.source
                    .as_ref()
                    .is_some_and(|source| *source != HistorySource::Own)
            })
        {
            return Err(i18n::tr("subagent-history-invalid-response"));
        }
        let mut text = String::new();
        for item in &response.items {
            for sub in &item.sub_items {
                if sub.system_reminder {
                    let reminder = serde_json::from_value(sub.update.clone())
                        .ok()
                        .and_then(crate::kit::tui_render_unit::TuiSystemReminder::from_wire);
                    if let Some(reminder) = reminder {
                        append(&mut text, &reminder.summary)?;
                        append(&mut text, &reminder.body)?;
                    }
                    continue;
                }
                use crate::kit::acp_types::AcpEventData;
                match crate::kit::acp_notifier::decode_history_sub_item(&sub.update) {
                    Some(AcpEventData::CommittedAssistantText {
                        text: body,
                        reasoning,
                    }) => {
                        append(&mut text, &i18n::tr("thread-history-assistant"))?;
                        if let Some(reasoning) = reasoning {
                            append(&mut text, &reasoning)?;
                        }
                        append(&mut text, &body)?;
                    }
                    Some(AcpEventData::ReplayedUserBubble { text: body, .. })
                    | Some(AcpEventData::LocalUserBubble { text: body, .. }) => {
                        append(&mut text, &i18n::tr("thread-history-user"))?;
                        append(&mut text, &body)?;
                    }
                    Some(AcpEventData::ReplayToolStarted {
                        tool_name,
                        raw_input,
                        ..
                    }) => {
                        append(&mut text, &tool_name)?;
                        append(&mut text, &raw_input.to_string())?;
                    }
                    Some(AcpEventData::ReplayToolEnded {
                        output_summary,
                        is_error,
                        ..
                    }) => {
                        append(
                            &mut text,
                            &i18n::tr(if is_error {
                                "subagent-history-tool-error"
                            } else {
                                "thread-history-tool"
                            }),
                        )?;
                        append(&mut text, &output_summary)?;
                    }
                    _ => {}
                }
            }
        }
        let mut bodies = Vec::new();
        for item in response.items {
            if let Some(body) = item.descriptor {
                if body.entry_id != item.entry_id
                    || body
                        .source
                        .as_ref()
                        .is_some_and(|source| *source != HistorySource::Own)
                    || body.read_ticket != response.read_ticket
                    || body.snapshot_token.as_deref()
                        != Some(response.identity.snapshot_token.as_str())
                {
                    return Err(i18n::tr("subagent-history-invalid-response"));
                }
                bodies.push(body);
            }
        }
        if bodies.len() > usize::from(PAGE_ITEMS) {
            return Err(i18n::tr("subagent-history-invalid-response"));
        }
        Ok(Self {
            identity: response.identity,
            committed_tail: response.committed_tail,
            ticket: response.read_ticket,
            before: response.before_cursor,
            after: response.after_cursor,
            text: text.into(),
            bodies,
            selected_body: 0,
        })
    }
}

fn append(output: &mut String, value: &str) -> Result<(), String> {
    if output.len().saturating_add(value.len()).saturating_add(2) > TEXT_BYTES {
        return Err(i18n::tr("subagent-history-page-too-large"));
    }
    output.reserve_exact(value.len().saturating_add(2));
    output.push_str(value);
    output.push_str("\n\n");
    Ok(())
}

fn bounded_error(error: &str) -> String {
    let mut end = error.len().min(1024);
    while !error.is_char_boundary(end) {
        end -= 1;
    }
    error[..end].into()
}

fn wrap(text: &str, width: usize, lines: &mut Vec<Line<'static>>) {
    let width = width.max(2);
    for input in text.split('\n') {
        let mut output = String::new();
        let mut columns = 0;
        for ch in input.chars().filter(|ch| !ch.is_control()) {
            let count = ch.width().unwrap_or(0);
            if columns + count > width && !output.is_empty() {
                lines.push(Line::from(std::mem::take(&mut output)));
                columns = 0;
            }
            output.push(ch);
            columns += count;
        }
        lines.push(Line::from(output));
    }
}

pub(super) fn request(view: State<ChildHistoryView>, action: Action) {
    let pending = view.write().begin(action);
    let Some((ticket, request)) = pending else {
        return;
    };
    let Some(client) = ACP_CLIENT_HANDLE.get().cloned() else {
        view.write()
            .finish(&ticket, &request, Err(ReadFailure::Disconnected));
        return;
    };
    tokio::spawn(async move {
        let scope = ticket.scope();
        if !scope.still_current()
            || client.current_session_id().as_deref() != Some(scope.parent.session_id())
        {
            if scope.still_current() {
                view.write()
                    .finish(&ticket, &request, Err(ReadFailure::Disconnected));
            }
            return;
        }
        let result = tokio::select! {
            _ = ticket.token().cancelled() => return,
            result = async {
                match &request {
                    Request::Page(request) => client.load_child_history_window(
                        scope.parent.session_id(), &scope.child, request).await,
                    Request::Body(request) => client.load_child_history_body(
                        scope.parent.session_id(), &scope.child, request).await,
                }
            } => result.map_err(ReadFailure::Remote),
        };
        if ticket.scope().still_current() {
            view.write().finish(&ticket, &request, result);
        }
    });
}

#[cfg(test)]
#[path = "child_history_detail_test.rs"]
mod tests;
