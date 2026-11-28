//! Token accounting: one turn's persisted usage and aggregates over turns.
//!
//! The gateway is the only writer of `swarm_turns.usage_json`, so this module
//! owns its wire shape (stable, `snake_case`):
//!
//! ```json
//! {
//!   "input_tokens": 0, "output_tokens": 0,
//!   "cache_read_tokens": 0, "cache_creation_tokens": 0,
//!   "total_tokens": 0, "context_used": 0, "context_size": 0,
//!   "model": "…", "request_id": "…", "calls": 0
//! }
//! ```
//!
//! The counters use the TUI's accounting rule (`peri-tui/src/cli_print.rs`):
//! `input_tokens` excludes both cache buckets because the provider's
//! `inputTokens` already contains them, and `total_tokens` is the sum of the
//! four counters. Every host `usage_update` is one LLM call, so `calls` counts
//! observations.
//!
//! A host that never negotiated `token_stats` only sends context occupancy
//! (`used`/`size`). Such a turn persists `context_used`/`context_size` and no
//! token counter: writing `0` would claim "no tokens were spent", which the
//! gateway cannot know.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One normalized `usage_update` observation from the host.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageSample {
    /// Standard `usage_update.used` (context occupancy of the latest prompt).
    pub context_used: Option<u64>,
    /// Standard `usage_update.size` (context window).
    pub context_size: Option<u64>,
    /// Token breakdown from the update `_meta`; `None` when the host sent no
    /// `inputTokens`/`outputTokens` pair.
    pub tokens: Option<TokenSample>,
    /// Host `stopReason` from the update `_meta`. Parsed for the projection
    /// contract but not persisted: the turn terminal already carries it.
    pub stop_reason: Option<String>,
}

/// One call's token counters, already normalized to the gateway's unit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenSample {
    /// Prompt tokens excluding both cache buckets (saturating subtraction).
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub model: Option<String>,
    pub request_id: Option<String>,
}

impl TokenSample {
    /// The one-total rule shared with the persisted `total_tokens`.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_creation_tokens)
    }
}

/// The persisted `Turn.usage` object (cumulative for one turn).
///
/// Token counters stay `None` until a `_meta` observation arrives. `context_*`
/// and `model` hold the latest observation; `calls` counts token observations.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TurnUsage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_used: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calls: Option<u64>,
}

impl TurnUsage {
    /// Fold one observation into the cumulative turn usage.
    ///
    /// Context occupancy, model and request id keep the latest value; token
    /// counters and `calls` add up.
    pub fn observe(&mut self, sample: &UsageSample) {
        if let Some(used) = sample.context_used {
            self.context_used = Some(used);
        }
        if let Some(size) = sample.context_size {
            self.context_size = Some(size);
        }
        let Some(tokens) = &sample.tokens else {
            return;
        };
        add(&mut self.input_tokens, tokens.input_tokens);
        add(&mut self.output_tokens, tokens.output_tokens);
        add(&mut self.cache_read_tokens, tokens.cache_read_tokens);
        add(
            &mut self.cache_creation_tokens,
            tokens.cache_creation_tokens,
        );
        self.total_tokens = Some(
            self.input_tokens
                .unwrap_or(0)
                .saturating_add(self.output_tokens.unwrap_or(0))
                .saturating_add(self.cache_read_tokens.unwrap_or(0))
                .saturating_add(self.cache_creation_tokens.unwrap_or(0)),
        );
        add(&mut self.calls, 1);
        if let Some(model) = &tokens.model {
            self.model = Some(model.clone());
        }
        if let Some(request_id) = &tokens.request_id {
            self.request_id = Some(request_id.clone());
        }
    }

    /// Whether no observation produced any fact (nothing to persist).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.input_tokens.is_none()
            && self.output_tokens.is_none()
            && self.cache_read_tokens.is_none()
            && self.cache_creation_tokens.is_none()
            && self.total_tokens.is_none()
            && self.context_used.is_none()
            && self.context_size.is_none()
            && self.model.is_none()
            && self.request_id.is_none()
            && self.calls.is_none()
    }

    /// The persisted JSON, or `None` when the turn has no usage fact yet.
    #[must_use]
    pub fn to_json(&self) -> Option<Value> {
        if self.is_empty() {
            return None;
        }
        // Plain scalars only: serialization cannot fail.
        serde_json::to_value(self).ok()
    }

    /// Decode a persisted `usage_json` value. A shape this build wrote always
    /// parses; a corrupt value yields `None` instead of failing a read.
    #[must_use]
    pub fn from_json(value: &Value) -> Option<Self> {
        serde_json::from_value(value.clone()).ok()
    }
}

fn add(target: &mut Option<u64>, value: u64) {
    *target = Some(target.unwrap_or(0).saturating_add(value));
}

/// Cumulative usage over many turns (`sessions.retrieve`, Agent metrics).
///
/// Token counters are omitted when no contributing turn carried a token
/// observation, so a context-only aggregate never reads as "zero tokens".
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageTotals {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    /// Turns that contributed at least one usage fact.
    pub turns: u64,
    /// LLM calls observed (sum of the turns' `calls`).
    pub calls: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_used: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl UsageTotals {
    /// Fold one turn's usage in chronological order: context/model keep the
    /// latest value, counters add up.
    pub fn absorb(&mut self, usage: &TurnUsage) {
        if usage.is_empty() {
            return;
        }
        self.turns = self.turns.saturating_add(1);
        self.calls = self.calls.saturating_add(usage.calls.unwrap_or(0));
        merge(&mut self.input_tokens, usage.input_tokens);
        merge(&mut self.output_tokens, usage.output_tokens);
        merge(&mut self.cache_read_tokens, usage.cache_read_tokens);
        merge(&mut self.cache_creation_tokens, usage.cache_creation_tokens);
        merge(&mut self.total_tokens, usage.total_tokens);
        if usage.context_used.is_some() {
            self.context_used = usage.context_used;
        }
        if usage.context_size.is_some() {
            self.context_size = usage.context_size;
        }
        if usage.model.is_some() {
            self.model = usage.model.clone();
        }
    }

    /// Add another aggregate's counters. `context_*`/`model` are deliberately
    /// not merged: "latest context" has no meaning across sessions.
    pub fn merge_counters(&mut self, other: &Self) {
        self.turns = self.turns.saturating_add(other.turns);
        self.calls = self.calls.saturating_add(other.calls);
        merge(&mut self.input_tokens, other.input_tokens);
        merge(&mut self.output_tokens, other.output_tokens);
        merge(&mut self.cache_read_tokens, other.cache_read_tokens);
        merge(&mut self.cache_creation_tokens, other.cache_creation_tokens);
        merge(&mut self.total_tokens, other.total_tokens);
    }

    /// Whether no turn contributed any fact.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.turns == 0
    }
}

fn merge(target: &mut Option<u64>, value: Option<u64>) {
    if let Some(value) = value {
        *target = Some(target.unwrap_or(0).saturating_add(value));
    }
}

/// One bounded Agent-metrics query (`GET /v1/agents/metrics`).
///
/// `start_time`/`end_time` are Unix **microseconds**; the store keeps turn
/// times in Unix seconds, so [`Self::second_bounds`] widens both ends to whole
/// seconds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetricsWindow {
    pub start_time: i64,
    pub end_time: i64,
    /// Maximum sessions returned; totals stay over the whole window.
    pub limit: u32,
}

impl MetricsWindow {
    pub const DEFAULT_WINDOW_US: i64 = 24 * 60 * 60 * 1_000_000;
    pub const MAX_WINDOW_US: i64 = 31 * 24 * 60 * 60 * 1_000_000;
    pub const DEFAULT_LIMIT: u32 = 50;
    pub const MAX_LIMIT: u32 = 500;

    /// Second-granularity bounds for the store: inclusive `start`, inclusive
    /// `end`; fractional ends widen to the whole second.
    #[must_use]
    pub fn second_bounds(&self) -> (i64, i64) {
        (
            self.start_time.div_euclid(1_000_000),
            self.end_time.div_euclid(1_000_000),
        )
    }
}

/// Per-session token totals for the Agent metrics endpoint.
///
/// Only sessions with at least one turn that carried a usage fact appear: a
/// session whose turns predate usage recording has nothing to report and is
/// not shown as `0`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentMetricsSession {
    pub session_id: String,
    pub agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    pub turns: u64,
    pub calls: u64,
    /// The session row's `last_activity_at` (RFC3339).
    pub last_active_at: String,
}

/// `GET /v1/agents/metrics` response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMetrics {
    pub start_time: i64,
    pub end_time: i64,
    /// Newest-first by total tokens, then last activity.
    pub sessions: Vec<AgentMetricsSession>,
    pub totals: UsageTotals,
    /// The window held more turn rows than one scan reads; totals cover the
    /// newest rows only.
    pub truncated: bool,
}

/// Build the response's `sessions[]` entry from one aggregate.
#[must_use]
pub fn metrics_session(
    session_id: &str,
    usage: &UsageTotals,
    session_model: Option<String>,
    last_active_at: &str,
) -> AgentMetricsSession {
    AgentMetricsSession {
        session_id: session_id.to_owned(),
        agent_id: "root".to_owned(),
        model: usage.model.clone().or(session_model),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_creation_tokens: usage.cache_creation_tokens,
        total_tokens: usage.total_tokens,
        turns: usage.turns,
        calls: usage.calls,
        last_active_at: last_active_at.to_owned(),
    }
}

/// Rendered `session.usage`: the aggregate as a JSON object (stable key order
/// is not promised; consumers read by name).
#[must_use]
pub fn totals_json(totals: &UsageTotals, truncated: bool) -> Value {
    let mut value = match serde_json::to_value(totals) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    };
    if truncated {
        value.insert("truncated".to_owned(), Value::Bool(true));
    }
    Value::Object(value)
}

#[cfg(test)]
#[path = "usage_test.rs"]
mod tests;
