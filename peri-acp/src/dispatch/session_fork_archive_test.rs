//! Independent forks own the original bytes of frozen inherited archives.
use super::*;
use peri_acp_types::{
    context_archive::{
        build_catalog, ArchiveError, ArchiveReadRequest, ArchiveRoot, ContextArchiveReadPort,
    },
    session_resources::{BindingState, ChildSnapshot, FrozenState},
    store::InheritedContext,
};
use peri_agent::context_archive::ContextArchiveReader;

#[tokio::test]
async fn own_fork_rejects_unknown_catalog_or_changed_body_without_creating_target() {
    let temp = tempfile::tempdir().unwrap();
    let resources = open_facade(&temp).await;
    let workspace = resources.resolve_workspace(temp.path()).await.unwrap();
    for corruption in ["version", "body", "valid"] {
        let source_id = format!("own-archive-{corruption}");
        create_source(&resources, &source_id, &temp.path().to_string_lossy()).await;
        let mut pair = vec![
            PersistedPayload::Message(BaseMessage::ai_with_tool_calls(
                "",
                vec![ToolCallRequest::new(
                    "read",
                    "Read",
                    serde_json::json!({"path":"a"}),
                )],
            )),
            PersistedPayload::Message(BaseMessage::tool_result("read", "original canonical")),
        ];
        let anchor = pair[1].id();
        let mut catalog = build_catalog(
            source_id.clone(),
            "own-group".into(),
            anchor,
            &pair,
            "Closed read.".into(),
        )
        .unwrap();
        match corruption {
            "version" => catalog.format_version = 99,
            "body" => {
                pair[1] = PersistedPayload::Message(
                    BaseMessage::tool_result("read", "changed canonical").with_message_id(anchor),
                );
            }
            "valid" => {}
            _ => unreachable!(),
        }
        resources.append_history(&source_id, &pair).await.unwrap();
        resources
            .apply_message_projections(
                &source_id,
                &[(
                    anchor,
                    MessageFlags {
                        projection: Some(MessageProjectionDirective {
                            policy_version: 3,
                            archive_catalog: Some(catalog),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                )],
            )
            .await
            .unwrap();
        let source = load_fork_source(&resources, &source_id).await.unwrap();
        let before = session_ids(&resources).await;
        let result = fork_bound_session(
            &resources,
            &source,
            &workspace,
            chrono::Utc::now().to_rfc3339(),
        )
        .await;
        if corruption == "valid" {
            let (target, _) = result.unwrap();
            let copied = resources.load_session_snapshot(&target).await.unwrap();
            let copied_catalog = copied
                .flags
                .values()
                .find_map(|flags| flags.projection.as_ref()?.archive_catalog.as_ref())
                .unwrap();
            assert_eq!(copied_catalog.source_thread_id, target);
            assert_eq!(copied_catalog.format_version, 1);
        } else {
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("archive dependencies"));
            assert_eq!(
                session_ids(&resources).await,
                before,
                "{corruption}: no target saved"
            );
        }
        let original = resources.load_session_snapshot(&source_id).await.unwrap();
        let canonical = |payloads: &[PersistedPayload]| {
            payloads
                .iter()
                .map(|payload| peri_acp_types::store::serialize_persisted_payload(payload).unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            canonical(&original.payloads),
            canonical(&pair),
            "fork must leave its source unchanged"
        );
    }
}

#[tokio::test]
async fn child_full_fork_materializes_only_frozen_authorized_archives_into_target_scope() {
    let temp = tempfile::tempdir().unwrap();
    let resources = open_facade(&temp).await;
    create_source(&resources, SOURCE, &temp.path().to_string_lossy()).await;
    let source_id = SOURCE.to_string();
    let pair = vec![
        PersistedPayload::Message(BaseMessage::ai_with_tool_calls(
            "",
            vec![ToolCallRequest::new(
                "read-old",
                "Read",
                serde_json::json!({"path":"old.rs","input":"immutable old input"}),
            )],
        )),
        PersistedPayload::Message(BaseMessage::tool_result("read-old", "immutable old result")),
    ];
    resources.append_history(&source_id, &pair).await.unwrap();
    let anchor = pair[1].id();
    let catalog = build_catalog(
        source_id.clone(),
        "parent-group".into(),
        anchor,
        &pair,
        "Closed historical read.".into(),
    )
    .unwrap();
    let original_ref = catalog.member_ref(0).unwrap();
    let root = ArchiveRoot {
        source_thread_id: source_id.clone(),
        first_anchor: anchor,
        last_anchor: anchor,
        committed_tail: anchor,
    };
    let parent_summary = BaseMessage::human("Parent summary without identifiers.");
    let summary_flags = MessageFlags {
        projection: Some(MessageProjectionDirective {
            policy_version: 3,
            archive_roots: vec![root.clone()],
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut flag_updates = pair
        .iter()
        .map(|payload| {
            (
                payload.id(),
                MessageFlags {
                    excluded: true,
                    projection: Some(MessageProjectionDirective {
                        policy_version: 3,
                        superseded_by: Some(parent_summary.id()),
                        archive_catalog: (payload.id() == anchor).then(|| catalog.clone()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
        })
        .collect::<Vec<_>>();
    flag_updates.push((parent_summary.id(), summary_flags.clone()));
    resources
        .apply_compaction(
            &source_id,
            &CompactionChange {
                flag_updates,
                appended_messages: vec![parent_summary.clone()],
            },
        )
        .await
        .unwrap();
    let parent = resources.load_active_snapshot(&source_id).await.unwrap();
    assert_eq!(
        parent.payloads.len(),
        1,
        "Full has released the archived original bodies"
    );
    let BindingState::Bound(binding) = parent.binding else {
        panic!("bound")
    };
    let FrozenState::Present(frozen) = parent.frozen else {
        panic!("frozen")
    };
    let child = "archive-child".to_string();
    resources
        .save_child(&ChildSnapshot {
            target: NewSession {
                thread_id: child.clone(),
                created_at: chrono::Utc::now().to_rfc3339(),
                meta: NewSessionMeta {
                    title: None,
                    cwd: parent.meta.cwd,
                    parent_thread_id: Some(source_id.clone()),
                    hidden: true,
                    cancel_policy: CancelPolicy::default(),
                    snapshot_at_message_id: Some(parent_summary.id()),
                },
                binding,
                frozen,
            },
            parent_id: source_id.clone(),
            root_id: source_id.clone(),
            inherited: InheritedContext {
                payloads: parent.payloads,
                flags: parent.flags,
            },
        })
        .await
        .unwrap();
    let question = BaseMessage::human("Child work.");
    resources
        .append_history(&child, &[PersistedPayload::Message(question.clone())])
        .await
        .unwrap();
    let child_summary = BaseMessage::human("Child Full summary without identifiers.");
    resources
        .apply_compaction(
            &child,
            &CompactionChange {
                flag_updates: vec![
                    (
                        question.id(),
                        MessageFlags {
                            excluded: true,
                            projection: Some(MessageProjectionDirective {
                                policy_version: 3,
                                superseded_by: Some(child_summary.id()),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    ),
                    (child_summary.id(), summary_flags),
                ],
                appended_messages: vec![child_summary],
            },
        )
        .await
        .unwrap();
    // Parent additions after the frozen child boundary are not inherited or copied.
    let secret = PersistedPayload::Message(BaseMessage::human("LATER_PARENT_SECRET"));
    resources
        .append_history(&source_id, &[secret])
        .await
        .unwrap();
    let source = load_fork_source(&resources, &child).await.unwrap();
    let workspace = resources.resolve_workspace(temp.path()).await.unwrap();
    let (target, active) = fork_bound_session(
        &resources,
        &source,
        &workspace,
        chrono::Utc::now().to_rfc3339(),
    )
    .await
    .unwrap();
    assert_eq!(
        active.len(),
        2,
        "materialized archived bodies must remain excluded"
    );
    let fork = resources.load_session_snapshot(&target).await.unwrap();
    assert!(fork.inherited.payloads.is_empty());
    assert!(fork
        .payloads
        .iter()
        .filter_map(PersistedPayload::as_message)
        .all(|message| !message.content().contains("LATER_PARENT_SECRET")));
    let copied_catalog = fork
        .flags
        .values()
        .find_map(|flags| {
            flags
                .projection
                .as_ref()
                .and_then(|p| p.archive_catalog.as_ref())
        })
        .unwrap();
    assert_eq!(copied_catalog.source_thread_id, target);
    assert!(copied_catalog
        .members
        .iter()
        .all(|member| fork.flags[&member.message_id].excluded));
    assert!(fork
        .flags
        .values()
        .flat_map(|flags| flags.projection.iter().flat_map(|p| &p.archive_roots))
        .all(|root| root.source_thread_id == target));
    let reader = ContextArchiveReader::new(resources.clone(), target.clone());
    let recovered = reader
        .read(ArchiveReadRequest {
            archive_ref: copied_catalog.member_ref(0).unwrap(),
            cursor: None,
            budget_tokens: None,
        })
        .await
        .unwrap();
    assert!(recovered
        .entries
        .iter()
        .any(|entry| entry.data.contains("immutable old input")));
    assert!(matches!(
        reader
            .read(ArchiveReadRequest {
                archive_ref: original_ref,
                cursor: None,
                budget_tokens: None
            })
            .await,
        Err(ArchiveError::Forbidden)
    ));
    let original_child = resources.load_session_snapshot(&child).await.unwrap();
    assert_eq!(
        original_child.inherited.payloads[0].id(),
        parent_summary.id(),
        "source frozen identity remains unchanged"
    );
}
