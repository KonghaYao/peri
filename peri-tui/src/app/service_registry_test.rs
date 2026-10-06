//! 进程内存阈值告警（`peri.mem`）的规则与节流状态回归。

use super::*;
use std::time::{Duration, Instant};

/// 未超阈值（含恰好等于阈值）时不告警，并清除节流状态。
#[test]
fn memory_below_or_at_threshold_resets() {
    let now = Instant::now();
    assert_eq!(memory_warning_action(0, None, now), MemoryWarning::Reset);
    assert_eq!(
        memory_warning_action(MEMORY_WARN_THRESHOLD_MB, Some(now), now),
        MemoryWarning::Reset
    );
}

/// 首次越过阈值立即告警。
#[test]
fn memory_above_threshold_emits_on_first_sample() {
    let now = Instant::now();
    assert_eq!(
        memory_warning_action(MEMORY_WARN_THRESHOLD_MB + 1, None, now),
        MemoryWarning::Emit
    );
}

/// 持续超阈值按重复间隔告警：间隔内节流，达到间隔后再次记录。
#[test]
fn memory_above_threshold_throttles_until_repeat_interval() {
    let now = Instant::now();
    let within_interval = now - (MEMORY_WARN_REPEAT_INTERVAL - Duration::from_secs(1));
    assert_eq!(
        memory_warning_action(MEMORY_WARN_THRESHOLD_MB + 1, Some(within_interval), now),
        MemoryWarning::Throttled
    );
    let at_interval = now - MEMORY_WARN_REPEAT_INTERVAL;
    assert_eq!(
        memory_warning_action(MEMORY_WARN_THRESHOLD_MB + 1, Some(at_interval), now),
        MemoryWarning::Emit
    );
}

/// 采样器状态迁移：首次告警记录时刻 → 间隔内不更新 → 回落到阈值下清除。
#[test]
fn monitor_throttle_state_transitions() {
    let mut monitor = ProcessResourceMonitor::new();

    monitor.memory_mb = MEMORY_WARN_THRESHOLD_MB + 1;
    monitor.warn_if_memory_above_threshold();
    let first = monitor.last_memory_warn.expect("first overshoot emits");

    monitor.warn_if_memory_above_threshold();
    assert_eq!(
        monitor.last_memory_warn,
        Some(first),
        "重复间隔内不得重复告警"
    );

    monitor.memory_mb = MEMORY_WARN_THRESHOLD_MB;
    monitor.warn_if_memory_above_threshold();
    assert_eq!(monitor.last_memory_warn, None, "回落到阈值下须清除节流状态");
}
