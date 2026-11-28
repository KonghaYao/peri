//! Usage aggregates: one session (`sessions.retrieve`) and the bounded
//! cross-session Agent-metrics window.
//!
//! Both stores persist the turn usage as TEXT JSON owned by
//! [`crate::domain::TurnUsage`]. Aggregation happens here in Rust so the
//! database never parses that shape, and one implementation serves PostgreSQL
//! and the memory store.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::domain::{
    metrics_session, totals_json, AgentMetrics, MetricsWindow, SessionView, SwarmError, TurnUsage,
    UsageTotals,
};
use crate::store::{store_failure, SwarmStore, SwarmTurnUsageRow};

/// Turns one `sessions.retrieve` aggregate reads. A session above this bound
/// reports its newest turns plus `truncated: true` instead of paying an
/// unbounded read on every retrieve.
pub(crate) const MAX_SESSION_USAGE_TURNS: u32 = 10_000;

/// Turn rows one Agent-metrics scan reads for a whole window. Reaching the cap
/// is reported as `truncated: true`; the report never pretends to be complete.
pub(crate) const MAX_METRIC_TURNS: u32 = 50_000;

/// Cumulative usage of one session, or `None` when no turn carries a fact.
pub(crate) async fn session_usage(
    store: &dyn SwarmStore,
    logical_id: &str,
) -> Result<Option<Value>, SwarmError> {
    let probe = MAX_SESSION_USAGE_TURNS.saturating_add(1);
    let mut rows = store
        .swarm_session_usage_turns(logical_id, probe)
        .await
        .map_err(|error| store_failure("session_usage_turns", error))?;
    let truncated = rows.len() > MAX_SESSION_USAGE_TURNS as usize;
    rows.truncate(MAX_SESSION_USAGE_TURNS as usize);
    match aggregate_oldest_last(&rows) {
        Some(totals) => Ok(Some(totals_json(&totals, truncated))),
        None => Ok(None),
    }
}

/// `GET /v1/agents/metrics`: per-session token totals for one window.
pub(crate) async fn agent_metrics(
    store: &dyn SwarmStore,
    window: &MetricsWindow,
) -> Result<AgentMetrics, SwarmError> {
    let (start_at, end_at) = window.second_bounds();
    let probe = MAX_METRIC_TURNS.saturating_add(1);
    let mut rows = store
        .swarm_turn_usage_window(start_at, end_at, probe)
        .await
        .map_err(|error| store_failure("turn_usage_window", error))?;
    let truncated = rows.len() > MAX_METRIC_TURNS as usize;
    rows.truncate(MAX_METRIC_TURNS as usize);

    // Rows arrive newest first; the per-session list keeps that order so each
    // session's newest context/model can win when it is folded in reverse.
    let mut grouped: BTreeMap<String, SessionRows> = BTreeMap::new();
    for row in &rows {
        let entry = grouped
            .entry(row.logical_id.clone())
            .or_insert_with(|| SessionRows {
                model: row.model.clone(),
                last_active_at: row.last_active_at.clone(),
                usages: Vec::new(),
            });
        if let Some(usage) = decode(row) {
            entry.usages.push(usage);
        }
    }

    let mut totals = UsageTotals::default();
    let mut sessions = Vec::with_capacity(grouped.len());
    for (session_id, group) in grouped {
        let mut session_totals = UsageTotals::default();
        for usage in group.usages.iter().rev() {
            session_totals.absorb(usage);
        }
        if session_totals.is_empty() {
            // Turns without any usage fact are not reportable: showing zero
            // counters would claim a measurement the gateway never had.
            continue;
        }
        totals.merge_counters(&session_totals);
        sessions.push(metrics_session(
            &session_id,
            &session_totals,
            group.model,
            &group.last_active_at,
        ));
    }
    sessions.sort_by(|left, right| {
        right
            .total_tokens
            .unwrap_or(0)
            .cmp(&left.total_tokens.unwrap_or(0))
            .then_with(|| right.last_active_at.cmp(&left.last_active_at))
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    sessions.truncate(window.limit as usize);

    Ok(AgentMetrics {
        start_time: window.start_time,
        end_time: window.end_time,
        sessions,
        totals,
        truncated,
    })
}

struct SessionRows {
    /// `swarm_sessions.model`, used when no usage observation named a model.
    model: Option<String>,
    last_active_at: String,
    /// Newest first, matching the store order.
    usages: Vec<TurnUsage>,
}

/// Attach the session's persisted turn aggregate to a retrieve projection.
///
/// `sessions.list` keeps `usage: null` on purpose: aggregating every listed
/// session would add one turn scan per row. Only retrieve/create/update report
/// the aggregate.
pub(crate) async fn with_session_usage(
    store: &dyn SwarmStore,
    mut view: SessionView,
) -> Result<SessionView, SwarmError> {
    view.usage = session_usage(store, &view.logical_id).await?;
    Ok(view)
}

/// Fold usage rows in chronological order: the first slice element is the
/// newest, so context/model keep the latest observation.
fn aggregate_oldest_last(rows: &[SwarmTurnUsageRow]) -> Option<UsageTotals> {
    let mut totals = UsageTotals::default();
    for row in rows.iter().rev() {
        if let Some(usage) = decode(row) {
            totals.absorb(&usage);
        }
    }
    (!totals.is_empty()).then_some(totals)
}

fn decode(row: &SwarmTurnUsageRow) -> Option<TurnUsage> {
    let value = row.usage.as_ref()?;
    match TurnUsage::from_json(value) {
        Some(usage) => Some(usage),
        None => {
            tracing::warn!(
                logical_id = %row.logical_id,
                turn_id = %row.turn_id,
                "turn usage_json did not decode; the aggregate skips it"
            );
            None
        }
    }
}
