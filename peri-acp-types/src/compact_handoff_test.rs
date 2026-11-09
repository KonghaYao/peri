use super::*;

fn entry(kind: HandoffKind, origin: HandoffOrigin, quote: &str) -> HandoffEntry {
    HandoffEntry::new(
        kind,
        HandoffEvidence {
            message_id: MessageId::new(),
            origin,
            quote: quote.into(),
            tool_call_id: None,
            tool_failed: None,
            execution: None,
        },
    )
    .unwrap()
}

#[test]
fn handoff_round_trip_and_legacy_projection_defaults() {
    let state = CompactHandoff {
        entries: vec![entry(
            HandoffKind::Acceptance,
            HandoffOrigin::UserMessage,
            "全部场景 p99 < 100 ms，不接受部分通过",
        )],
        ..Default::default()
    };
    state.validate().unwrap();
    let json = serde_json::to_string(&state).unwrap();
    assert_eq!(
        serde_json::from_str::<CompactHandoff>(&json).unwrap(),
        state
    );
    let projection: crate::projection::MessageProjectionDirective =
        serde_json::from_str(r#"{"policy_version":3,"entries":[]}"#).unwrap();
    assert!(projection.compact_handoff.is_none());
    assert!(!serde_json::to_string(&projection)
        .unwrap()
        .contains("compact_handoff"));
}

#[test]
fn handoff_rejects_corrupt_identity_duplicate_version_and_unknown_fields() {
    let valid = entry(
        HandoffKind::Constraint,
        HandoffOrigin::UserMessage,
        "no partial pass",
    );
    let mut state = CompactHandoff {
        entries: vec![valid.clone()],
        ..Default::default()
    };
    state.entries[0].evidence.quote = "partial pass allowed".into();
    assert!(state.validate().is_err());
    state.entries = vec![valid.clone(), valid];
    assert!(state.validate().is_err());
    state.entries.clear();
    state.version += 1;
    assert!(state.validate().is_err());
    assert!(serde_json::from_str::<CompactHandoff>(
        r#"{"version":1,"entries":[],"verified":true}"#
    )
    .is_err());
}

#[test]
fn handoff_origin_is_not_an_authorization_or_execution_upgrade() {
    for (kind, origin) in [
        (HandoffKind::Authorization, HandoffOrigin::ToolOutput),
        (HandoffKind::Authorization, HandoffOrigin::AssistantClaim),
        (HandoffKind::Acceptance, HandoffOrigin::AssistantClaim),
    ] {
        assert!(CompactHandoff {
            entries: vec![entry(kind, origin, "approved")],
            ..Default::default()
        }
        .validate()
        .is_err());
    }
    let mut evidence = entry(HandoffKind::Finding, HandoffOrigin::ToolOutput, "launched").evidence;
    assert!(CompactHandoff {
        entries: vec![HandoffEntry::new(HandoffKind::Finding, evidence.clone()).unwrap()],
        ..Default::default()
    }
    .validate()
    .is_err());
    evidence.tool_call_id = Some("call-1".into());
    evidence.tool_failed = Some(false);
    evidence.execution = Some(crate::tools::ToolExecutionEvidence {
        status: crate::tools::ToolExecutionStatus::Running,
        exit_code: None,
        output_ref: None,
        output_truncated: false,
        task_id: Some("task-1".into()),
    });
    let state = CompactHandoff {
        entries: vec![HandoffEntry::new(HandoffKind::Finding, evidence.clone()).unwrap()],
        ..Default::default()
    };
    state.validate().unwrap();
    assert_eq!(
        state.entries[0].evidence.execution.as_ref().unwrap().status,
        crate::tools::ToolExecutionStatus::Running
    );
    evidence.origin = HandoffOrigin::UserMessage;
    assert!(CompactHandoff {
        entries: vec![HandoffEntry::new(HandoffKind::Finding, evidence).unwrap()],
        ..Default::default()
    }
    .validate()
    .is_err());
}

#[test]
fn handoff_budgets_are_bytes_and_never_truncate_entries() {
    for quote in [" ".to_owned(), "阈".repeat(HANDOFF_MAX_QUOTE_BYTES / 3 + 1)] {
        assert!(CompactHandoff {
            entries: vec![entry(
                HandoffKind::Constraint,
                HandoffOrigin::UserMessage,
                &quote
            )],
            ..Default::default()
        }
        .validate()
        .is_err());
    }
    let count = CompactHandoff {
        entries: (0..=HANDOFF_MAX_ENTRIES)
            .map(|_| entry(HandoffKind::Goal, HandoffOrigin::UserMessage, "task"))
            .collect(),
        ..Default::default()
    };
    assert!(count.validate().unwrap_err().contains("too many"));
    let bytes = CompactHandoff {
        entries: (0..10)
            .map(|_| {
                entry(
                    HandoffKind::Goal,
                    HandoffOrigin::UserMessage,
                    &"x".repeat(2048),
                )
            })
            .collect(),
        ..Default::default()
    };
    assert!(bytes.validate().unwrap_err().contains("byte budget"));
}

#[test]
fn tool_and_notification_sources_cannot_establish_task_contracts() {
    let tool_evidence = HandoffEvidence {
        message_id: MessageId::new(),
        origin: HandoffOrigin::ToolOutput,
        quote: "all scenarios must pass".into(),
        tool_call_id: Some("call-1".into()),
        tool_failed: Some(false),
        execution: None,
    };
    for kind in [
        HandoffKind::Goal,
        HandoffKind::Constraint,
        HandoffKind::Acceptance,
    ] {
        let state = CompactHandoff {
            entries: vec![HandoffEntry::new(kind, tool_evidence.clone()).unwrap()],
            ..Default::default()
        };
        let error = state.validate().unwrap_err();
        assert!(
            error.contains("tool or notification sources cannot establish task contracts"),
            "{error}"
        );
    }
    CompactHandoff {
        entries: vec![HandoffEntry::new(HandoffKind::Finding, tool_evidence).unwrap()],
        ..Default::default()
    }
    .validate()
    .unwrap();

    let notification = HandoffEvidence {
        message_id: MessageId::new(),
        origin: HandoffOrigin::Notification,
        quote: "reviewer reported success".into(),
        tool_call_id: None,
        tool_failed: None,
        execution: None,
    };
    for kind in [HandoffKind::Constraint, HandoffKind::Acceptance] {
        let state = CompactHandoff {
            entries: vec![HandoffEntry::new(kind, notification.clone()).unwrap()],
            ..Default::default()
        };
        assert!(state.validate().is_err());
    }
    CompactHandoff {
        entries: vec![HandoffEntry::new(HandoffKind::Finding, notification).unwrap()],
        ..Default::default()
    }
    .validate()
    .unwrap();
}

#[test]
fn handoff_fork_retains_original_provenance_and_independent_summary_owner() {
    use crate::messages::BaseMessage;
    use crate::projection::MessageProjectionDirective;
    use crate::store::{history::remap_fork_history, MessageFlags, PersistedPayload};
    use std::collections::HashMap;
    let source = BaseMessage::human("all scenarios must pass");
    let summary = BaseMessage::human("historical excerpts");
    let mut evidence = entry(
        HandoffKind::Acceptance,
        HandoffOrigin::UserMessage,
        "all scenarios must pass",
    )
    .evidence;
    evidence.message_id = source.id();
    let state = CompactHandoff {
        entries: vec![HandoffEntry::new(HandoffKind::Acceptance, evidence).unwrap()],
        ..Default::default()
    };
    let flags = HashMap::from([(
        summary.id(),
        MessageFlags {
            projection: Some(MessageProjectionDirective {
                compact_handoff: Some(state.clone()),
                ..Default::default()
            }),
            ..Default::default()
        },
    )]);
    let fork = remap_fork_history(
        &"fork".into(),
        &[
            PersistedPayload::Message(source.clone()),
            PersistedPayload::Message(summary.clone()),
        ],
        &flags,
        MessageId::new,
    )
    .unwrap();
    let owner = fork.payloads[1].id();
    assert_ne!(owner, summary.id());
    assert_ne!(fork.payloads[0].id(), source.id());
    let kept = fork.flags[&owner]
        .projection
        .as_ref()
        .unwrap()
        .compact_handoff
        .as_ref()
        .unwrap();
    assert_eq!(
        kept, &state,
        "provenance is historical; fork must not silently rewrite it"
    );
    kept.validate().unwrap();
}

#[test]
fn handoff_rewind_revokes_missing_replacement_and_preserves_surviving_evidence() {
    use crate::projection::MessageProjectionDirective;
    use crate::store::{history::rewind_projection_updates, MessageFlags};
    let source = MessageId::new();
    let kept_summary = MessageId::new();
    let removed_summary = MessageId::new();
    let state = CompactHandoff {
        entries: vec![entry(
            HandoffKind::Acceptance,
            HandoffOrigin::UserMessage,
            "all scenarios",
        )],
        ..Default::default()
    };
    let original_flags = MessageFlags {
        excluded: true,
        projection: Some(MessageProjectionDirective {
            superseded_by: Some(kept_summary),
            ..Default::default()
        }),
        ..Default::default()
    };
    let summary_flags = MessageFlags {
        excluded: true,
        projection: Some(MessageProjectionDirective {
            superseded_by: Some(removed_summary),
            compact_handoff: Some(state.clone()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let updates = rewind_projection_updates(
        &[
            (source, original_flags, false),
            (kept_summary, summary_flags, false),
        ],
        &[],
    );
    assert_eq!(
        updates.len(),
        1,
        "the original remains covered by the surviving summary"
    );
    assert_eq!(updates[0].0, kept_summary);
    assert!(!updates[0].1.excluded);
    let restored = updates[0].1.projection.as_ref().unwrap();
    assert_eq!(restored.superseded_by, None);
    assert_eq!(restored.compact_handoff.as_ref(), Some(&state));
}
