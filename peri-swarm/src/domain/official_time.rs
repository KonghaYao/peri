//! Clock and timestamp conversion helpers for the official event frames.
//!
//! These are re-exported from [`super::official`] so call sites keep using
//! `crate::domain::official::unix_now` and friends.

/// Unix seconds, used for the `created_at`/`started_at` fields an official
/// `Turn`/session carries.
#[must_use]
pub fn unix_now() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0),
    )
    .unwrap_or(i64::MAX)
}

/// Convert the persisted RFC3339 item timestamp to Unix seconds.
///
/// A malformed timestamp yields `0` rather than a fabricated "now": replay must
/// be deterministic, and `0` is visibly not a real observation.
#[must_use]
pub fn unix_from_rfc3339(value: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.timestamp())
        .unwrap_or(0)
}
