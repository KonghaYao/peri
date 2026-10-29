use std::sync::Arc;

use super::TurnTraceRegistry;

#[test]
fn registered_sid_resolves_to_turn_trace_with_parent() {
    let registry = TurnTraceRegistry::default();
    registry.register("sid-1", "trace-1", Some("trace-1"));

    let active = registry.resolve("sid-1").expect("sid must resolve");
    assert_eq!(active.trace_id, "trace-1");
    assert_eq!(
        active.parent_observation_id.as_deref(),
        Some("trace-1"),
        "采样的 turn 有一个根 span，指标应挂其下"
    );
}

#[test]
fn unsampled_registration_resolves_without_parent() {
    let registry = TurnTraceRegistry::default();
    registry.register("sid-1", "trace-1", None);

    let active = registry.resolve("sid-1").expect("sid must resolve");
    assert_eq!(active.trace_id, "trace-1");
    assert!(
        active.parent_observation_id.is_none(),
        "未采样 turn 没有根 span，指标只共享 trace"
    );
}

#[test]
fn unknown_sid_has_no_active_trace() {
    let registry = TurnTraceRegistry::default();
    assert!(registry.resolve("sid-missing").is_none());
}

#[test]
fn cleared_sid_has_no_active_trace() {
    let registry = TurnTraceRegistry::default();
    registry.register("sid-1", "trace-1", None);
    registry.clear("sid-1", "trace-1");
    assert!(registry.resolve("sid-1").is_none());
}

#[test]
fn clear_keeps_newer_turn_registration() {
    let registry = TurnTraceRegistry::default();
    registry.register("sid-1", "trace-old", None);
    registry.register("sid-1", "trace-new", None);

    // 旧 turn 收尾不得清掉新 turn 的登记
    registry.clear("sid-1", "trace-old");
    assert_eq!(
        registry
            .resolve("sid-1")
            .expect("newer registration survives")
            .trace_id,
        "trace-new"
    );
}

#[test]
fn clear_trace_removes_main_and_child_agent_sids() {
    let registry = TurnTraceRegistry::default();
    registry.register("sess-main", "trace-1", Some("trace-1"));
    registry.register("child-thread-1", "trace-1", Some("trace-1"));
    registry.register("child-thread-2", "trace-1", None);

    registry.clear_trace("trace-1");

    assert!(registry.resolve("sess-main").is_none());
    assert!(registry.resolve("child-thread-1").is_none());
    assert!(registry.resolve("child-thread-2").is_none());
}

#[test]
fn clear_trace_leaves_other_traces_registered() {
    let registry = TurnTraceRegistry::default();
    registry.register("sess-main", "trace-1", Some("trace-1"));
    registry.register("child-thread-1", "trace-1", Some("trace-1"));
    registry.register("sess-other", "trace-2", Some("trace-2"));

    registry.clear_trace("trace-1");

    assert!(registry.resolve("sess-main").is_none());
    assert!(registry.resolve("child-thread-1").is_none());
    assert_eq!(
        registry
            .resolve("sess-other")
            .expect("other trace must survive")
            .trace_id,
        "trace-2"
    );
}

#[test]
fn clear_trace_keeps_later_turn_registration_for_same_sid() {
    let registry = TurnTraceRegistry::default();
    registry.register("sid-1", "trace-old", Some("trace-old"));
    registry.register("sid-1", "trace-new", Some("trace-new"));

    // 旧 turn 的 clear_trace 只认 trace 身份，不误删后一 turn
    registry.clear_trace("trace-old");
    assert_eq!(
        registry
            .resolve("sid-1")
            .expect("later turn must survive")
            .trace_id,
        "trace-new"
    );
}

#[test]
fn sids_are_isolated() {
    let registry = TurnTraceRegistry::default();
    registry.register("sid-a", "trace-a", None);
    registry.register("sid-b", "trace-b", None);

    assert_eq!(registry.resolve("sid-a").unwrap().trace_id, "trace-a");
    assert_eq!(registry.resolve("sid-b").unwrap().trace_id, "trace-b");

    registry.clear("sid-a", "trace-a");
    assert!(registry.resolve("sid-a").is_none());
    assert_eq!(registry.resolve("sid-b").unwrap().trace_id, "trace-b");
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
                    registry.register(&sid, &trace, Some(trace.as_str()));
                    let active = registry.resolve(&sid).expect("registered sid resolves");
                    assert_eq!(active.trace_id, trace);
                    assert_eq!(
                        active.parent_observation_id.as_deref(),
                        Some(trace.as_str())
                    );
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
