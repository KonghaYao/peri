//! Core-metrics, request-log and Query page wording.
//!
//! These pages all present OpenObserve-backed data. Kept in a submodule so the
//! central text module stays inside the repository size budget; every string is
//! still reachable as `text::…` through the re-export in [`super`].

use chrono::{DateTime, Local};
use serde_json::Value;

use crate::metrics::TimeWindow;
use crate::query::SqlError;

use super::{relative_time, DASH};

// ---------------------------------------------------------------------------
// Metrics pages
// ---------------------------------------------------------------------------

/// Core-metrics page title.
pub const CORE_METRICS_TITLE: &str = "Core metrics";

/// Agent-metrics page title.
pub const AGENT_METRICS_TITLE: &str = "Agent metrics";

/// Request-log page title.
pub const REQUEST_LOG_TITLE: &str = "Request log";

/// Short window label used by the toolbar.
#[must_use]
pub fn window_label(window: TimeWindow) -> &'static str {
    match window {
        TimeWindow::FifteenMinutes => "15m",
        TimeWindow::OneHour => "1h",
        TimeWindow::TwentyFourHours => "24h",
    }
}

/// Scope note for Core metrics.
#[must_use]
pub fn core_metrics_scope(window: TimeWindow, stale: bool) -> String {
    let mut note = format!(
        "Window {} · source OpenObserve metrics",
        window_label(window)
    );
    if stale {
        note.push_str(" · stale");
    }
    note
}

/// Scope note for Agent metrics.
#[must_use]
pub fn agent_metrics_scope(
    window: TimeWindow,
    loaded: usize,
    truncated: bool,
    stale: bool,
) -> String {
    let mut note = format!(
        "Window {} · source gateway agent metrics · {}",
        window_label(window),
        sessions_label(loaded)
    );
    if truncated {
        note.push_str(" · truncated");
    }
    if stale {
        note.push_str(" · stale");
    }
    note
}

/// Scope note for the request log.
#[must_use]
pub fn request_log_scope(window: TimeWindow, page: usize, stale: bool) -> String {
    let mut note = format!(
        "Window {} · source OpenObserve logs · page {}",
        window_label(window),
        page.saturating_add(1)
    );
    if stale {
        note.push_str(" · stale");
    }
    note
}

/// Requests-per-minute value.
#[must_use]
pub fn requests_per_minute_value(value: Option<f64>) -> String {
    match value.filter(|value| value.is_finite()) {
        Some(value) => format!("{value:.1}"),
        None => DASH.to_owned(),
    }
}

/// Error-rate value as a percentage.
#[must_use]
pub fn error_rate_value(value: Option<f64>) -> String {
    match value.filter(|value| value.is_finite()) {
        Some(value) => format!("{:.1}%", value * 100.0),
        None => DASH.to_owned(),
    }
}

/// p95 latency value in milliseconds.
#[must_use]
pub fn p95_value(seconds: Option<f64>) -> String {
    match seconds.filter(|value| value.is_finite()) {
        Some(seconds) => format!("{:.0} ms", seconds * 1000.0),
        None => DASH.to_owned(),
    }
}

/// Integer total with an em dash for an unreported value.
#[must_use]
pub fn total_value(value: Option<u64>) -> String {
    value.map_or_else(|| DASH.to_owned(), |value| value.to_string())
}

/// Actual sample range covered by a Core-metrics snapshot.
#[must_use]
pub fn data_range(start_us: Option<u64>, end_us: Option<u64>) -> String {
    match (start_us, end_us) {
        (Some(start), Some(end)) => format!("{} – {}", clock(start), clock(end)),
        _ => DASH.to_owned(),
    }
}

/// Core-metrics labels.
pub const LABEL_REQUESTS_PER_MINUTE: &str = "Requests / min";
/// 5xx error-rate label.
pub const LABEL_ERROR_RATE: &str = "Error rate (5xx)";
/// p95 latency label.
pub const LABEL_P95_LATENCY: &str = "p95 latency";
/// Total request label.
pub const LABEL_TOTAL_REQUESTS: &str = "Total requests";
/// Data-range label.
pub const LABEL_DATA_RANGE: &str = "Data range";

/// OpenObserve is not configured.
pub const OBSERVABILITY_NOT_CONFIGURED_MESSAGE: &str = "Observability is not configured.";

/// How to configure OpenObserve.
pub const OBSERVABILITY_NOT_CONFIGURED_HINT: &str =
    "Add an OpenObserve connection in Settings to load this page.";

/// Initial Core-metrics load.
pub const LOADING_CORE_METRICS: &str = "Loading core metrics…";

/// Initial Agent-metrics load.
pub const LOADING_AGENT_METRICS: &str = "Loading agent metrics…";

/// Initial request-log load.
pub const LOADING_REQUEST_LOG: &str = "Loading requests…";

/// No metric samples in the window.
pub const EMPTY_CORE_METRICS: &str = "No metric data in this window.";

/// Empty Core-metrics explanation.
pub const EMPTY_CORE_METRICS_HINT: &str =
    "OpenObserve reported no samples for the selected window.";

/// No usage in the window.
pub const EMPTY_AGENT_METRICS: &str = "No usage reported in this window.";

/// Empty Agent-metrics explanation.
pub const EMPTY_AGENT_METRICS_HINT: &str =
    "Only sessions with at least one usage-carrying turn appear here.";

/// No access-log rows in the window.
pub const EMPTY_REQUEST_LOG: &str = "No requests in this window.";

/// Empty request-log explanation.
pub const EMPTY_REQUEST_LOG_HINT: &str = "The access log has no entries for the selected window.";

/// Core-metrics query failure.
#[must_use]
pub fn core_metrics_error(detail: &str) -> String {
    format!("Could not load core metrics: {detail}")
}

/// Agent-metrics query failure.
#[must_use]
pub fn agent_metrics_error(detail: &str) -> String {
    format!("Could not load agent metrics: {detail}")
}

/// Request-log query failure.
#[must_use]
pub fn request_log_error(detail: &str) -> String {
    format!("Could not load requests: {detail}")
}

/// Visible marker when a failed refresh kept the previous data on screen.
pub const STALE_DATA: &str = "Showing the last successful data; the latest refresh failed.";

/// Agent table headers.
pub const AGENT_COL_SESSION: &str = "Session";
/// Model column.
pub const AGENT_COL_MODEL: &str = "Model";
/// Input token column.
pub const AGENT_COL_INPUT: &str = "Input";
/// Output token column.
pub const AGENT_COL_OUTPUT: &str = "Output";
/// Combined cache token column.
pub const AGENT_COL_CACHE: &str = "Cache";
/// Total token column.
pub const AGENT_COL_TOTAL: &str = "Total";
/// Turn count column.
pub const AGENT_COL_TURNS: &str = "Turns";
/// Last activity column.
pub const AGENT_COL_LAST_ACTIVE: &str = "Last active";

/// Exact truncation notice required by the gateway contract.
pub const AGENT_METRICS_TRUNCATED: &str = "Window truncated; totals cover the newest 500 sessions.";

/// `1 session` / `N sessions`.
#[must_use]
pub fn sessions_label(count: usize) -> String {
    match count {
        1 => "1 session".to_owned(),
        count => format!("{count} sessions"),
    }
}

/// Totals line built from the gateway `totals` object.
#[must_use]
pub fn agent_totals_line(totals: &crate::api::AgentMetricsTotals, sessions: usize) -> String {
    format!(
        "Totals · {} · input {} · output {} · cache {} · total {} · turns {} · calls {} · model {}",
        sessions_label(sessions),
        token_value(totals.input_tokens),
        token_value(totals.output_tokens),
        cache_value(totals.cache_read_tokens, totals.cache_creation_tokens),
        token_value(totals.total_tokens),
        totals.turns,
        totals.calls,
        model_value(totals.model.as_deref()),
    )
}

/// Token count with an em dash for an unreported value.
#[must_use]
pub fn token_value(value: Option<u64>) -> String {
    value.map_or_else(|| DASH.to_owned(), |value| value.to_string())
}

/// Combined cache tokens: read + creation. Both missing renders an em dash.
#[must_use]
pub fn cache_value(read: Option<u64>, creation: Option<u64>) -> String {
    match (read, creation) {
        (None, None) => DASH.to_owned(),
        (Some(read), Some(creation)) => read.saturating_add(creation).to_string(),
        (Some(value), None) | (None, Some(value)) => value.to_string(),
    }
}

/// Model name or an em dash.
#[must_use]
pub fn model_value(model: Option<&str>) -> String {
    model
        .filter(|model| !model.is_empty())
        .map_or_else(|| DASH.to_owned(), str::to_owned)
}

/// Relative last-activity label for an RFC3339 gateway timestamp.
#[must_use]
pub fn agent_last_active(value: &str, now_unix: i64) -> String {
    if value.is_empty() {
        return DASH.to_owned();
    }
    match DateTime::parse_from_rfc3339(value) {
        Ok(timestamp) => relative_time(timestamp.timestamp(), now_unix),
        // The value is gateway-provided display data, not a secret; show it
        // unchanged when this client cannot parse it.
        Err(_) => value.to_owned(),
    }
}

/// Request-log table headers.
pub const REQUEST_COL_TIME: &str = "Time";
/// HTTP method column.
pub const REQUEST_COL_METHOD: &str = "Method";
/// Route column.
pub const REQUEST_COL_ROUTE: &str = "Route";
/// Status column.
pub const REQUEST_COL_STATUS: &str = "Status";
/// Duration column.
pub const REQUEST_COL_DURATION: &str = "Duration";
/// Session column.
pub const REQUEST_COL_SESSION: &str = "Session";

/// Request timestamp in local time.
#[must_use]
pub fn request_time(timestamp_us: Option<u64>) -> String {
    timestamp_us.map_or_else(|| DASH.to_owned(), clock)
}

/// Local `hh:mm:ss` label for narrow request-log tables.
#[must_use]
pub fn request_time_short(timestamp_us: Option<u64>) -> String {
    let Some(timestamp_us) = timestamp_us else {
        return DASH.to_owned();
    };
    let Ok(timestamp) = i64::try_from(timestamp_us) else {
        return DASH.to_owned();
    };
    DateTime::from_timestamp_micros(timestamp).map_or_else(
        || DASH.to_owned(),
        |timestamp| {
            timestamp
                .with_timezone(&Local)
                .format("%H:%M:%S")
                .to_string()
        },
    )
}

/// HTTP status or an em dash.
#[must_use]
pub fn request_status(status: Option<u16>) -> String {
    status.map_or_else(|| DASH.to_owned(), |status| status.to_string())
}

/// Request duration in milliseconds.
#[must_use]
pub fn request_duration(duration_ms: Option<u64>) -> String {
    duration_ms.map_or_else(|| DASH.to_owned(), |duration| format!("{duration} ms"))
}

/// Optional request-log text (method, route, session id).
#[must_use]
pub fn request_optional(value: Option<&str>) -> String {
    value
        .filter(|value| !value.is_empty())
        .map_or_else(|| DASH.to_owned(), str::to_owned)
}

/// `Page 1 / 3` for client-side paging.
#[must_use]
pub fn page_indicator(page: usize, page_count: usize) -> String {
    format!("Page {} / {}", page.saturating_add(1), page_count.max(1))
}

/// `Page 1` for server-side paging without a known total.
#[must_use]
pub fn request_page_indicator(page: usize) -> String {
    format!("Page {}", page.saturating_add(1))
}

/// Local `mm-dd hh:mm:ss` label for a Unix-microsecond timestamp.
fn clock(timestamp_us: u64) -> String {
    let Ok(timestamp) = i64::try_from(timestamp_us) else {
        return DASH.to_owned();
    };
    DateTime::from_timestamp_micros(timestamp).map_or_else(
        || DASH.to_owned(),
        |timestamp| {
            timestamp
                .with_timezone(&Local)
                .format("%m-%d %H:%M:%S")
                .to_string()
        },
    )
}

// ---------------------------------------------------------------------------
// Query page
// ---------------------------------------------------------------------------

/// Query page title.
pub const QUERY_TITLE: &str = "Query";

/// SQL editor label.
pub const QUERY_SQL_LABEL: &str = "SQL";

/// Empty-editor placeholder. Deliberately an instruction, not a runnable
/// statement, so it cannot suggest a `LIMIT` the Run button would reject.
pub const QUERY_SQL_PLACEHOLDER: &str = "Enter a SELECT statement (Ctrl-Enter runs)";

/// Editor hint, shown under the text area.
pub const QUERY_HINT: &str = "Read-only SELECT · Ctrl-Enter runs the query";

/// Shown when the executed SQL has its own top-level LIMIT/OFFSET.
///
/// OpenObserve ignores the request offset in that case, so the pager is
/// disabled rather than silently repeating the first page.
pub const QUERY_MANUAL_LIMIT: &str =
    "SQL has LIMIT/OFFSET: server-side paging is off. Remove them to page through all rows.";

/// Loading label while a query is running.
pub const QUERY_RUNNING: &str = "Running…";

/// A completed query matched no rows.
pub const QUERY_EMPTY: &str = "No rows returned.";

/// Empty-result explanation.
pub const QUERY_EMPTY_HINT: &str = "The statement completed and matched no rows in this window.";

/// Longest cell value shown in the result table, before the ellipsis.
pub const QUERY_CELL_MAX_CHARS: usize = 96;

/// Scope note for the Query page.
#[must_use]
pub fn query_scope(window: TimeWindow, page: usize, manual_limit: bool) -> String {
    let position = if manual_limit {
        "SQL limit".to_owned()
    } else {
        format!("page {}", page.saturating_add(1))
    };
    format!(
        "Window {} · source OpenObserve logs and traces · {position} · manual run",
        window_label(window)
    )
}

/// Query failure.
#[must_use]
pub fn query_error(detail: &str) -> String {
    format!("Could not run the query: {detail}")
}

/// Local SQL validation message. The statement is never echoed.
#[must_use]
pub fn sql_error(error: SqlError) -> &'static str {
    match error {
        SqlError::Empty => "Enter a SELECT statement.",
        SqlError::TooLarge => "Query is too large (limit 8 KiB).",
        SqlError::NotSelect => "Query must be a single SELECT statement.",
        SqlError::Semicolon => "Only a single trailing semicolon is allowed.",
        SqlError::Nul => "Query must not contain NUL characters.",
    }
}

/// Notice shown when the result has more columns than fit on screen.
#[must_use]
pub fn query_columns_truncated(shown: usize, total: usize) -> String {
    format!("Showing {shown} of {total} columns.")
}

/// One result cell.
///
/// `null` and a missing key render as an em dash; nested values become compact
/// JSON; control characters become spaces so one cell cannot break the table
/// row; very long values are cut at [`QUERY_CELL_MAX_CHARS`] with an ellipsis.
#[must_use]
pub fn query_cell(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return DASH.to_owned();
    };
    let text = match value {
        Value::Null => return DASH.to_owned(),
        Value::String(text) => text.clone(),
        Value::Bool(_) | Value::Number(_) => value.to_string(),
        Value::Array(_) | Value::Object(_) => {
            serde_json::to_string(value).unwrap_or_else(|_| DASH.to_owned())
        }
    };
    let cleaned: String = text
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    if cleaned.chars().count() <= QUERY_CELL_MAX_CHARS {
        return cleaned;
    }
    let mut truncated: String = cleaned.chars().take(QUERY_CELL_MAX_CHARS).collect();
    truncated.push('…');
    truncated
}
