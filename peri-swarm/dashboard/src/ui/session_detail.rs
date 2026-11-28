//! Session detail page: header, approvals, tabs, timeline, turns, info and the
//! message composer.
//!
//! Every clickable region comes from this render pass. The timeline renders the
//! official `data` projection grouped by `turn_id`; message text goes through
//! the shared Markdown renderer, reasoning is muted, and tool blocks expand on
//! click. One page is drawn in local coordinates by [`crate::ui::pages`] and
//! lifted into the shell by the hit map.

use peri_theme::bridge::ThemeDefinitionExt;
use peri_tui_components::button::ButtonTone;
use peri_tui_components::markdown::{parse_markdown, MarkdownSegment};
use peri_tui_components::tabs::tab_layout;
use peri_tui_components::textarea::{render_text_area_with_cursor, TextAreaStyles};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::app::{
    ActionId, App, DetailTab, FieldId, Focus, Intent, RequiredAction, TextAreaClick, TimelineEntry,
    TimelineKind,
};
use crate::ui::hit::{HitMap, Layer};
use crate::ui::text;
use crate::ui::widgets::{self, Rows};

/// Composer inner rows.
const COMPOSER_ROWS: u16 = 4;
/// Maximum Markdown lines drawn for one message.
const MESSAGE_ROWS: usize = 12;
/// Maximum reasoning lines drawn.
const REASONING_ROWS: usize = 5;
/// Maximum expanded tool lines drawn.
const TOOL_ROWS: usize = 8;
/// Maximum columns given to the timeline entry indent.
const ENTRY_INDENT: u16 = 2;
/// Turns-table rows per block. Chunks bound the height of one scrollable
/// block so a partially visible table still draws the correct rows.
const TABLE_CHUNK_ROWS: usize = 40;

/// Render the session detail body.
pub fn render(buf: &mut Buffer, rows: &mut Rows, app: &App, hits: &mut HitMap) -> Option<Position> {
    let detail = app.detail();
    let id = app.detail_id()?;
    let mut cursor = None;

    widgets::render_body(buf, rows.line(), text::DETAIL_TITLE, widgets::title_style());
    widgets::render_body(
        buf,
        rows.line(),
        &text::session_id_label(id),
        widgets::muted_style(),
    );
    render_summary(buf, rows, detail.session(), app.spinner_frame());
    if detail.stale() {
        render_wrapped(buf, rows, text::DETAIL_STALE, widgets::warning_style());
    }
    if let Some(error) = detail.error() {
        render_wrapped(buf, rows, error, widgets::error_style());
    }
    rows.skip(1);
    render_actions(buf, rows, app, hits);
    if let Some(error) = detail.action_error() {
        render_wrapped(buf, rows, &error.message, widgets::error_style());
        if error.offer_cancel {
            let row = rows.line();
            if let Some(rect) = render_action_button(
                buf,
                row,
                app,
                hits,
                ActionId::CancelTurn,
                ButtonTone::Danger,
            ) {
                hits.region(Layer::Base, rect, Intent::OpenCancelTurn);
            }
        }
    }
    render_approvals(buf, rows, app, hits);
    rows.skip(1);
    render_tab_bar(buf, rows, app, hits);
    render_composer(buf, rows, app, hits, &mut cursor);
    match app.detail_tab() {
        DetailTab::Timeline => render_timeline(buf, rows, app, hits),
        DetailTab::Turns => render_turns(buf, rows, app, hits),
        DetailTab::Info => render_info(buf, rows, app),
    }
    cursor
}

fn render_summary(
    buf: &mut Buffer,
    rows: &mut Rows,
    session: Option<&crate::api::Session>,
    frame: u64,
) {
    let Some(session) = session else {
        widgets::render_loading(
            buf,
            rows.line(),
            frame,
            text::DETAIL_LOADING,
            widgets::dim_style(),
        );
        return;
    };
    let row = rows.line();
    let tone = match session.status.as_str() {
        "failed" => widgets::StatusTone::Error,
        "requires_action" => widgets::StatusTone::Warning,
        "in_progress" => widgets::StatusTone::Ok,
        _ => widgets::StatusTone::Muted,
    };
    let mut spans = vec![widgets::status_dot(tone), Span::raw(" ")];
    spans.push(Span::styled(
        session.status.clone(),
        widgets::tone_style(tone),
    ));
    let model = session.agent.model.as_deref().unwrap_or(text::DASH);
    spans.push(Span::styled(
        format!("  ·  {model}"),
        widgets::muted_style(),
    ));
    spans.push(Span::styled(
        format!(
            "  ·  {}",
            text::environment_value(&session.environment.kind, session.environment.id.as_deref())
        ),
        widgets::muted_style(),
    ));
    Paragraph::new(Line::from(spans)).render(row, buf);

    let now = chrono::Utc::now().timestamp();
    let facts = text::created_last_active(
        &text::relative_time(session.created_at, now),
        &text::relative_time(session.last_active_at, now),
    );
    widgets::render_body(buf, rows.line(), &facts, widgets::dim_style());
    if let Some(error) = &session.error {
        render_wrapped(buf, rows, error, widgets::error_style());
    }
}

fn render_actions(buf: &mut Buffer, rows: &mut Rows, app: &App, hits: &mut HitMap) {
    let mut actions = vec![
        (ActionId::Back, ButtonTone::Secondary),
        (ActionId::RefreshDetail, ButtonTone::Secondary),
    ];
    if app.detail().session().is_some() {
        actions.push((ActionId::EditSession, ButtonTone::Primary));
        actions.push((ActionId::DeleteSession, ButtonTone::Danger));
    }
    if app.detail().is_running() {
        actions.push((ActionId::CancelTurn, ButtonTone::Danger));
    }
    // Wrap onto another row when the narrow layout cannot fit every action.
    let mut row = rows.line();
    let mut x = row.x;
    for (action, tone) in actions {
        let label = text::action_label(action);
        let width = widgets::button_width(label, tone);
        if x > row.x && x.saturating_add(width) > row.right() {
            row = rows.line();
            x = row.x;
        }
        let rect = widgets::button_rect(row, x, label, tone);
        widgets::render_button(buf, rect, label, app.focus() == Focus::Action(action), tone);
        hits.region(Layer::Base, rect, Intent::Activate(action));
        x = rect.right().saturating_add(2);
    }
}

fn render_action_button(
    buf: &mut Buffer,
    row: Rect,
    app: &App,
    hits: &mut HitMap,
    action: ActionId,
    tone: ButtonTone,
) -> Option<Rect> {
    let label = text::action_label(action);
    let rect = widgets::button_rect(row, row.x, label, tone);
    if rect.is_empty() {
        return None;
    }
    widgets::render_button(buf, rect, label, app.focus() == Focus::Action(action), tone);
    hits.region(Layer::Base, rect, Intent::Activate(action));
    Some(rect)
}

fn render_approvals(buf: &mut Buffer, rows: &mut Rows, app: &App, hits: &mut HitMap) {
    let detail = app.detail();
    let actions = detail.required_actions();
    if actions.is_empty() {
        return;
    }
    rows.skip(1);
    widgets::render_section(buf, rows.line(), text::SECTION_APPROVALS);
    if detail.approval_in_flight() {
        widgets::render_body(
            buf,
            rows.line(),
            text::APPROVAL_IN_FLIGHT,
            widgets::dim_style(),
        );
    }
    if let Some(error) = detail.approval_error() {
        render_wrapped(buf, rows, error, widgets::error_style());
    }
    for action in actions {
        match action {
            RequiredAction::Unsupported { kind } => {
                render_wrapped(
                    buf,
                    rows,
                    &text::unsupported_action(&kind),
                    widgets::warning_style(),
                );
            }
            RequiredAction::Approval(entry) => {
                render_approval(buf, rows, app, hits, &entry);
            }
        }
    }
}

fn render_approval(
    buf: &mut Buffer,
    rows: &mut Rows,
    app: &App,
    hits: &mut HitMap,
    entry: &crate::app::ApprovalEntry,
) {
    let detail = app.detail();
    let heading = text::approval_heading(&entry.server_label, entry.tool.as_deref());
    widgets::render_body(buf, rows.line(), &heading, widgets::section_style());
    if let Some(message) = &entry.message {
        render_wrapped(buf, rows, message, widgets::muted_style());
    }
    let arguments = crate::app::pretty_json(&entry.arguments);
    for line in arguments.lines().take(TOOL_ROWS) {
        widgets::render_body(buf, rows.line(), line, widgets::dim_style());
    }
    let row = rows.line();
    let disabled = detail.approval_in_flight();
    let mut x = row.x;
    for (label, decision, focused, tone) in [
        (
            text::APPROVE,
            crate::api::ApprovalDecision::Approve,
            app.focus() == Focus::Action(ActionId::ApproveFirst),
            ButtonTone::Primary,
        ),
        (
            text::DENY,
            crate::api::ApprovalDecision::Deny,
            app.focus() == Focus::Action(ActionId::DenyFirst),
            ButtonTone::Danger,
        ),
    ] {
        let width = widgets::button_width(label, tone);
        if x.saturating_add(width) > row.right() {
            break;
        }
        let rect = widgets::button_rect(row, x, label, tone);
        widgets::render_button(buf, rect, label, focused && !disabled, tone);
        if !disabled {
            hits.region(
                Layer::Base,
                rect,
                Intent::AnswerApproval {
                    request_id: entry.request_id.clone(),
                    decision,
                },
            );
        }
        x = rect.right().saturating_add(2);
    }
}

/// Draw the tab bar and register one click region per tab.
pub fn render_tab_bar(buf: &mut Buffer, rows: &mut Rows, app: &App, hits: &mut HitMap) {
    let area = rows.line();
    if area.is_empty() {
        return;
    }
    let selected = DetailTab::ALL
        .iter()
        .position(|tab| *tab == app.detail_tab())
        .unwrap_or(0);
    let labels: Vec<&str> = DetailTab::ALL.iter().map(|tab| tab.label()).collect();
    widgets::render_tabs(buf, area, &labels, selected);
    // Hit regions mirror the drawn labels: `ratatui::widgets::Tabs` pads every
    // title with one space on each side and draws the `|` divider between
    // titles, so adjacent labels start five columns apart.
    let layout = tab_layout(area, &labels, selected);
    for (tab, rect) in DetailTab::ALL.iter().zip(layout.rects.iter()) {
        hits.region(Layer::Base, *rect, Intent::SelectDetailTab(*tab));
    }
}

fn render_composer(
    buf: &mut Buffer,
    rows: &mut Rows,
    app: &App,
    hits: &mut HitMap,
    cursor: &mut Option<Position>,
) {
    let detail = app.detail();
    widgets::render_body(
        buf,
        rows.line(),
        text::field_label(FieldId::DetailComposer),
        widgets::section_style(),
    );
    let rect = rows.take(COMPOSER_ROWS.saturating_add(2));
    let focused = app.focus() == Focus::Field(FieldId::DetailComposer);
    let styles = TextAreaStyles {
        cursor: widgets::selected_style(),
        selection: widgets::selected_style(),
        placeholder: widgets::dim_style(),
        base: widgets::muted_style(),
    };
    if let Some(position) = render_text_area_with_cursor(
        buf,
        rect,
        detail.composer(),
        focused,
        widgets::border_style(focused),
        styles,
        Some(&detail.composer().placeholder),
    ) {
        *cursor = Some(position);
    }
    hits.region(
        Layer::Base,
        rect,
        Intent::FocusTextAreaAt {
            field: FieldId::DetailComposer,
            click: TextAreaClick {
                row: 0,
                column: 0,
                width: rect.width.saturating_sub(2),
                height: rect.height.saturating_sub(2),
            },
        },
    );
    if let Some(error) = detail.composer_error() {
        render_wrapped(buf, rows, error, widgets::error_style());
    }
    let row = rows.line();
    let label = text::action_label(ActionId::SendMessage);
    let rect = widgets::button_rect(row, row.x, label, ButtonTone::Primary);
    widgets::render_button(
        buf,
        rect,
        label,
        app.focus() == Focus::Action(ActionId::SendMessage),
        ButtonTone::Primary,
    );
    hits.region(Layer::Base, rect, Intent::Activate(ActionId::SendMessage));
    if detail.composer_disabled() {
        widgets::render_body(
            buf,
            rows.line(),
            text::COMPOSER_BLOCKED_APPROVAL,
            widgets::warning_style(),
        );
    } else if detail.send_in_flight() {
        widgets::render_loading(
            buf,
            rows.line(),
            app.spinner_frame(),
            text::COMPOSER_SENDING,
            widgets::dim_style(),
        );
    }
}

fn render_timeline(buf: &mut Buffer, rows: &mut Rows, app: &App, hits: &mut HitMap) {
    let timeline = app.detail().timeline();
    rows.skip(1);
    // `Load earlier` stays at the top of the scrolling content so it appears
    // when the user scrolls up. `Jump to latest` is drawn as a fixed control by
    // the screen renderer so it can never be clipped by the scroll window.
    let action = ActionId::LoadEarlierItems;
    let label = text::action_label(action);
    let mut drawn = false;
    if timeline.can_load_earlier() {
        let row = rows.line();
        let width = widgets::button_width(label, ButtonTone::Secondary);
        if row.x.saturating_add(width) <= row.right() {
            let rect = widgets::button_rect(row, row.x, label, ButtonTone::Secondary);
            widgets::render_button(
                buf,
                rect,
                label,
                app.focus() == Focus::Action(action),
                ButtonTone::Secondary,
            );
            hits.region(Layer::Base, rect, Intent::Activate(action));
            drawn = true;
        }
    }
    if !drawn {
        rows.skip(1);
    }
    if timeline.unseen() {
        render_wrapped(buf, rows, text::TIMELINE_UNSEEN, widgets::warning_style());
    }
    if timeline.loading_more() {
        widgets::render_loading(
            buf,
            rows.line(),
            app.spinner_frame(),
            text::TIMELINE_LOADING_OLDER,
            widgets::dim_style(),
        );
    }
    if let Some(error) = timeline.error() {
        render_wrapped(buf, rows, error, widgets::error_style());
    }
    if timeline.cap_reached() {
        render_wrapped(
            buf,
            rows,
            text::TIMELINE_LIMIT_REACHED,
            widgets::dim_style(),
        );
    }
    if !timeline.ever_loaded() {
        widgets::render_loading(
            buf,
            rows.line(),
            app.spinner_frame(),
            text::TIMELINE_LOADING,
            widgets::dim_style(),
        );
        return;
    }
    if timeline.entries().is_empty() {
        widgets::render_body(buf, rows.line(), text::TIMELINE_EMPTY, widgets::dim_style());
        return;
    }
    let mut current_turn = None;
    for entry in timeline.entries() {
        if current_turn.as_deref() != Some(entry.turn_id.as_str()) {
            current_turn = Some(entry.turn_id.clone());
            let heading = if entry.turn_id.is_empty() {
                text::TURN_UNASSIGNED.to_owned()
            } else {
                text::turn_heading(&entry.turn_id)
            };
            widgets::render_body(buf, rows.line(), &heading, widgets::section_style());
        }
        render_entry(buf, rows, app, hits, entry);
    }
}

fn render_entry(
    buf: &mut Buffer,
    rows: &mut Rows,
    app: &App,
    hits: &mut HitMap,
    entry: &TimelineEntry,
) {
    let expanded = app.detail().timeline().is_expanded(&entry.id);
    match entry.kind {
        TimelineKind::UserMessage => {
            widgets::render_body(buf, rows.line(), text::ROLE_USER, widgets::section_style());
            render_markdown(buf, rows, &entry.text, MESSAGE_ROWS);
        }
        TimelineKind::AssistantMessage => {
            widgets::render_body(
                buf,
                rows.line(),
                text::ROLE_ASSISTANT,
                widgets::section_style(),
            );
            render_markdown(buf, rows, &entry.text, MESSAGE_ROWS);
        }
        TimelineKind::Reasoning => {
            widgets::render_body(buf, rows.line(), text::ROLE_REASONING, widgets::dim_style());
            let body_rows = rows.take(
                widgets::wrapped_height(&entry.text, rows.width()).min(REASONING_ROWS as u16),
            );
            widgets::render_body(buf, body_rows, &entry.text, widgets::muted_style());
        }
        TimelineKind::ToolCall | TimelineKind::ToolOutput => {
            let label = match entry.kind {
                TimelineKind::ToolCall => text::tool_call_heading(entry.tool.as_deref()),
                _ => text::tool_output_heading(entry.call_id.as_deref()),
            };
            let marker = if expanded {
                text::EXPANDED
            } else {
                text::COLLAPSED
            };
            let line = format!("{marker} {label}");
            let row = rows.line();
            widgets::render_body(
                buf,
                row,
                &line,
                if expanded {
                    widgets::muted_style()
                } else {
                    widgets::section_style()
                },
            );
            if entry.has_details() {
                hits.region(
                    Layer::Base,
                    row,
                    Intent::ToggleTimelineBlock(entry.id.clone()),
                );
            }
            if expanded {
                render_tool_details(buf, rows, entry);
            }
        }
        TimelineKind::Other => {
            widgets::render_body(
                buf,
                rows.line(),
                &text::item_heading(&entry.label),
                widgets::dim_style(),
            );
            if !entry.text.is_empty() {
                render_markdown(buf, rows, &entry.text, REASONING_ROWS);
            }
        }
    }
}

fn render_tool_details(buf: &mut Buffer, rows: &mut Rows, entry: &TimelineEntry) {
    if let Some(arguments) = &entry.arguments {
        for line in arguments.lines().take(TOOL_ROWS) {
            widgets::render_body(buf, rows.line(), line, widgets::dim_style());
        }
    }
    if let Some(error) = &entry.error {
        render_wrapped(buf, rows, error, widgets::error_style());
    }
    if !entry.text.is_empty() {
        for line in entry.text.lines().take(TOOL_ROWS) {
            widgets::render_body(buf, rows.line(), line, widgets::muted_style());
        }
    }
}

fn render_markdown(buf: &mut Buffer, rows: &mut Rows, body: &str, max_rows: usize) {
    if body.trim().is_empty() {
        return;
    }
    let width = usize::from(rows.width().saturating_sub(ENTRY_INDENT).max(1));
    let palette = widgets::theme().to_palette();
    let base = widgets::theme().component.markdown.text;
    let segments = parse_markdown(body, width, palette, base);
    let mut lines: Vec<Line<'static>> = Vec::new();
    for segment in segments {
        match segment {
            MarkdownSegment::Text(mut text) => lines.append(&mut text),
            MarkdownSegment::Table(table) => {
                lines.extend(
                    peri_tui_components::markdown::table_data_to_lines_with_theme(
                        &table,
                        width,
                        widgets::theme(),
                    ),
                );
            }
            MarkdownSegment::Image(image) => lines.extend(image.lines),
            MarkdownSegment::GenUi(genui) => {
                lines.push(Line::from(genui.source));
            }
            MarkdownSegment::Mermaid(mermaid) => lines.extend(mermaid.lines),
        }
        if lines.len() >= max_rows {
            break;
        }
    }
    let truncated = lines.len() > max_rows;
    lines.truncate(max_rows);
    if truncated {
        if let Some(last) = lines.last_mut() {
            last.spans
                .push(Span::styled(text::ELLIPSIS, widgets::dim_style()));
        }
    }
    let height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let area = rows.take(height);
    Paragraph::new(lines).render(area, buf);
}

fn render_turns(buf: &mut Buffer, rows: &mut Rows, app: &App, hits: &mut HitMap) {
    let turns = app.detail().turns();
    rows.skip(1);
    if turns.loading_more() {
        widgets::render_loading(
            buf,
            rows.line(),
            app.spinner_frame(),
            text::TIMELINE_LOADING_OLDER,
            widgets::dim_style(),
        );
    }
    if let Some(error) = turns.error() {
        render_wrapped(buf, rows, error, widgets::error_style());
    }
    if !turns.ever_loaded() {
        widgets::render_loading(
            buf,
            rows.line(),
            app.spinner_frame(),
            text::TURNS_LOADING,
            widgets::dim_style(),
        );
        return;
    }
    if turns.rows().is_empty() {
        widgets::render_body(buf, rows.line(), text::TURNS_EMPTY, widgets::dim_style());
        return;
    }
    let now = chrono::Utc::now().timestamp();
    let headers = [
        text::TURN_COL_ID,
        text::TURN_COL_STATUS,
        text::TURN_COL_STARTED,
        text::TURN_COL_COMPLETED,
        text::TURN_COL_ERROR,
    ];
    let data: Vec<Vec<String>> = turns
        .rows()
        .iter()
        .map(|turn| {
            vec![
                turn.id.clone(),
                turn.status.clone(),
                relative_optional(turn.started_at, now),
                relative_optional(turn.completed_at, now),
                turn.error
                    .as_ref()
                    .map(serde_json::Value::to_string)
                    .unwrap_or_else(|| text::DASH.to_owned()),
            ]
        })
        .collect();
    let width = rows.width();
    let id_width = (width / 3).clamp(10, 40);
    let widths = [
        Constraint::Length(id_width),
        Constraint::Length(12),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Min(4),
    ];
    let mut first_chunk = true;
    for chunk in data.chunks(TABLE_CHUNK_ROWS) {
        let header_rows = u16::from(first_chunk);
        let height = u16::try_from(chunk.len())
            .unwrap_or(u16::MAX)
            .saturating_add(header_rows);
        let area = rows.take(height);
        if first_chunk {
            widgets::render_table(buf, area, &headers, chunk, &widths);
            first_chunk = false;
        } else {
            widgets::render_table_body(buf, area, chunk, &widths);
        }
    }
    if turns.can_load_more() {
        let row = rows.line();
        if let Some(rect) = render_action_button(
            buf,
            row,
            app,
            hits,
            ActionId::LoadMoreTurns,
            ButtonTone::Secondary,
        ) {
            hits.region(Layer::Base, rect, Intent::Activate(ActionId::LoadMoreTurns));
        }
    }
}

fn relative_optional(value: Option<i64>, now: i64) -> String {
    value.map_or_else(
        || text::DASH.to_owned(),
        |seconds| text::relative_time(seconds, now),
    )
}

fn render_info(buf: &mut Buffer, rows: &mut Rows, app: &App) {
    let detail = app.detail();
    let Some(session) = detail.session() else {
        widgets::render_body(
            buf,
            rows.line(),
            text::DETAIL_NOT_LOADED,
            widgets::dim_style(),
        );
        return;
    };
    rows.skip(1);
    let mut facts: Vec<(&str, String)> = vec![
        (text::LABEL_SESSION_ID, session.id.clone()),
        (text::LABEL_LOGICAL_ID, session.logical_id.clone()),
        (text::LABEL_ROOT_SESSION, session.root_session_id.clone()),
        (text::LABEL_CWD, session.cwd.clone()),
        (
            text::LABEL_MODEL,
            session
                .agent
                .model
                .clone()
                .unwrap_or_else(|| text::VALUE_NONE.to_owned()),
        ),
        (
            text::LABEL_ENVIRONMENT,
            text::environment_value(&session.environment.kind, session.environment.id.as_deref()),
        ),
        (text::LABEL_STATE, session.state.clone()),
        (text::LABEL_STATUS, session.status.clone()),
        (
            text::LABEL_CREATED,
            text::relative_time(session.created_at, chrono::Utc::now().timestamp()),
        ),
        (
            text::LABEL_LAST_ACTIVE,
            text::relative_time(session.last_active_at, chrono::Utc::now().timestamp()),
        ),
        (
            text::LABEL_PROMPT_CACHE,
            session
                .prompt_cache_options
                .as_ref()
                .map(|options| options.ttl.clone())
                .unwrap_or_else(|| text::VALUE_NONE.to_owned()),
        ),
        (
            text::LABEL_VAULT_IDS,
            if session.vault_ids.is_empty() {
                text::VALUE_NONE.to_owned()
            } else {
                session.vault_ids.join(", ")
            },
        ),
        (
            text::LABEL_SANDBOX,
            session
                .sandbox
                .as_ref()
                .map(crate::app::pretty_json)
                .unwrap_or_else(|| text::VALUE_NONE.to_owned()),
        ),
    ];
    for (label, value) in facts.drain(..) {
        let height = widgets::wrapped_height(&value, rows.width().saturating_sub(22).max(1));
        let area = rows.take(height.max(1));
        widgets::render_label_value(buf, area, label, &value, widgets::muted_style());
    }
    render_info_block(buf, rows, text::LABEL_METADATA, &metadata_text(session));
    render_info_block(
        buf,
        rows,
        text::LABEL_INSTRUCTIONS,
        session
            .agent
            .instructions
            .as_deref()
            .unwrap_or(text::VALUE_NONE),
    );
    render_info_block(
        buf,
        rows,
        text::LABEL_USAGE,
        &session
            .usage
            .as_ref()
            .map(crate::app::pretty_json)
            .unwrap_or_else(|| text::VALUE_NONE.to_owned()),
    );
    if let Some(error) = &session.error {
        render_info_block(buf, rows, text::LABEL_ERROR, error);
    }
}

fn metadata_text(session: &crate::api::Session) -> String {
    if session.metadata.is_empty() {
        return text::VALUE_NONE.to_owned();
    }
    session
        .metadata
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_info_block(buf: &mut Buffer, rows: &mut Rows, label: &str, value: &str) {
    rows.skip(1);
    widgets::render_section(buf, rows.line(), label);
    let height = widgets::wrapped_height(value, rows.width());
    widgets::render_body(buf, rows.take(height.max(1)), value, widgets::muted_style());
}

fn render_wrapped(buf: &mut Buffer, rows: &mut Rows, body: &str, style: ratatui::style::Style) {
    let height = widgets::wrapped_height(body, rows.width());
    widgets::render_body(buf, rows.take(height), body, style);
}

#[cfg(test)]
#[path = "session_detail_test.rs"]
mod tests;
