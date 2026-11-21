use super::*;

#[test]
fn reconcile_without_any_cursor_starts_at_the_tail() {
    assert_eq!(
        Cursor::reconcile_events(None, None, None).expect("tail"),
        EventCursor::Tail
    );
    assert_eq!(
        Cursor::reconcile_events(Some(0), None, None).expect("explicit zero"),
        EventCursor::After(Cursor::START),
        "an explicit zero still replays the whole log"
    );
    assert_eq!(
        Cursor::reconcile_events(Some(4), Some(4), Some(4)).expect("agree"),
        EventCursor::After(Cursor::new(4))
    );
}

#[test]
fn reconcile_rejects_disagreement() {
    for result in [
        Cursor::reconcile_events(Some(1), Some(2), None),
        Cursor::reconcile_events(Some(1), None, Some(2)),
        Cursor::reconcile_events(None, Some(1), Some(2)),
    ] {
        let error = result.expect_err("conflict");
        assert_eq!(error.code(), crate::domain::ErrorCode::ConflictingCursor);
    }
}

#[test]
fn optional_reconcile_preserves_absence() {
    assert_eq!(Cursor::reconcile_optional(None, None).expect("none"), None);
    assert_eq!(
        Cursor::reconcile_optional(None, Some(5)).expect("after"),
        Some(5)
    );
    assert_eq!(
        Cursor::reconcile_optional(Some(5), None).expect("afterSeq"),
        Some(5)
    );
    assert_eq!(
        Cursor::reconcile_optional(Some(5), Some(5)).expect("agree"),
        Some(5)
    );
    assert!(Cursor::reconcile_optional(Some(5), Some(6)).is_err());
}
