use std::sync::Arc;

use super::test_support::*;
use super::*;

#[test]
fn resident_index_keeps_slot_prefix_sums_in_layout_order() {
    let mut window = enabled_window(WindowLimits::default());
    let a = window
        .register_segment(
            segment_with_units("a", 10, 1, 10, 20),
            SegmentPlacement::Tail,
        )
        .expect("a");
    let b = window
        .register_segment(
            segment_with_units("b", 20, 2, 20, 30),
            SegmentPlacement::Tail,
        )
        .expect("b");
    assert_eq!(window.index().slot_range(a), Some(0..1));
    assert_eq!(window.index().slot_range(b), Some(1..3));
    assert_eq!(window.unit_count(), 3);

    let c = window
        .register_segment(
            segment_with_units("c", 5, 3, 30, 40),
            SegmentPlacement::BeforeSegment(a),
        )
        .expect("c");
    assert_eq!(window.index().slot_range(c), Some(0..3));
    assert_eq!(window.index().slot_range(a), Some(3..4));
    assert_eq!(window.index().slot_range(b), Some(4..6));
    assert_eq!(window.unit_count(), 6);
}

#[test]
fn resident_index_rejects_duplicate_identity_until_segment_is_released() {
    let mut window = enabled_window(WindowLimits::default());
    let first = window
        .register_segment(segment("a", 10, 10, 20), SegmentPlacement::Tail)
        .expect("first");
    assert!(matches!(
        window.register_segment(segment("a", 10, 10, 20), SegmentPlacement::Tail),
        Err(WindowModelError::DuplicateEntry(_))
    ));

    window.detach_segment(first).expect("detach");
    assert!(
        !window
            .index()
            .contains_entry(&PersistentEntryKey::own("s1:a"))
    );
    assert!(
        window
            .register_segment(segment("a", 10, 10, 20), SegmentPlacement::Tail)
            .is_ok(),
        "after eviction the same identity may be materialized again (A25)"
    );
}

#[test]
fn shared_arc_allocation_is_counted_once_but_distinct_allocations_are_not() {
    let mut index = ResidentIndex::default();
    let shared = Arc::new(vec![0u8; 64]);
    assert_eq!(index.account_shared_arc(&shared, 64), 64);
    assert_eq!(index.account_shared_arc(&shared.clone(), 64), 0);
    let other = Arc::new(vec![0u8; 32]);
    assert_eq!(index.account_shared_arc(&other, 32), 32);
    assert_eq!(index.allocations().bytes(), 96);
    assert_eq!(index.allocations().deduplicated_calls(), 1);
    assert_eq!(index.allocations().len(), 2);
}
