use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hasher},
    time::Duration,
};

use super::ExportConfig;
use crate::error::LangfuseError;

pub(super) fn budget_error() -> LangfuseError {
    LangfuseError::IngestionApi("OTLP cumulative export time budget exhausted".into())
}

pub(super) fn parse_retry_after(value: &str) -> Option<Duration> {
    parse_retry_after_at(value, chrono::Utc::now())
}

pub(super) fn parse_retry_after_at(
    value: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<Duration> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|digit| digit.is_ascii_digit()) {
        return Some(
            value
                .parse::<u64>()
                .map(Duration::from_secs)
                .unwrap_or(Duration::MAX),
        );
    }
    let timestamp = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    Some(
        timestamp
            .signed_duration_since(now)
            .to_std()
            .unwrap_or(Duration::ZERO),
    )
}

pub(super) fn delay(
    config: &ExportConfig,
    attempt: usize,
    retry_after: Option<Duration>,
) -> Duration {
    let jitter = RandomState::new().build_hasher().finish();
    jittered_backoff(config, attempt, jitter).max(retry_after.unwrap_or(Duration::ZERO))
}

pub(super) fn jittered_backoff(config: &ExportConfig, attempt: usize, jitter: u64) -> Duration {
    let mut upper = config.initial_retry_delay.min(config.max_retry_delay);
    for _ in 0..attempt.min(128) {
        if upper >= config.max_retry_delay {
            break;
        }
        upper = upper.saturating_mul(2).min(config.max_retry_delay);
    }
    let nanos = (upper.as_nanos() * (500_000 + jitter % 500_001) as u128 / 1_000_000).max(1);
    Duration::new(
        (nanos / 1_000_000_000) as u64,
        (nanos % 1_000_000_000) as u32,
    )
}
