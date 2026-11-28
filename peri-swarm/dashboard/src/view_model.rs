//! Pure presentation logic for the session log.
//!
//! Nothing here touches the terminal or the network: status buckets, counts,
//! search, relative times, truncation and column planning are plain functions
//! over the loaded [`Session`] set, which keeps them cheap to test.
//!
//! All counts and searches operate on the **loaded** set only. The gateway is
//! the source of truth for anything beyond it, so the UI labels the scope
//! instead of claiming global totals.

use peri_tui_components::text::truncate_to_width;
use ratatui::text::Span;

use crate::api::Session;
use crate::ui::text;
use crate::ui::widgets::StatusTone;

/// Width at and above which every column is shown.
pub const WIDTH_WIDE: u16 = 120;
/// Width at and above which the metadata column is shown.
pub const WIDTH_MEDIUM: u16 = 90;
/// Width below which the sidebar becomes a menu button.
pub const WIDTH_COMPACT: u16 = 80;
/// Narrowest pane that still fits the approvals column.
pub const APPROVALS_MIN_PANE: u16 = 44;

/// Status buckets used by the filter chips.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum StatusBucket {
    /// Every loaded session.
    All,
    /// `in_progress`, or a matching durable state.
    InProgress,
    /// `requires_action`, or a matching durable state.
    RequiresAction,
    /// `failed`, or a matching durable state.
    Failed,
    /// `idle`, or any unrecognized state.
    Idle,
}

impl StatusBucket {
    /// Every bucket, in chip order.
    pub const ALL: [StatusBucket; 5] = [
        StatusBucket::All,
        StatusBucket::InProgress,
        StatusBucket::RequiresAction,
        StatusBucket::Failed,
        StatusBucket::Idle,
    ];

    /// English label.
    #[must_use]
    pub fn label(self) -> &'static str {
        text::status_bucket_label(self)
    }

    /// Colour mapping for chips, dots and cells.
    #[must_use]
    pub fn tone(self) -> StatusTone {
        match self {
            StatusBucket::InProgress => StatusTone::Ok,
            StatusBucket::RequiresAction => StatusTone::Warning,
            StatusBucket::Failed => StatusTone::Error,
            StatusBucket::All | StatusBucket::Idle => StatusTone::Muted,
        }
    }
}

/// Bucket for one session. Never returns [`StatusBucket::All`].
#[must_use]
pub fn bucket_of(status: &str, state: &str) -> StatusBucket {
    match status {
        "in_progress" => StatusBucket::InProgress,
        "requires_action" => StatusBucket::RequiresAction,
        "failed" => StatusBucket::Failed,
        "idle" => StatusBucket::Idle,
        // The durable state is the fallback when the status is unknown to this
        // client (for example during `creating`).
        _ => match state {
            "running" | "creating" => StatusBucket::InProgress,
            "requires_action" | "waiting" => StatusBucket::RequiresAction,
            "failed" => StatusBucket::Failed,
            _ => StatusBucket::Idle,
        },
    }
}

/// Bucket for a full session.
#[must_use]
pub fn bucket(session: &Session) -> StatusBucket {
    bucket_of(&session.status, &session.state)
}

/// Counts per bucket over the loaded set.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StatusCounts {
    /// All loaded sessions.
    pub all: usize,
    /// In-progress sessions.
    pub in_progress: usize,
    /// Sessions waiting for an approval or other action.
    pub requires_action: usize,
    /// Failed sessions.
    pub failed: usize,
    /// Idle sessions.
    pub idle: usize,
}

impl StatusCounts {
    /// Count every bucket in one pass.
    #[must_use]
    pub fn of(sessions: &[Session]) -> Self {
        let mut counts = Self::default();
        for session in sessions {
            counts.all += 1;
            match bucket(session) {
                StatusBucket::InProgress => counts.in_progress += 1,
                StatusBucket::RequiresAction => counts.requires_action += 1,
                StatusBucket::Failed => counts.failed += 1,
                StatusBucket::Idle | StatusBucket::All => counts.idle += 1,
            }
        }
        counts
    }

    /// Count for one bucket.
    #[must_use]
    pub fn get(self, bucket: StatusBucket) -> usize {
        match bucket {
            StatusBucket::All => self.all,
            StatusBucket::InProgress => self.in_progress,
            StatusBucket::RequiresAction => self.requires_action,
            StatusBucket::Failed => self.failed,
            StatusBucket::Idle => self.idle,
        }
    }
}

/// Whether a session matches a search query.
///
/// The query is matched case-insensitively against the session id, logical id,
/// model, metadata keys and values, and the environment id.
#[must_use]
pub fn matches_search(session: &Session, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty() || session_matches(session, &query)
}

fn session_matches(session: &Session, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    if contains(&session.id, query) || contains(&session.logical_id, query) {
        return true;
    }
    if let Some(model) = session.agent.model.as_deref() {
        if contains(model, query) {
            return true;
        }
    }
    if let Some(environment) = session.environment.id.as_deref() {
        if contains(environment, query) {
            return true;
        }
    }
    session.metadata.iter().any(|(key, value)| {
        contains(key, query) || contains(value, query) || contains(&format!("{key}={value}"), query)
    })
}

fn contains(haystack: &str, lowercase_query: &str) -> bool {
    haystack.to_lowercase().contains(lowercase_query)
}

/// Indexes of the loaded sessions that pass a bucket filter and a search.
#[must_use]
pub fn filtered_indices(sessions: &[Session], bucket: StatusBucket, query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    sessions
        .iter()
        .enumerate()
        .filter(|(_, session)| {
            (bucket == StatusBucket::All || bucket_of(&session.status, &session.state) == bucket)
                && (query.is_empty() || session_matches(session, &query))
        })
        .map(|(index, _)| index)
        .collect()
}

/// Relative time label for a Unix-seconds timestamp.
///
/// Zero or negative timestamps render as [`text::DASH`]: the gateway uses `0`
/// when it has never reported the value.
#[must_use]
pub fn relative_time(unix_seconds: i64, now_unix: i64) -> String {
    text::relative_time(unix_seconds, now_unix)
}

/// Number of pending MCP approvals on a session.
#[must_use]
pub fn approvals_count(session: &Session) -> usize {
    session
        .required_actions
        .iter()
        .filter(|action| {
            action.get("type").and_then(serde_json::Value::as_str) == Some("mcp_approval_request")
        })
        .count()
}

/// Approvals summary for one row.
#[must_use]
pub fn approvals_summary(count: usize) -> String {
    text::approvals_summary(count)
}

/// First metadata entries as a compact summary, or [`text::DASH`].
#[must_use]
pub fn metadata_summary(session: &Session) -> String {
    if session.metadata.is_empty() {
        return text::DASH.to_owned();
    }
    let mut parts = Vec::with_capacity(3);
    for (index, (key, value)) in session.metadata.iter().enumerate() {
        if index == 2 {
            parts.push(format!("+{}", session.metadata.len() - 2));
            break;
        }
        parts.push(format!("{key}={value}"));
    }
    parts.join(", ")
}

/// Session cell split into the primary id and an optional model suffix.
///
/// The two parts always fit in `width` columns together; the model is dropped
/// before the id drops below a readable size.
#[must_use]
pub fn session_cell(session: &Session, width: u16) -> (String, String) {
    let width = usize::from(width);
    if width == 0 {
        return (String::new(), String::new());
    }
    let id = session.id.as_str();
    let model = session.agent.model.as_deref().unwrap_or("");
    let fit_id = |budget: usize| truncate_to_width(id, budget);
    if model.is_empty() || width < 16 {
        return (fit_id(width), String::new());
    }
    let model_width = display_width(model);
    let id_width = display_width(id);
    if id_width + 1 + model_width <= width {
        return (id.to_owned(), format!(" {model}"));
    }
    let model_budget = model_width.min(width / 2).max(4);
    let id_text = fit_id(width.saturating_sub(model_budget + 1));
    let used = display_width(&id_text);
    (
        id_text,
        format!(
            " {}",
            truncate_to_width(model, width.saturating_sub(used + 1))
        ),
    )
}

/// Truncate to a column budget with the shared ellipsis rule.
#[must_use]
pub fn fit_column(value: &str, width: u16) -> String {
    truncate_to_width(value, usize::from(width))
}

/// One adaptive table column.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Column {
    /// Session id plus model.
    Session,
    /// Status bucket label.
    Status,
    /// Pending approval count.
    Approvals,
    /// Last-active time.
    LastActive,
    /// Metadata summary.
    Metadata,
}

impl Column {
    /// Every column in render order.
    pub const ALL: [Column; 5] = [
        Column::Session,
        Column::Status,
        Column::Approvals,
        Column::LastActive,
        Column::Metadata,
    ];

    /// Column header text.
    #[must_use]
    pub fn header(self) -> &'static str {
        text::column_header(self)
    }

    fn fixed_width(self) -> u16 {
        match self {
            Column::Status => 15,
            Column::Approvals => 9,
            Column::LastActive => 12,
            Column::Session | Column::Metadata => 0,
        }
    }
}

/// Columns visible for a terminal width and the content pane width.
///
/// The shell breakpoints are terminal widths, so both values are needed: the
/// pane width decides whether the optional approvals column still fits.
#[must_use]
pub fn columns(terminal_width: u16, pane_width: u16) -> Vec<Column> {
    if terminal_width >= WIDTH_WIDE {
        return Column::ALL.to_vec();
    }
    if terminal_width >= WIDTH_MEDIUM {
        return vec![
            Column::Session,
            Column::Status,
            Column::Approvals,
            Column::LastActive,
        ];
    }
    let base = vec![Column::Session, Column::Status, Column::LastActive];
    if terminal_width < WIDTH_COMPACT || pane_width < APPROVALS_MIN_PANE {
        return base;
    }
    let mut columns = base;
    columns.insert(2, Column::Approvals);
    columns
}

/// Width of every column, gaps excluded; `available` includes the gaps.
#[must_use]
pub fn column_widths(columns: &[Column], available: u16) -> Vec<u16> {
    if columns.is_empty() {
        return Vec::new();
    }
    let gaps = u16::try_from(columns.len() - 1).unwrap_or(0);
    let usable = available.saturating_sub(gaps);
    let metadata = if columns.contains(&Column::Metadata) {
        (usable / 4).clamp(12, 28)
    } else {
        0
    };
    let fixed: u16 = columns.iter().map(|column| column.fixed_width()).sum();
    let session = usable.saturating_sub(fixed.saturating_add(metadata)).max(6);
    columns
        .iter()
        .map(|column| match column {
            Column::Session => session,
            Column::Metadata => metadata,
            other => other.fixed_width(),
        })
        .collect()
}

fn display_width(value: &str) -> usize {
    Span::raw(value).width()
}

#[cfg(test)]
#[path = "view_model_test.rs"]
mod tests;
