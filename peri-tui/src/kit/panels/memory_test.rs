//! Tests for memory panel utilities.

use super::*;

#[test]
fn test_format_size_bytes_below_kb() {
    // 0 走 fallthrough 路径（UNITS 全部 threshold 都不满足 0 >= t）
    assert_eq!(format_size(0), "0 B");
    // 1~9 走 B 分支：v = bytes/1.0 < 10 → "X.0 B"
    assert_eq!(format_size(1), "1.0 B");
    assert_eq!(format_size(9), "9.0 B");
    // 10~1023 走 B 分支：v >= 10 → "XX B"
    assert_eq!(format_size(10), "10 B");
    assert_eq!(format_size(512), "512 B");
    assert_eq!(format_size(1023), "1023 B");
}

#[test]
fn test_format_size_kb_threshold() {
    // 1024 = 1.0 KB（< 10，保留 1 位小数）
    assert_eq!(format_size(1024), "1.0 KB");
    // 1536 = 1.5 KB
    assert_eq!(format_size(1536), "1.5 KB");
    // 10240 = 10 KB（≥10，无小数）
    assert_eq!(format_size(10240), "10 KB");
    // 51200 = 50 KB
    assert_eq!(format_size(51200), "50 KB");
}

#[test]
fn test_format_size_mb_and_gb() {
    // 1 MB = 1048576
    assert_eq!(format_size(1048576), "1.0 MB");
    // 5 MB
    assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
    // 1 GB
    assert_eq!(format_size(1024 * 1024 * 1024), "1.0 GB");
}

#[test]
fn test_format_size_u64_max_no_overflow() {
    // u64::MAX ~ 18.45 EB，但本函数最高只到 GB 单位——不应 panic 或 overflow
    let s = format_size(u64::MAX);
    assert!(
        s.ends_with(" GB"),
        "expected GB suffix for u64::MAX, got: {}",
        s
    );
}

#[test]
fn test_format_relative_time_just_now() {
    use chrono::Utc;
    let now = Utc::now();
    // 30s 前 → "just now"
    assert_eq!(format_relative_time(now), "just now");
    // 59s 前 → "just now"
    let almost_minute_ago = now - chrono::Duration::seconds(59);
    assert_eq!(format_relative_time(almost_minute_ago), "just now");
}

#[test]
fn test_format_relative_time_minutes() {
    use chrono::Utc;
    let now = Utc::now();
    let five_min_ago = now - chrono::Duration::minutes(5);
    assert_eq!(format_relative_time(five_min_ago), "5m ago");
    let fifty_nine_min_ago = now - chrono::Duration::minutes(59);
    assert_eq!(format_relative_time(fifty_nine_min_ago), "59m ago");
}

#[test]
fn test_format_relative_time_hours() {
    use chrono::Utc;
    let now = Utc::now();
    let two_hours_ago = now - chrono::Duration::hours(2);
    assert_eq!(format_relative_time(two_hours_ago), "2h ago");
    let twenty_three_hours_ago = now - chrono::Duration::hours(23);
    assert_eq!(format_relative_time(twenty_three_hours_ago), "23h ago");
}

#[test]
fn test_format_relative_time_days() {
    use chrono::Utc;
    let now = Utc::now();
    let five_days_ago = now - chrono::Duration::days(5);
    assert_eq!(format_relative_time(five_days_ago), "5d ago");
    let twenty_nine_days_ago = now - chrono::Duration::days(29);
    assert_eq!(format_relative_time(twenty_nine_days_ago), "29d ago");
}

#[test]
fn test_format_relative_time_over_30_days_falls_to_date() {
    use chrono::Utc;
    let now = Utc::now();
    let forty_days_ago = now - chrono::Duration::days(40);
    // 超过 30 天 → 显示 YYYY-MM-DD
    let result = format_relative_time(forty_days_ago);
    assert!(
        result.len() == 10 && result.chars().nth(4) == Some('-'),
        "expected date format YYYY-MM-DD, got: {}",
        result
    );
}
