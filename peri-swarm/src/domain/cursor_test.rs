use super::*;

#[test]
fn reconcile_defaults_to_start() {
    assert_eq!(
        Cursor::reconcile(None, None, None).expect("start"),
        Cursor::START
    );
    assert_eq!(
        Cursor::reconcile(Some(4), Some(4), Some(4)).expect("agree"),
        Cursor::new(4)
    );
}

#[test]
fn reconcile_rejects_disagreement() {
    for result in [
        Cursor::reconcile(Some(1), Some(2), None),
        Cursor::reconcile(Some(1), None, Some(2)),
        Cursor::reconcile(None, Some(1), Some(2)),
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
