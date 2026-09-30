use std::sync::Arc;

use super::TurnTraceRegistry;

#[test]
fn registered_sid_resolves_to_turn_trace() {
    let registry = TurnTraceRegistry::default();
    registry.register("sid-1", "trace-1");
    assert_eq!(registry.resolve("sid-1").as_deref(), Some("trace-1"));
}

#[test]
fn unknown_sid_has_no_active_trace() {
    let registry = TurnTraceRegistry::default();
    assert!(registry.resolve("sid-missing").is_none());
}

#[test]
fn cleared_sid_has_no_active_trace() {
    let registry = TurnTraceRegistry::default();
    registry.register("sid-1", "trace-1");
    registry.clear("sid-1", "trace-1");
    assert!(registry.resolve("sid-1").is_none());
}

#[test]
fn clear_keeps_newer_turn_registration() {
    let registry = TurnTraceRegistry::default();
    registry.register("sid-1", "trace-old");
    registry.register("sid-1", "trace-new");

    // 旧 turn 收尾不得清掉新 turn 的登记
    registry.clear("sid-1", "trace-old");
    assert_eq!(registry.resolve("sid-1").as_deref(), Some("trace-new"));
}

#[test]
fn sids_are_isolated() {
    let registry = TurnTraceRegistry::default();
    registry.register("sid-a", "trace-a");
    registry.register("sid-b", "trace-b");

    assert_eq!(registry.resolve("sid-a").as_deref(), Some("trace-a"));
    assert_eq!(registry.resolve("sid-b").as_deref(), Some("trace-b"));

    registry.clear("sid-a", "trace-a");
    assert!(registry.resolve("sid-a").is_none());
    assert_eq!(registry.resolve("sid-b").as_deref(), Some("trace-b"));
}

#[test]
fn concurrent_sids_never_cross_resolve() {
    // 每个 sid 只由自己的线程登记/清理：并发下不得取到别的 sid 的 trace。
    let registry = Arc::new(TurnTraceRegistry::default());
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let registry = Arc::clone(&registry);
            std::thread::spawn(move || {
                let sid = format!("sid-{i}");
                let trace = format!("trace-{i}");
                for _ in 0..200 {
                    registry.register(&sid, &trace);
                    assert_eq!(registry.resolve(&sid).as_deref(), Some(trace.as_str()));
                    registry.clear(&sid, &trace);
                    assert!(registry.resolve(&sid).is_none());
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("registry thread must not panic");
    }
    assert!(registry.resolve("sid-0").is_none());
}
