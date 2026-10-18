//! `peri.historyWindowV1` 纯类型测试：请求 schema、游标/票据绑定、错误码与双向计划。

use serde_json::json;

use super::*;
use crate::history_page::{HistoryPageCandidate, HistoryPagePlan};
use crate::messages::MessageId;

fn thread() -> ThreadId {
    "window-session".to_owned()
}

fn metadata(tail: Option<HistoryCommittedTail>) -> HistoryWindowMetadata {
    HistoryWindowMetadata {
        session_id: thread(),
        session_generation: "generation-1".into(),
        structure_version: 7,
        snapshot_token: "snapshot-1".into(),
        committed_tail: tail,
        checkpoint_id: Some("window-session:summary".into()),
    }
}

fn tail() -> HistoryCommittedTail {
    HistoryCommittedTail::new(&thread(), MessageId::new(), 42)
}

fn cursor(metadata: &HistoryWindowMetadata, source: HistorySource) -> HistoryWindowCursor {
    HistoryWindowCursor {
        thread_id: metadata.session_id.clone(),
        session_generation: metadata.session_generation.clone(),
        structure_version: metadata.structure_version,
        snapshot_token: metadata.snapshot_token.clone(),
        committed_tail: metadata.committed_tail.clone(),
        source,
        anchor: MessageId::new(),
        direction: HistoryWindowDirection::Before,
        projection_version: HISTORY_WINDOW_PROJECTION_VERSION,
    }
}

#[test]
fn open_defaults_to_latest_and_both_sides() {
    let request =
        HistoryWindowRequest::decode(&json!({"kind": "open", "direction": "around"})).unwrap();
    let HistoryWindowRequest::Open(request) = request else {
        panic!("expected open request");
    };
    let plan = request.validate().unwrap();
    assert_eq!(plan.anchor, HistoryWindowAnchor::Latest);
    assert_eq!(plan.before_limit, HISTORY_WINDOW_DEFAULT_LIMIT as usize);
    assert_eq!(plan.after_limit, HISTORY_WINDOW_DEFAULT_LIMIT as usize);
    assert_eq!(
        plan.byte_budget,
        HISTORY_WINDOW_DEFAULT_BYTE_BUDGET as usize
    );
}

#[test]
fn open_direction_only_fills_the_requested_side() {
    let request = HistoryWindowRequest::decode(
        &json!({"kind": "open", "direction": "before", "limit": 7, "byteBudget": 1024}),
    )
    .unwrap();
    let HistoryWindowRequest::Open(request) = request else {
        panic!("expected open request");
    };
    let plan = request.validate().unwrap();
    assert_eq!(plan.before_limit, 7);
    assert_eq!(plan.after_limit, 0);
    assert_eq!(plan.byte_budget, 1024);
}

#[test]
fn unknown_fields_and_kinds_are_not_degraded_to_initial_load() {
    let unknown_field = HistoryWindowRequest::decode(
        &json!({"kind": "open", "direction": "around", "cursor": "old"}),
    )
    .unwrap_err();
    assert_eq!(
        unknown_field,
        HistoryWindowError::invalid_request("history open fields are invalid")
    );

    let unknown_kind = HistoryWindowRequest::decode(&json!({"kind": "rewind"})).unwrap_err();
    assert_eq!(
        unknown_kind,
        HistoryWindowError::invalid_request("unknown history.kind")
    );

    let not_object = HistoryWindowRequest::decode(&json!("open")).unwrap_err();
    assert_eq!(not_object.code(), -32602);
}

#[test]
fn page_requires_exactly_one_cursor_and_a_valid_limit() {
    let base = json!({
        "kind": "page",
        "snapshotToken": "snapshot-1",
        "readTicket": "ticket-1",
        "limit": 10
    });
    let neither = HistoryWindowRequest::decode(&base).unwrap();
    let HistoryWindowRequest::Page(page) = neither else {
        panic!("expected page request");
    };
    assert_eq!(
        page.validate().unwrap_err(),
        HistoryWindowError::invalid_request("exactly one of beforeCursor/afterCursor is required")
    );

    let both = HistoryWindowRequest::decode(&json!({
        "kind": "page",
        "snapshotToken": "snapshot-1",
        "readTicket": "ticket-1",
        "beforeCursor": "a",
        "afterCursor": "b",
        "limit": 10
    }))
    .unwrap();
    let HistoryWindowRequest::Page(page) = both else {
        panic!("expected page request");
    };
    assert_eq!(
        page.validate().unwrap_err(),
        HistoryWindowError::invalid_request("beforeCursor and afterCursor are mutually exclusive")
    );

    let overflow = HistoryWindowRequest::decode(&json!({
        "kind": "page",
        "snapshotToken": "snapshot-1",
        "readTicket": "ticket-1",
        "beforeCursor": "a",
        "limit": HISTORY_WINDOW_MAX_LIMIT + 1
    }))
    .unwrap();
    let HistoryWindowRequest::Page(page) = overflow else {
        panic!("expected page request");
    };
    assert!(page.validate().is_err());
}

#[test]
fn page_reports_the_read_direction() {
    let request = HistoryWindowRequest::decode(&json!({
        "kind": "page",
        "snapshotToken": "snapshot-1",
        "readTicket": "ticket-1",
        "afterCursor": "cursor",
        "limit": 1
    }))
    .unwrap();
    let HistoryWindowRequest::Page(page) = request else {
        panic!("expected page request");
    };
    assert_eq!(page.validate().unwrap(), HistoryWindowDirection::After);
}

#[test]
fn seek_requires_a_stable_entry_anchor() {
    let request = HistoryWindowRequest::decode(&json!({
        "kind": "seek",
        "snapshotToken": "snapshot-1",
        "readTicket": "ticket-1",
        "anchor": {"entryId": "window-session:not-a-uuid", "source": "own"}
    }))
    .unwrap();
    let HistoryWindowRequest::Seek(seek) = request else {
        panic!("expected seek request");
    };
    assert_eq!(seek.validate().unwrap(), (50, 50));

    let parsed =
        HistoryEntryId::parse_for_session("window-session:not-a-uuid", &thread()).unwrap_err();
    assert_eq!(parsed.code(), -32602);

    let other_session = HistoryEntryId::new(&"other".to_owned(), MessageId::new());
    let foreign =
        HistoryEntryId::parse_for_session(&other_session.encode(), &thread()).unwrap_err();
    assert_eq!(foreign.code(), -32016);
}

#[test]
fn open_anchor_entry_shape_is_strict() {
    let raw = json!({"kind": "entry", "entryId": "window-session:uuid", "source": "own"});
    let anchor: HistoryWindowAnchor = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(serde_json::to_value(&anchor).unwrap(), raw);

    assert!(
        serde_json::from_value::<HistoryWindowAnchor>(json!({"kind": "latest", "extra": 1}))
            .is_err()
    );
    assert!(serde_json::from_value::<HistoryWindowAnchor>(
        json!({"kind": "entry", "entryId": "x", "source": "own", "extra": 1})
    )
    .is_err());
    assert!(serde_json::from_value::<HistoryWindowAnchor>(json!({"kind": "unknown"})).is_err());
}

#[test]
fn source_wire_shape_is_explicit() {
    assert_eq!(
        serde_json::to_value(HistorySource::Own).unwrap(),
        json!("own")
    );
    assert_eq!(
        serde_json::to_value(HistorySource::inherited("frozen-1")).unwrap(),
        json!({"inherited": {"frozenSnapshotId": "frozen-1"}})
    );
    let inherited: HistorySource =
        serde_json::from_value(json!({"inherited": {"frozenSnapshotId": "frozen-1"}})).unwrap();
    assert_eq!(inherited.frozen_snapshot_id(), Some("frozen-1"));
    assert_eq!(inherited.ancestor_thread_id(), None);

    let with_ancestor =
        HistorySource::inherited_with_ancestor("frozen-1", Some("parent-1".to_owned()));
    assert_eq!(
        serde_json::to_value(&with_ancestor).unwrap(),
        json!({
            "inherited": {
                "frozenSnapshotId": "frozen-1",
                "ancestorThreadId": "parent-1"
            }
        })
    );
    let decoded: HistorySource =
        serde_json::from_value(serde_json::to_value(&with_ancestor).unwrap()).unwrap();
    assert_eq!(decoded.ancestor_thread_id(), Some(&"parent-1".to_owned()));
}

#[test]
fn entry_identity_roundtrips_and_rejects_other_sessions() {
    let message_id = MessageId::new();
    let entry = HistoryEntryId::new(&thread(), message_id);
    assert_eq!(HistoryEntryId::parse(&entry.encode()).unwrap(), entry);
    assert_eq!(
        HistoryEntryId::parse_for_session(&entry.encode(), &thread()).unwrap(),
        message_id
    );
    assert_eq!(
        HistoryEntryId::parse_for_session(&entry.encode(), &"other".to_owned()).unwrap_err(),
        HistoryWindowError::anchor_unavailable("entryId belongs to another session")
    );
}

#[test]
fn committed_tail_is_opaque_and_session_bound() {
    let tail = tail();
    let encoded = tail.encode();
    assert_eq!(
        HistoryCommittedTail::decode(&encoded, &thread()).unwrap(),
        tail
    );
    assert_eq!(
        HistoryCommittedTail::decode(&encoded, &"other".to_owned()).unwrap_err(),
        HistoryWindowError::CrossSession {
            expected: "other".to_owned(),
            actual: thread(),
        }
    );
    assert!(HistoryCommittedTail::decode("not-json", &thread()).is_err());
}

#[test]
fn cursor_roundtrips_and_rejects_other_sessions() {
    let metadata = metadata(Some(tail()));
    let cursor = cursor(&metadata, HistorySource::Own);
    let encoded = cursor.encode();
    assert_eq!(
        HistoryWindowCursor::decode(&encoded, &thread()).unwrap(),
        cursor
    );
    assert_eq!(
        HistoryWindowCursor::decode(&encoded, &"other".to_owned()).unwrap_err(),
        HistoryWindowError::CrossSession {
            expected: "other".to_owned(),
            actual: thread(),
        }
    );
    assert!(HistoryWindowCursor::decode("not-json", &thread()).is_err());
}

#[test]
fn cursor_verification_distinguishes_invalidation_from_snapshot_mismatch() {
    let metadata = metadata(Some(tail()));
    let cursor = cursor(&metadata, HistorySource::Own);
    cursor
        .verify_against(&metadata, &HistorySource::Own)
        .expect("fresh cursor");

    let mut stale_structure = metadata.clone();
    stale_structure.structure_version += 1;
    assert_eq!(
        cursor
            .verify_against(&stale_structure, &HistorySource::Own)
            .unwrap_err()
            .code(),
        -32012
    );

    let mut other_snapshot = metadata.clone();
    other_snapshot.snapshot_token = "snapshot-2".into();
    assert_eq!(
        cursor
            .verify_against(&other_snapshot, &HistorySource::Own)
            .unwrap_err()
            .code(),
        -32015
    );

    assert_eq!(
        cursor
            .verify_against(&metadata, &HistorySource::inherited("frozen-1"))
            .unwrap_err()
            .code(),
        -32015
    );
}

#[test]
fn read_ticket_debug_never_prints_the_opaque_value() {
    let ticket = HistoryReadTicket::new("super-secret-ticket");
    assert_eq!(format!("{ticket:?}"), "HistoryReadTicket(<opaque>)");
    assert!(!format!("{ticket:?}").contains("super-secret-ticket"));
}

#[test]
fn read_ticket_binding_checks_expiry_generation_and_snapshot() {
    let metadata = metadata(Some(tail()));
    let binding = HistoryReadTicketBinding {
        connection_id: "connection-1".into(),
        session_id: metadata.session_id.clone(),
        session_generation: metadata.session_generation.clone(),
        structure_version: metadata.structure_version,
        snapshot_token: metadata.snapshot_token.clone(),
        source: HistorySource::Own,
        committed_tail: metadata.committed_tail.clone(),
        projection_version: HISTORY_WINDOW_PROJECTION_VERSION,
        issued_at_unix_ms: 1_000,
        expires_at_unix_ms: 2_000,
        nonce: "nonce".into(),
    };
    binding
        .validate_at(1_500, &metadata, &HistorySource::Own)
        .expect("live ticket");
    assert_eq!(
        binding
            .validate_at(2_000, &metadata, &HistorySource::Own)
            .unwrap_err(),
        HistoryWindowError::ReadTicketExpired
    );

    let mut stale = metadata.clone();
    stale.session_generation = "generation-2".into();
    assert_eq!(
        binding
            .validate_at(1_500, &stale, &HistorySource::Own)
            .unwrap_err()
            .code(),
        -32012
    );

    assert_eq!(
        binding
            .validate_at(1_500, &metadata, &HistorySource::inherited("frozen-1"))
            .unwrap_err()
            .code(),
        -32015
    );
}

#[test]
fn error_codes_and_reasons_match_the_wire_table() {
    let cases = [
        (
            HistoryWindowError::NotNegotiated,
            -32601,
            "history_window_not_negotiated",
        ),
        (
            HistoryWindowError::invalid_request("bad"),
            -32602,
            "invalid_history_request",
        ),
        (
            HistoryWindowError::SessionNotFound,
            -32602,
            "session_not_found",
        ),
        (
            HistoryWindowError::SessionClosing,
            -32010,
            "session_closing",
        ),
        (
            HistoryWindowError::MutationInProgress,
            -32010,
            "history_mutation_in_progress",
        ),
        (
            HistoryWindowError::CrossSession {
                expected: thread(),
                actual: "other".into(),
            },
            -32011,
            "cross_session_cursor",
        ),
        (
            HistoryWindowError::window_invalidated("rewind"),
            -32012,
            "history_window_invalidated",
        ),
        (
            HistoryWindowError::ReadTicketInvalid {
                detail: "bad".into(),
            },
            -32014,
            "history_read_ticket_invalid",
        ),
        (
            HistoryWindowError::ReadTicketExpired,
            -32014,
            "history_read_ticket_expired",
        ),
        (
            HistoryWindowError::SnapshotMismatch {
                detail: "mixed".into(),
            },
            -32015,
            "history_snapshot_mismatch",
        ),
        (
            HistoryWindowError::anchor_unavailable("missing"),
            -32016,
            "history_anchor_unavailable",
        ),
        (
            HistoryWindowError::ReadFailed {
                detail: "unsupported".into(),
            },
            -32603,
            "history_read_failed",
        ),
    ];
    for (error, code, reason) in cases {
        assert_eq!(error.code(), code, "{error}");
        assert_eq!(error.reason(), reason, "{error}");
    }
}

#[test]
fn body_chunk_validation_requires_progress_and_bounded_bytes() {
    let entry = HistoryEntryId::new(&thread(), MessageId::new()).encode();
    let valid = HistoryBodyChunk {
        entry_id: entry.clone(),
        offset: 0,
        bytes: b"abc".to_vec(),
        next_offset: Some(3),
        complete: false,
    };
    valid.validate(&entry, 0, 4).unwrap();
    assert!(valid.validate(&entry, 1, 4).is_err());
    assert!(valid.validate(&entry, 0, 2).is_err());

    let empty_progress = HistoryBodyChunk {
        entry_id: entry.clone(),
        offset: 0,
        bytes: Vec::new(),
        next_offset: Some(0),
        complete: false,
    };
    assert!(empty_progress.validate(&entry, 0, 4).is_err());

    let complete = HistoryBodyChunk {
        entry_id: entry.clone(),
        offset: 3,
        bytes: b"d".to_vec(),
        next_offset: None,
        complete: true,
    };
    complete.validate(&entry, 3, 4).unwrap();
}

#[test]
fn descriptor_requires_a_positive_length_and_bounded_chunk() {
    assert!(HistoryBodyDescriptor::new(0, 16, "snapshot", HistoryReadTicket::new("t")).is_err());
    assert!(HistoryBodyDescriptor::new(
        10,
        HISTORY_WINDOW_MAX_CHUNK_SIZE + 1,
        "snapshot",
        HistoryReadTicket::new("t")
    )
    .is_err());
    let descriptor =
        HistoryBodyDescriptor::new(10, 16, "snapshot", HistoryReadTicket::new("t")).unwrap();
    assert_eq!(descriptor.total_bytes, 10);
    assert_eq!(descriptor.snapshot_token, "snapshot");
}

#[test]
fn plan_after_selects_the_nearest_rows_and_reports_more() {
    let thread = thread();
    let candidates: Vec<HistoryPageCandidate> = (0..3)
        .map(|index| HistoryPageCandidate {
            message_id: MessageId::new(),
            position: index + 1,
            encoded_bytes: 4,
        })
        .collect();
    let plan = HistoryPagePlan::plan_after(&thread, "", candidates.clone(), 2, 100);
    assert_eq!(plan.entries(), &candidates[..2]);
    assert_eq!(plan.anchor(), Some(candidates[1].message_id));
    assert!(plan.has_more());
}

#[test]
fn rewind_invalidation_defaults_to_active() {
    let invalidation = HistoryRewindInvalidation::active();
    assert!(invalidation.is_active());
    assert_eq!(invalidation.status, HistoryRewindStatus::Active);
    assert!(!invalidation.reopen_required);
}
