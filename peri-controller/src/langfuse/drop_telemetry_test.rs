use super::*;

#[test]
fn test_registry_records_only_safe_drop_dimensions() {
    let registry = LangfuseDropRegistry::new(2);
    registry.record(
        "trace-a",
        LangfuseEventKind::Generation,
        LangfuseDropReason::DropNewQueueFull,
    );
    let snapshot = registry.snapshot("trace-a").expect("snapshot must exist");

    assert_eq!(snapshot.total, 1);
    assert_eq!(
        snapshot.by_event_kind.get(&LangfuseEventKind::Generation),
        Some(&1)
    );
    assert_eq!(
        snapshot
            .by_reason
            .get(&LangfuseDropReason::DropNewQueueFull),
        Some(&1)
    );
    assert!(!format!("{snapshot:?}").contains("sentinel-secret"));
}

#[test]
fn test_registry_evicts_oldest_trace_at_capacity() {
    let registry = LangfuseDropRegistry::new(1);
    registry.record(
        "trace-a",
        LangfuseEventKind::Span,
        LangfuseDropReason::BatcherClosed,
    );
    registry.record(
        "trace-b",
        LangfuseEventKind::Observation,
        LangfuseDropReason::DropNewQueueFull,
    );

    assert!(registry.snapshot("trace-a").is_none());
    assert_eq!(
        registry.snapshot("trace-b").expect("newest snapshot").total,
        1
    );
}

#[test]
fn test_error_reason_mapping_is_stable() {
    assert_eq!(
        LangfuseDropReason::from_error(&LangfuseError::QueueFull),
        Some(LangfuseDropReason::DropNewQueueFull)
    );
    assert_eq!(
        LangfuseDropReason::from_error(&LangfuseError::ChannelClosed),
        Some(LangfuseDropReason::BatcherClosed)
    );
}
