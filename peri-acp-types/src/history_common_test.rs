//! 共用历史读取类型：身份、持久边界、正文分块与票据的最小回归。

use serde_json::json;

use super::*;
use crate::messages::MessageId;

fn thread() -> ThreadId {
    "session-a".to_owned()
}

#[test]
fn source_wire_shape_is_explicit() {
    assert_eq!(
        serde_json::to_value(HistorySource::own()).unwrap(),
        json!("own")
    );
    let inherited = HistorySource::inherited_with_ancestor("snap-1", Some("parent".into()));
    assert_eq!(
        serde_json::to_value(&inherited).unwrap(),
        json!({"inherited": {"frozenSnapshotId": "snap-1", "ancestorThreadId": "parent"}})
    );
    assert_eq!(inherited.frozen_snapshot_id(), Some("snap-1"));
    assert_eq!(
        inherited.ancestor_thread_id().map(String::as_str),
        Some("parent")
    );
}

#[test]
fn entry_identity_roundtrips_and_rejects_other_sessions() {
    let message_id = MessageId::new();
    let encoded = HistoryEntryId::new(&thread(), message_id).encode();
    let parsed = HistoryEntryId::parse(&encoded).unwrap();
    assert_eq!(parsed.thread_id, thread());
    assert_eq!(parsed.message_id, message_id);
    assert!(HistoryEntryId::parse("no-separator").is_err());
    assert!(matches!(
        HistoryEntryId::parse_for_session(&encoded, &"other".to_owned()),
        Err(HistoryWindowError::AnchorUnavailable { .. })
    ));
}

#[test]
fn committed_tail_is_opaque_and_session_bound() {
    let tail = HistoryCommittedTail::new(&thread(), MessageId::new(), 42);
    let encoded = tail.encode();
    assert_eq!(
        HistoryCommittedTail::decode(&encoded, &thread()).unwrap(),
        tail
    );
    assert!(matches!(
        HistoryCommittedTail::decode(&encoded, &"other".to_owned()),
        Err(HistoryWindowError::CrossSession { .. })
    ));
    assert!(matches!(
        HistoryCommittedTail::decode("not-json", &thread()),
        Err(HistoryWindowError::SnapshotMismatch { .. })
    ));
}

#[test]
fn read_ticket_debug_never_prints_the_opaque_value() {
    let ticket = HistoryReadTicket::new("super-secret-ticket");
    let debug = format!("{ticket:?}");
    assert!(!debug.contains("super-secret-ticket"));
    assert_eq!(debug, "HistoryReadTicket(<opaque>)");
}

#[test]
fn descriptor_requires_a_positive_length_and_bounded_chunk() {
    let ticket = HistoryReadTicket::new("ticket");
    assert!(HistoryBodyDescriptor::new(0, 1024, "snap", ticket.clone()).is_err());
    assert!(HistoryBodyDescriptor::new(10, 0, "snap", ticket.clone()).is_err());
    assert!(HistoryBodyDescriptor::new(
        10,
        HISTORY_WINDOW_MAX_CHUNK_SIZE + 1,
        "snap",
        ticket.clone()
    )
    .is_err());
    assert!(HistoryBodyDescriptor::new(10, 1024, "snap", ticket).is_ok());
}

#[test]
fn body_chunk_validation_requires_progress_and_bounded_bytes() {
    let chunk = HistoryBodyChunk {
        entry_id: "s:m".into(),
        offset: 0,
        bytes: vec![1, 2, 3],
        next_offset: Some(3),
        complete: false,
    };
    assert!(chunk.validate("s:m", 0, 3).is_ok());
    assert!(chunk.validate("s:other", 0, 3).is_err());
    assert!(chunk.validate("s:m", 1, 3).is_err());
    assert!(chunk.validate("s:m", 0, 2).is_err());

    let complete = HistoryBodyChunk {
        next_offset: None,
        complete: true,
        ..chunk.clone()
    };
    assert!(complete.validate("s:m", 0, 3).is_ok());

    let inconsistent = HistoryBodyChunk {
        next_offset: Some(99),
        ..chunk
    };
    assert!(inconsistent.validate("s:m", 0, 3).is_err());
}

#[test]
fn inherited_snapshot_uses_the_child_session_identity() {
    let snapshot = HistoryInheritedSnapshot {
        snapshot_id: "snap".into(),
        ancestor_thread_id: Some("parent".into()),
        first_entry: None,
        last_entry: None,
        entry_count: 0,
    };
    let child = thread();
    let message_id = MessageId::new();
    assert_eq!(
        snapshot.entry_id(&child, message_id),
        HistoryEntryId::new(&child, message_id).encode()
    );
}
