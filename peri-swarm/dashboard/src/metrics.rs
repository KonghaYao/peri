//! Core-metrics and request-log queries.
//!
//! OpenObserve stores OTLP metrics in Prometheus-compatible streams and accepts
//! **PromQL only** for them (`_search` answers "Search stream not found" for a
//! metrics stream). Telemetry logs are ordinary log streams and are queried with
//! SQL. Both paths are read-only; the SQL is built here from fixed column names
//! and numeric window bounds only.
//!
//! The metric counters are cumulative, but this OpenObserve build does not
//! implement `rate()` or `increase()` (both return an empty matrix). The
//! dashboard therefore reads a small range of raw cumulative samples and sums
//! the positive increments client-side. That keeps counter resets from
//! producing negative traffic and reports the actual samples the window
//! covered.

use serde_json::Value;

use crate::observability::{ObservabilityError, OpenObserveClient, PromSample};

/// Sessions per Agent-metrics display page.
pub const AGENT_METRICS_PAGE_SIZE: usize = 10;
/// Sessions requested from the gateway for the Agent-metrics window.
pub const AGENT_METRICS_LIMIT: u32 = 500;
/// Rows requested for one request-log display page.
pub const REQUEST_LOG_PAGE_SIZE: usize = 10;
/// One extra row detects whether a following request-log page exists.
const REQUEST_LOG_FETCH: u64 = REQUEST_LOG_PAGE_SIZE as u64 + 1;
/// Fixed log stream produced by the deploy stack's OTLP logs header.
pub const REQUEST_LOG_STREAM: &str = "peri_swarm_logs";

/// PromQL stream names after OTel's dot-to-underscore conversion.
const REQUESTS_QUERY: &str = "sum(peri_swarm_http_server_requests)";
const ERRORS_QUERY: &str = "sum(peri_swarm_http_server_requests{status=~\"5..\"})";
const DURATION_QUERY: &str = "sum by (le) (peri_swarm_http_server_duration_bucket)";
/// 95th percentile used by Core metrics.
const P95_QUANTILE: f64 = 0.95;

/// Time window offered by the metrics pages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeWindow {
    /// Last 15 minutes.
    FifteenMinutes,
    /// Last hour.
    OneHour,
    /// Last 24 hours.
    TwentyFourHours,
}

impl TimeWindow {
    /// Every window in toolbar order.
    pub const ALL: [TimeWindow; 3] = [
        TimeWindow::FifteenMinutes,
        TimeWindow::OneHour,
        TimeWindow::TwentyFourHours,
    ];

    /// Window length in seconds.
    #[must_use]
    pub const fn seconds(self) -> u64 {
        match self {
            TimeWindow::FifteenMinutes => 15 * 60,
            TimeWindow::OneHour => 60 * 60,
            TimeWindow::TwentyFourHours => 24 * 60 * 60,
        }
    }

    /// Window length in Unix microseconds.
    #[must_use]
    pub const fn micros(self) -> u64 {
        self.seconds().saturating_mul(1_000_000)
    }

    /// PromQL counter step and duration-histogram step for this window.
    ///
    /// The counter step stays at one minute even for 24 hours so requests per
    /// minute remains fresh; the bucket step is coarser there because p95 over
    /// a day does not need one-minute resolution.
    const fn prom_steps(self) -> (u64, u64) {
        match self {
            TimeWindow::FifteenMinutes => (15, 15),
            TimeWindow::OneHour => (60, 60),
            TimeWindow::TwentyFourHours => (60, 300),
        }
    }
}

/// One Core-metrics snapshot. Every value is optional: a missing metric stays
/// `None` and is rendered as an em dash instead of a fabricated zero.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CoreMetricsSnapshot {
    /// Requests per minute over the covered samples.
    pub requests_per_minute: Option<f64>,
    /// 5xx responses divided by all requests over the covered samples.
    pub error_rate: Option<f64>,
    /// Estimated 95th percentile request duration in seconds.
    pub p95_seconds: Option<f64>,
    /// Requests observed over the covered samples.
    pub total_requests: Option<u64>,
    /// Requested window start, Unix microseconds.
    pub window_start_us: u64,
    /// Requested window end, Unix microseconds.
    pub window_end_us: u64,
    /// First metric sample that contributed to the values.
    pub covered_start_us: Option<u64>,
    /// Last metric sample that contributed to the values.
    pub covered_end_us: Option<u64>,
}

impl CoreMetricsSnapshot {
    /// Whether no metric samples exist in the window.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.covered_start_us.is_none() && self.covered_end_us.is_none()
    }
}

/// One request-log row. Fields the access log did not report stay `None`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestLogRow {
    /// Row timestamp, Unix microseconds.
    pub timestamp_us: Option<u64>,
    /// HTTP method.
    pub method: Option<String>,
    /// Route template.
    pub route: Option<String>,
    /// HTTP status code.
    pub status: Option<u16>,
    /// Server-side duration in milliseconds.
    pub duration_ms: Option<u64>,
    /// Session id when the route carries one.
    pub session_id: Option<String>,
}

/// One request-log page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestLogPage {
    /// Rows in newest-first order; at most [`REQUEST_LOG_PAGE_SIZE`].
    pub rows: Vec<RequestLogRow>,
    /// Requested page, 0-based.
    pub page: usize,
    /// Whether at least one older row exists.
    pub has_next: bool,
    /// Requested window start, Unix microseconds.
    pub window_start_us: u64,
    /// Requested window end, Unix microseconds.
    pub window_end_us: u64,
}

/// Counter change over one series.
#[derive(Clone, Copy, Debug, Default)]
struct Delta {
    total: f64,
    first: Option<i64>,
    last: Option<i64>,
    samples: usize,
}

/// Sum positive increments so a process restart cannot produce negative traffic.
fn series_delta(values: &[PromSample]) -> Delta {
    let mut points: Vec<(i64, f64)> = values
        .iter()
        .filter(|sample| sample.value.is_finite())
        .map(|sample| (sample.timestamp, sample.value))
        .collect();
    points.sort_by_key(|point| point.0);
    points.dedup_by_key(|point| point.0);

    let mut delta = Delta {
        first: points.first().map(|point| point.0),
        last: points.last().map(|point| point.0),
        samples: points.len(),
        total: 0.0,
    };
    for pair in points.windows(2) {
        delta.total += (pair[1].1 - pair[0].1).max(0.0);
    }
    delta
}

/// Aggregate the deltas of every series that matches one selector.
fn aggregate_delta(series: &[crate::observability::PromSeries]) -> Delta {
    let mut total = Delta::default();
    for series in series {
        let delta = series_delta(&series.values);
        total.total += delta.total;
        total.samples = total.samples.saturating_add(delta.samples);
        total.first = match (total.first, delta.first) {
            (Some(current), Some(next)) => Some(current.min(next)),
            (None, next) => next,
            (current, None) => current,
        };
        total.last = match (total.last, delta.last) {
            (Some(current), Some(next)) => Some(current.max(next)),
            (None, next) => next,
            (current, None) => current,
        };
    }
    total
}

/// Parse an `le` bucket boundary; `inf` is OpenObserve's last bucket.
fn parse_bound(label: &str) -> Option<f64> {
    if label.eq_ignore_ascii_case("inf") || label == "+Inf" {
        return Some(f64::INFINITY);
    }
    label.parse::<f64>().ok()
}

/// Cumulative bucket counts for the window, sorted by upper bound.
fn bucket_counts(series: &[crate::observability::PromSeries]) -> Vec<(f64, f64)> {
    let mut buckets: Vec<(f64, f64)> = Vec::new();
    for series in series {
        let Some(bound) = series.labels.get("le").and_then(|label| parse_bound(label)) else {
            continue;
        };
        let delta = series_delta(&series.values).total;
        match buckets.iter_mut().find(|entry| entry.0 == bound) {
            Some(entry) => entry.1 += delta,
            None => buckets.push((bound, delta)),
        }
    }
    buckets.sort_by(|left, right| left.0.total_cmp(&right.0));
    // Each `le` series is already cumulative over requests, so its change over
    // the window is that bucket's count. Do not sum across bounds again.
    buckets
}

/// Prometheus-style histogram quantile over cumulative bucket counts.
///
/// `None` when the window has no observations or when the quantile falls into
/// the open `+Inf` bucket: the dashboard does not invent a finite latency for
/// an upper-bounded-unknown tail.
fn histogram_quantile(quantile: f64, buckets: &[(f64, f64)]) -> Option<f64> {
    let total = buckets.last()?.1;
    if !total.is_finite() || total <= 0.0 {
        return None;
    }
    let rank = quantile * total;
    let mut previous_bound = 0.0;
    let mut previous_count = 0.0;
    for (bound, count) in buckets {
        if *count >= rank {
            if !bound.is_finite() {
                return None;
            }
            let bucket_count = count - previous_count;
            if bucket_count <= 0.0 {
                return Some(*bound);
            }
            let fraction = (rank - previous_count) / bucket_count;
            return Some(previous_bound + (bound - previous_bound) * fraction);
        }
        previous_bound = *bound;
        previous_count = *count;
    }
    None
}

/// Build a snapshot from the three PromQL matrices.
fn build_snapshot(
    requests: &[crate::observability::PromSeries],
    errors: &[crate::observability::PromSeries],
    buckets: &[crate::observability::PromSeries],
    window_start_us: u64,
    window_end_us: u64,
) -> CoreMetricsSnapshot {
    let requests = aggregate_delta(requests);
    let errors = aggregate_delta(errors);
    let covered = match (requests.first, requests.last) {
        (Some(first), Some(last)) if first < last && requests.samples >= 2 => Some((first, last)),
        _ => None,
    };
    let total_requests = covered.map(|(first, last)| {
        let minutes = (last - first) as f64 / 60.0;
        (requests.total, minutes)
    });
    let mut snapshot = CoreMetricsSnapshot {
        window_start_us,
        window_end_us,
        ..CoreMetricsSnapshot::default()
    };
    if let Some((first, last)) = covered {
        snapshot.covered_start_us = Some(first.saturating_mul(1_000_000).max(0) as u64);
        snapshot.covered_end_us = Some(last.saturating_mul(1_000_000).max(0) as u64);
    }
    if let Some((total, minutes)) = total_requests {
        let total = total.round().max(0.0);
        snapshot.total_requests = Some(total as u64);
        if minutes > 0.0 {
            snapshot.requests_per_minute = Some(total / minutes);
        }
        if total > 0.0 {
            // No error series means no 5xx counter was exported. A single
            // error sample cannot say whether the error happened inside the
            // window, so the rate stays unknown instead of becoming 0%.
            if errors.samples == 0 {
                snapshot.error_rate = Some(0.0);
            } else if errors.samples >= 2 {
                snapshot.error_rate = Some((errors.total / total).clamp(0.0, 1.0));
            }
        }
    }
    snapshot.p95_seconds = histogram_quantile(P95_QUANTILE, &bucket_counts(buckets));
    snapshot
}

/// Load one Core-metrics snapshot from OpenObserve.
pub(crate) async fn load_core_metrics(
    client: &OpenObserveClient,
    window: TimeWindow,
    now_us: u64,
) -> Result<CoreMetricsSnapshot, ObservabilityError> {
    let window_start_us = now_us.saturating_sub(window.micros());
    let start_s = window_start_us / 1_000_000;
    let end_s = now_us / 1_000_000;
    let (counter_step, bucket_step) = window.prom_steps();

    let requests = client
        .query_range(
            REQUESTS_QUERY,
            start_s.saturating_sub(counter_step),
            end_s,
            counter_step,
        )
        .await?;
    let errors = client
        .query_range(
            ERRORS_QUERY,
            start_s.saturating_sub(counter_step),
            end_s,
            counter_step,
        )
        .await?;
    let buckets = client
        .query_range(
            DURATION_QUERY,
            start_s.saturating_sub(bucket_step),
            end_s,
            bucket_step,
        )
        .await?;

    Ok(build_snapshot(
        &requests.series,
        &errors.series,
        &buckets.series,
        window_start_us,
        now_us,
    ))
}

/// SQL for one request-log page.
///
/// The time bounds are repeated inside the SQL as well as in the `_search`
/// request so the statement is self-describing; both are numeric values built
/// here, never user input.
fn request_log_sql(start_us: u64, end_us: u64, page: usize) -> String {
    let offset = page.saturating_mul(REQUEST_LOG_PAGE_SIZE);
    format!(
        "SELECT _timestamp, method, route, status, duration_ms, session_id FROM \"{REQUEST_LOG_STREAM}\" \
         WHERE _timestamp >= {start_us} AND _timestamp < {end_us} \
         ORDER BY _timestamp DESC LIMIT {REQUEST_LOG_FETCH} OFFSET {offset}"
    )
}

fn value_u64(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

fn value_text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|text| !text.is_empty())
}

fn parse_row(value: &Value) -> RequestLogRow {
    RequestLogRow {
        timestamp_us: value_u64(value.get("_timestamp")),
        method: value_text(value.get("method")),
        route: value_text(value.get("route")),
        status: value_u64(value.get("status")).and_then(|status| u16::try_from(status).ok()),
        duration_ms: value_u64(value.get("duration_ms")),
        session_id: value_text(value.get("session_id")),
    }
}

/// Load one request-log page from OpenObserve.
pub(crate) async fn load_request_log(
    client: &OpenObserveClient,
    window: TimeWindow,
    page: usize,
    now_us: u64,
) -> Result<RequestLogPage, ObservabilityError> {
    let window_start_us = now_us.saturating_sub(window.micros());
    let sql = request_log_sql(window_start_us, now_us, page);
    let found = client
        .search(&sql, window_start_us, now_us, 0, REQUEST_LOG_FETCH)
        .await?;
    let has_next = found.hits.len() > REQUEST_LOG_PAGE_SIZE;
    let rows: Vec<RequestLogRow> = found
        .hits
        .iter()
        .take(REQUEST_LOG_PAGE_SIZE)
        .map(parse_row)
        .collect();
    Ok(RequestLogPage {
        rows,
        page,
        has_next,
        window_start_us,
        window_end_us: now_us,
    })
}

#[cfg(test)]
#[path = "metrics_test.rs"]
mod tests;
