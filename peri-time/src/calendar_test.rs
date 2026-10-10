use super::*;
use std::time::{Duration, UNIX_EPOCH};

#[test]
fn test_utc_date_crosses_midnight_at_utc_boundary() {
    let before = UNIX_EPOCH + Duration::from_secs(86_399);
    let after = UNIX_EPOCH + Duration::from_secs(86_400);
    assert_eq!(
        calendar_date(before, CalendarConvention::Utc).to_string(),
        "1970-01-01"
    );
    assert_eq!(
        calendar_date(after, CalendarConvention::Utc).to_string(),
        "1970-01-02"
    );
}

#[test]
fn test_utc_timestamp_has_explicit_zone() {
    assert_eq!(format_utc_rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00+00:00");
    assert_eq!(
        format_utc_rfc3339(UNIX_EPOCH + Duration::from_millis(123)),
        "1970-01-01T00:00:00.123+00:00"
    );
}
