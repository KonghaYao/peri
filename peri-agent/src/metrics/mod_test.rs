use std::sync::{Arc, Mutex, MutexGuard};

use super::*;

#[test]
fn test_truncate_short_string_unchanged() {
    let val = serde_json::json!({"error": "short"});
    let result = truncate_json_strings(val);
    assert_eq!(result["error"], "short");
}

#[test]
fn test_truncate_long_string() {
    let long: String = "x".repeat(600);
    let val = serde_json::json!({"error": long});
    let result = truncate_json_strings(val);
    assert_eq!(result["error"].as_str().unwrap().chars().count(), 500);
}

#[test]
fn test_truncate_cjk_string() {
    let long: String = "你".repeat(600);
    let val = serde_json::json!({"error": long});
    let result = truncate_json_strings(val);
    assert_eq!(result["error"].as_str().unwrap().chars().count(), 500);
}

#[test]
fn test_truncate_nested_object() {
    let long: String = "a".repeat(600);
    let val = serde_json::json!({"data": {"nested": long, "ok": "short"}, "arr": [long]});
    let result = truncate_json_strings(val);
    assert_eq!(
        result["data"]["nested"].as_str().unwrap().chars().count(),
        500
    );
    assert_eq!(result["data"]["ok"], "short");
    assert_eq!(result["arr"][0].as_str().unwrap().chars().count(), 500);
}

#[test]
fn test_truncate_non_string_unchanged() {
    let val = serde_json::json!({"count": 42, "flag": true, "null": null});
    let result = truncate_json_strings(val);
    assert_eq!(result["count"], 42);
    assert_eq!(result["flag"], true);
    assert!(result["null"].is_null());
}

#[test]
#[cfg_attr(not(unix), ignore = "RSS measurement only supported on Unix")]
fn test_current_rss_mb_returns_positive_on_unix() {
    let rss = current_rss_mb();
    assert!(rss.is_some(), "current_rss_mb() should return Some on Unix");
    assert!(rss.unwrap() > 0, "RSS should be positive");
}

#[cfg(unix)]
#[test]
fn test_current_rss_mb_is_realtime_not_monotonic_max() {
    // 验证返回的是当前 RSS（可下降），而非 ru_maxrss（单调递增）
    let baseline = current_rss_mb().expect("should get baseline RSS");
    assert!(baseline > 0);

    // 使用 mmap 分配并真实写入页面以强制 RSS 上升
    // Vec::drop → free() 在 macOS 上不归还物理页，必须用 munmap 才能验证回落
    let size = 200usize * 1024 * 1024; // 200 MB
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    assert_ne!(ptr, libc::MAP_FAILED, "mmap failed");
    // 逐页写入以触发实际物理页分配
    for i in 0..(size / 4096) {
        unsafe {
            *(ptr as *mut u8).add(i * 4096) = 1;
        }
    }
    let peak = current_rss_mb().expect("should get peak RSS after allocation");
    assert!(
        peak > baseline + 50,
        "peak RSS ({}) should be significantly > baseline ({}); got delta={}",
        peak,
        baseline,
        peak.saturating_sub(baseline)
    );

    // munmap 立即归还物理页给 OS（跨平台可靠）
    let ret = unsafe { libc::munmap(ptr, size) };
    assert_eq!(ret, 0, "munmap failed");
    let after = current_rss_mb().expect("should get RSS after free");
    // 允许 5 MB 容差（measurement overhead）
    assert!(
        after < peak.saturating_sub(50),
        "after-free RSS ({}) should be significantly < peak ({}). \
         munmap should return physical pages to the OS immediately",
        after,
        peak
    );
}

/// 出口槽是进程级共享状态：触碰它的用例必须串行，并在结束时清空出口。
static SINK_LOCK: Mutex<()> = Mutex::new(());

#[derive(Default)]
struct CapturingSink {
    events: Mutex<Vec<MetricEvent>>,
}

impl MetricsSink for CapturingSink {
    fn record(&self, event: MetricEvent) {
        self.events.lock().unwrap().push(event);
    }
}

/// 串行化 + 捕获出口；`Drop` 清空出口并释放锁。
struct SinkHarness {
    _lock: MutexGuard<'static, ()>,
    sink: Arc<CapturingSink>,
}

impl SinkHarness {
    fn new() -> Self {
        Self {
            _lock: SINK_LOCK.lock().unwrap_or_else(|e| e.into_inner()),
            sink: Arc::new(CapturingSink::default()),
        }
    }

    fn install(&self) {
        set_sink(Some(Arc::clone(&self.sink) as Arc<dyn MetricsSink>));
    }

    fn captured(&self, name: &str) -> Vec<MetricEvent> {
        self.sink
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.event == name)
            .cloned()
            .collect()
    }
}

impl Drop for SinkHarness {
    fn drop(&mut self) {
        set_sink(None);
    }
}

#[test]
fn emit_without_sink_drops_event_without_persistence() {
    let harness = SinkHarness::new();
    // 未安装出口 = 未配置 Langfuse：事件丢弃，本地不落盘（模块无文件系统写入）
    emit(
        "test.metrics.no-sink",
        serde_json::json!({"error": "boom"}),
        Some("sid-1"),
        Some("rid-1"),
    );
    assert!(harness.captured("test.metrics.no-sink").is_empty());
}

#[test]
fn emit_delivers_event_with_identity_and_truncation_to_sink() {
    let harness = SinkHarness::new();
    harness.install();
    emit(
        "test.metrics.deliver",
        serde_json::json!({"error": "x".repeat(600), "step": 3}),
        Some("sid-1"),
        Some("rid-1"),
    );

    let events = harness.captured("test.metrics.deliver");
    assert_eq!(
        events.len(),
        1,
        "installed sink must receive the event once"
    );
    let event = &events[0];
    assert_eq!(event.sid.as_deref(), Some("sid-1"));
    assert_eq!(event.rid.as_deref(), Some("rid-1"));
    assert_eq!(event.data["error"].as_str().unwrap().chars().count(), 500);
    assert_eq!(event.data["step"], 3);
    assert!(!event.ts.is_empty());
}

#[test]
fn emit_keeps_absent_identity_as_none() {
    let harness = SinkHarness::new();
    harness.install();
    emit("test.metrics.anonymous", serde_json::json!({}), None, None);

    let events = harness.captured("test.metrics.anonymous");
    assert_eq!(events.len(), 1);
    assert!(events[0].sid.is_none());
    assert!(events[0].rid.is_none());
}
