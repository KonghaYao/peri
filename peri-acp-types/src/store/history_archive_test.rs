use super::*;
use crate::context_archive::{build_catalog, ArchiveRoot};
use crate::projection::{ArchiveGroupLink, ProjectionActionEntry, ProjectionTarget};

#[test]
fn test_archive_root_upgrade_before_old_anchor_keeps_the_complete_range() {
    use crate::context_archive::coalesce_archive_roots;
    // IDs deliberately oppose canonical order: UUID comparisons are not history order.
    let first = MessageId::from(uuid::Uuid::from_u128(90));
    let last = MessageId::from(uuid::Uuid::from_u128(10));
    let tail = MessageId::from(uuid::Uuid::from_u128(1));
    let root = |anchor| ArchiveRoot {
        source_thread_id: "source".into(),
        first_anchor: anchor,
        last_anchor: anchor,
        committed_tail: tail,
    };
    let merged = coalesce_archive_roots(vec![root(last), root(first)], &[first, last, tail]);
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].first_anchor, first);
    assert_eq!(merged[0].last_anchor, last);
    assert_eq!(merged[0].committed_tail, tail);
    let next = MessageId::new();
    let merged = coalesce_archive_roots(
        vec![
            merged[0].clone(),
            ArchiveRoot {
                source_thread_id: "source".into(),
                first_anchor: next,
                last_anchor: next,
                committed_tail: next,
            },
        ],
        &[next],
    );
    assert_eq!(
        merged[0].first_anchor, first,
        "Full-released prefix stays recoverable"
    );
    assert_eq!(merged[0].last_anchor, next);
}

fn archived_history() -> (Vec<PersistedPayload>, HashMap<MessageId, MessageFlags>) {
    let call = BaseMessage::ai_with_tool_calls(
        "independent conclusion",
        vec![crate::messages::ToolCallRequest::new(
            "call",
            "Write",
            serde_json::json!({"path":"file","content":"past code"}),
        )],
    );
    let result = BaseMessage::tool_result("call", "persisted success");
    let payloads = vec![
        PersistedPayload::Message(call),
        PersistedPayload::Message(result),
    ];
    let anchor = payloads[1].id();
    let catalog = build_catalog(
        "source".into(),
        "group".into(),
        anchor,
        &payloads,
        "Completed Write; details require archive lookup".into(),
    )
    .unwrap();
    let flags = payloads
        .iter()
        .map(|p| {
            (
                p.id(),
                MessageFlags {
                    truncated: true,
                    projection: Some(MessageProjectionDirective {
                        policy_version: 3,
                        entries: vec![ProjectionActionEntry {
                            message_id: p.id(),
                            target: ProjectionTarget::Message,
                            action: ProjectionAction::OmitToolExchange {
                                anchor_message_id: anchor,
                                group_id: "group".into(),
                                tool_call_ids: vec!["call".into()],
                            },
                        }],
                        archive_group: Some(ArchiveGroupLink {
                            group_id: "group".into(),
                            anchor_message_id: anchor,
                        }),
                        archive_catalog: (p.id() == anchor).then(|| catalog.clone()),
                        archive_roots: if p.id() == anchor {
                            vec![ArchiveRoot {
                                source_thread_id: "source".into(),
                                first_anchor: anchor,
                                last_anchor: anchor,
                                committed_tail: anchor,
                            }]
                        } else {
                            vec![]
                        },
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
        })
        .collect();
    (payloads, flags)
}

#[test]
fn test_archive_directive_roundtrip_and_legacy_defaults() {
    let (_, flags) = archived_history();
    let directive = flags
        .values()
        .find_map(|f| {
            f.projection
                .as_ref()
                .filter(|p| p.archive_catalog.is_some())
        })
        .unwrap();
    let encoded = serde_json::to_string(directive).unwrap();
    assert_eq!(
        serde_json::from_str::<MessageProjectionDirective>(&encoded).unwrap(),
        *directive
    );
    let old: MessageProjectionDirective =
        serde_json::from_str(r#"{"policy_version":2,"entries":[]}"#).unwrap();
    assert_eq!(old.format_version, 1);
    assert!(old.archive_catalog.is_none());
    assert!(old.archive_roots.is_empty());
    assert!(!old.recovery_only, "旧格式未授予仅恢复语义");
    let recovery = MessageProjectionDirective {
        recovery_only: true,
        ..Default::default()
    };
    let encoded = serde_json::to_string(&recovery).unwrap();
    assert_eq!(
        serde_json::from_str::<MessageProjectionDirective>(&encoded).unwrap(),
        recovery
    );
}

#[test]
fn test_archive_fork_rewrites_scope_all_members_and_content_identity() {
    let (payloads, flags) = archived_history();
    let old = flags[&payloads[1].id()]
        .projection
        .as_ref()
        .unwrap()
        .archive_catalog
        .as_ref()
        .unwrap();
    let forked = remap_fork_history(&"target".into(), &payloads, &flags, MessageId::new).unwrap();
    let new_anchor = forked.payloads[1].id();
    let projection = forked.flags[&new_anchor].projection.as_ref().unwrap();
    let catalog = projection.archive_catalog.as_ref().unwrap();
    assert_eq!(catalog.source_thread_id, "target");
    assert_eq!(catalog.anchor_message_id, new_anchor);
    assert_eq!(catalog.members[0].message_id, forked.payloads[0].id());
    assert_ne!(
        catalog.members[0].content_digest,
        old.members[0].content_digest
    );
    assert_ne!(catalog.reference().unwrap(), old.reference().unwrap());
    assert_eq!(projection.archive_roots[0].source_thread_id, "target");
    assert_eq!(projection.archive_roots[0].committed_tail, new_anchor);
    for flags in forked.flags.values() {
        let projection = flags.projection.as_ref().unwrap();
        assert_eq!(
            projection.archive_group.as_ref().unwrap().anchor_message_id,
            new_anchor
        );
        assert!(
            matches!(&projection.entries[0].action, ProjectionAction::OmitToolExchange{anchor_message_id,..} if *anchor_message_id==new_anchor)
        );
    }
}

#[test]
fn test_archive_fork_rejects_missing_canonical_dependencies() {
    let (payloads, flags) = archived_history();
    let error =
        remap_fork_history(&"target".into(), &payloads[..1], &flags, MessageId::new).unwrap_err();
    assert!(matches!(error, HistoryError::InvalidArchive { .. }));
}

#[test]
fn test_archive_fork_rejects_unknown_or_corrupt_source_before_allocating_ids() {
    let (payloads, original_flags) = archived_history();
    let anchor = payloads[1].id();
    for corruption in ["version", "digest", "length", "chunk", "body"] {
        let mut flags = original_flags.clone();
        let mut source = payloads.clone();
        let catalog = flags
            .get_mut(&anchor)
            .unwrap()
            .projection
            .as_mut()
            .unwrap()
            .archive_catalog
            .as_mut()
            .unwrap();
        match corruption {
            "version" => catalog.format_version = 99,
            "digest" => catalog.members[0].content_digest = "0".repeat(64),
            "length" => catalog.members[0].canonical_bytes += 1,
            "chunk" => catalog.members[0].chunk_digests[0] = "0".repeat(64),
            "body" => {
                source[1] = PersistedPayload::Message(
                    BaseMessage::tool_result("call", "changed canonical result")
                        .with_message_id(anchor),
                );
            }
            _ => unreachable!(),
        }
        let error = remap_fork_history(&"target".into(), &source, &flags, || {
            panic!("invalid {corruption} must fail before allocating target identities")
        })
        .unwrap_err();
        assert!(
            matches!(error, HistoryError::InvalidArchive { id } if id == anchor),
            "{corruption} must not be reissued as a valid target archive"
        );
    }
}

#[test]
fn test_archive_fork_rejects_target_scope_that_exceeds_complete_metadata_budget() {
    use crate::context_archive::{validate_projection_metadata, ARCHIVE_MAX_METADATA_BYTES};
    let (payloads, mut flags) = archived_history();
    let anchor = payloads[1].id();
    let projection = flags.get_mut(&anchor).unwrap().projection.as_mut().unwrap();
    projection.archive_catalog.as_mut().unwrap().receipt.clear();
    let size = serde_json::to_vec(projection).unwrap().len();
    projection.archive_catalog.as_mut().unwrap().receipt =
        "x".repeat(ARCHIVE_MAX_METADATA_BYTES - size);
    assert_eq!(
        serde_json::to_vec(projection).unwrap().len(),
        ARCHIVE_MAX_METADATA_BYTES
    );
    validate_projection_metadata(projection).unwrap();
    let same_size = remap_fork_history(&"target".into(), &payloads, &flags, MessageId::new);
    assert!(
        same_size.is_ok(),
        "equal-width source/target scopes still fit"
    );
    let longer = remap_fork_history(&"target".repeat(20), &payloads, &flags, MessageId::new);
    assert!(
        matches!(longer, Err(HistoryError::InvalidArchive { .. })),
        "target scope must not make the stored complete directive unreadable"
    );
}

#[test]
fn test_rewind_removing_anchor_revokes_entire_surviving_exchange() {
    let (payloads, flags) = archived_history();
    let kept = vec![(payloads[0].id(), flags[&payloads[0].id()].clone(), false)];
    let updates = rewind_projection_updates(&kept, &[]);
    assert_eq!(updates.len(), 1);
    let projection = updates[0].1.projection.as_ref().unwrap();
    assert!(projection.entries.is_empty());
    assert!(projection.archive_group.is_none());
}

#[test]
fn test_rewind_removing_replacement_dependency_revokes_members_and_catalog() {
    let (payloads, mut flags) = archived_history();
    flags
        .get_mut(&payloads[1].id())
        .unwrap()
        .projection
        .as_mut()
        .unwrap()
        .archive_catalog
        .as_mut()
        .unwrap()
        .dependencies
        .push(MessageId::new());
    let kept = payloads
        .iter()
        .map(|p| (p.id(), flags[&p.id()].clone(), false))
        .collect::<Vec<_>>();
    let updates = rewind_projection_updates(&kept, &[]);
    assert_eq!(updates.len(), 2);
    for (_, flags) in updates {
        let p = flags.projection.unwrap();
        assert!(p.entries.is_empty());
        assert!(p.archive_catalog.is_none());
        assert!(p.archive_group.is_none());
        assert!(p.archive_roots.is_empty());
    }
}

#[test]
fn test_rewind_full_exclusion_retains_valid_catalog_identity() {
    let (payloads, mut flags) = archived_history();
    let anchor = payloads[1].id();
    let old_catalog = flags[&anchor]
        .projection
        .as_ref()
        .unwrap()
        .archive_catalog
        .clone();
    for flags in flags.values_mut() {
        flags.excluded = true;
        let p = flags.projection.as_mut().unwrap();
        p.entries.clear();
        p.superseded_by = Some(MessageId::new());
    }
    let kept = payloads
        .iter()
        .map(|p| (p.id(), flags[&p.id()].clone(), false))
        .collect::<Vec<_>>();
    let updates = rewind_projection_updates(&kept, &[payloads[0].id(), anchor]);
    let restored = &updates.iter().find(|(id, _)| *id == anchor).unwrap().1;
    assert!(!restored.excluded);
    assert_eq!(
        restored.projection.as_ref().unwrap().archive_catalog,
        old_catalog
    );
    assert!(restored
        .projection
        .as_ref()
        .unwrap()
        .superseded_by
        .is_none());
}

#[test]
fn test_rewind_shared_ai_preserves_other_complete_archive_group() {
    let ai = PersistedPayload::Message(BaseMessage::ai_with_tool_calls(
        "conclusion",
        vec![
            crate::messages::ToolCallRequest::new("a", "Read", serde_json::json!({"path":"a"})),
            crate::messages::ToolCallRequest::new("b", "Read", serde_json::json!({"path":"b"})),
        ],
    ));
    let a = PersistedPayload::Message(BaseMessage::tool_result("a", "a data"));
    let b = PersistedPayload::Message(BaseMessage::tool_result("b", "b data"));
    let ca = build_catalog(
        "source".into(),
        "a".into(),
        a.id(),
        &[ai.clone(), a.clone()],
        "a completed".into(),
    )
    .unwrap();
    let cb = build_catalog(
        "source".into(),
        "b".into(),
        b.id(),
        &[ai.clone(), b.clone()],
        "b completed".into(),
    )
    .unwrap();
    let action = |message_id, anchor, group: &str| ProjectionActionEntry {
        message_id,
        target: ProjectionTarget::ToolCall {
            tool_call_id: group.into(),
        },
        action: ProjectionAction::OmitToolExchange {
            anchor_message_id: anchor,
            group_id: group.into(),
            tool_call_ids: vec![group.into()],
        },
    };
    let mut ai_projection = MessageProjectionDirective {
        policy_version: 3,
        entries: vec![action(ai.id(), a.id(), "a"), action(ai.id(), b.id(), "b")],
        ..Default::default()
    };
    ai_projection.archive_group = Some(ArchiveGroupLink {
        group_id: "b".into(),
        anchor_message_id: b.id(),
    });
    let kept = vec![
        (
            ai.id(),
            MessageFlags {
                truncated: true,
                projection: Some(ai_projection),
                ..Default::default()
            },
            false,
        ),
        (
            a.id(),
            MessageFlags {
                truncated: true,
                projection: Some(MessageProjectionDirective {
                    policy_version: 3,
                    archive_catalog: Some(ca.clone()),
                    entries: vec![action(a.id(), a.id(), "a")],
                    ..Default::default()
                }),
                ..Default::default()
            },
            false,
        ),
    ];
    let updates = rewind_projection_updates(&kept, &[]);
    let ai_restored = updates
        .iter()
        .find(|(id, _)| *id == ai.id())
        .unwrap()
        .1
        .projection
        .as_ref()
        .unwrap();
    assert_eq!(ai_restored.entries.len(), 1);
    assert!(
        matches!(&ai_restored.entries[0].action,ProjectionAction::OmitToolExchange {anchor_message_id,..} if *anchor_message_id==a.id())
    );
    let mut complete_flags: HashMap<_, _> = kept
        .iter()
        .map(|(id, flags, _)| (*id, flags.clone()))
        .collect();
    complete_flags.insert(
        b.id(),
        MessageFlags {
            truncated: true,
            projection: Some(MessageProjectionDirective {
                policy_version: 3,
                archive_catalog: Some(cb),
                entries: vec![action(b.id(), b.id(), "b")],
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    let complete_fork = remap_fork_history(
        &"target".into(),
        &[ai.clone(), a.clone(), b.clone()],
        &complete_flags,
        MessageId::new,
    )
    .unwrap();
    let mapped_ai = complete_fork.flags[&complete_fork.payloads[0].id()]
        .projection
        .as_ref()
        .unwrap();
    assert_eq!(mapped_ai.entries.len(), 2);
    assert!(
        matches!(&mapped_ai.entries[0].action,ProjectionAction::OmitToolExchange {anchor_message_id,..} if *anchor_message_id==complete_fork.payloads[1].id())
    );
    assert!(
        matches!(&mapped_ai.entries[1].action,ProjectionAction::OmitToolExchange {anchor_message_id,..} if *anchor_message_id==complete_fork.payloads[2].id())
    );
    let forked = remap_fork_history(
        &"target".into(),
        &[ai.clone(), a.clone()],
        &kept.iter().map(|(id, f, _)| (*id, f.clone())).collect(),
        MessageId::new,
    );
    assert!(
        matches!(forked, Err(HistoryError::InvalidArchive { .. })),
        "尚未应用rewind撤销的半组不得fork"
    );
}

#[test]
fn test_rewind_missing_member_action_revokes_entire_omission_group() {
    let (payloads, mut flags) = archived_history();
    flags
        .get_mut(&payloads[0].id())
        .unwrap()
        .projection
        .as_mut()
        .unwrap()
        .entries
        .clear();
    let kept = payloads
        .iter()
        .map(|p| (p.id(), flags[&p.id()].clone(), false))
        .collect::<Vec<_>>();
    let updates = rewind_projection_updates(&kept, &[]);
    assert!(updates
        .iter()
        .all(|(_, f)| f.projection.as_ref().unwrap().entries.is_empty()));
    assert!(updates
        .iter()
        .find(|(id, _)| *id == payloads[1].id())
        .unwrap()
        .1
        .projection
        .as_ref()
        .unwrap()
        .archive_catalog
        .is_none());
}

#[test]
fn test_fork_clears_typed_archive_read_preview_without_rewriting_canonical_input() {
    let literal = "peri-archive:source-reference";
    let message = BaseMessage::tool_result("read", literal);
    let id = message.id();
    let flags = HashMap::from([(
        id,
        MessageFlags {
            truncated: true,
            projection: Some(MessageProjectionDirective {
                policy_version: 3,
                entries: vec![ProjectionActionEntry {
                    message_id: id,
                    target: ProjectionTarget::Message,
                    action: ProjectionAction::PreviewArchiveRead {
                        archive_ref: literal.into(),
                        keep_head: 10,
                        keep_tail: 0,
                    },
                }],
                ..Default::default()
            }),
            ..Default::default()
        },
    )]);
    let forked = remap_fork_history(
        &"target".into(),
        &[PersistedPayload::Message(message)],
        &flags,
        MessageId::new,
    )
    .unwrap();
    assert_eq!(forked.payloads[0].as_message().unwrap().content(), literal);
    assert!(forked.flags[&forked.payloads[0].id()]
        .projection
        .as_ref()
        .unwrap()
        .entries
        .is_empty());
}

/// [回归测试] archive-only 的持久事实不能借 refork 修复成有效历史。
#[test]
fn test_archive_fork_rejects_active_recovery_only_payload_before_allocating_ids() {
    let (payloads, mut flags) = archived_history();
    for flag in flags.values_mut() {
        flag.projection.as_mut().unwrap().recovery_only = true;
    }
    let mut allocations = 0;
    let error = remap_fork_history(&"target".into(), &payloads, &flags, || {
        allocations += 1;
        MessageId::new()
    })
    .unwrap_err();
    assert_eq!(
        error,
        HistoryError::InvalidArchive {
            id: payloads[0].id()
        }
    );
    assert_eq!(allocations, 0);
}
