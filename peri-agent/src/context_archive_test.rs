use super::*;
use crate::session::test_resources::TestSession;
use peri_acp_types::{
    messages::BaseMessage,
    projection::MessageProjectionDirective,
    session_resources::RewindBoundary,
    store::{serialize_persisted_payload, MessageFlags, PersistedPayload},
};

async fn committed_archive(text: &str) -> (TestSession, PersistedPayload, ArchiveCatalog) {
    let session = TestSession::open().await;
    let payload = PersistedPayload::Message(BaseMessage::ai(text));
    session
        .resources
        .append_history(&session.thread_id, std::slice::from_ref(&payload))
        .await
        .unwrap();
    let catalog = build_catalog(
        session.thread_id.clone(),
        "exchange-1".into(),
        payload.id(),
        std::slice::from_ref(&payload),
        "Historical tool data; execution status unknown.".into(),
    )
    .unwrap();
    let reader = ContextArchiveReader::new(session.resources(), session.thread_id());
    reader.verify_catalog(&catalog).await.unwrap();
    session
        .resources
        .apply_message_projections(
            &session.thread_id,
            &[(
                payload.id(),
                MessageFlags {
                    truncated: true,
                    excluded: false,
                    projection: Some(MessageProjectionDirective {
                        archive_catalog: Some(catalog.clone()),
                        ..Default::default()
                    }),
                },
            )],
        )
        .await
        .unwrap();
    (session, payload, catalog)
}

async fn collect(
    reader: &ContextArchiveReader,
    archive_ref: String,
    budget_tokens: u32,
) -> Vec<u8> {
    let mut cursor = None;
    let mut bytes = Vec::new();
    for _ in 0..1000 {
        let result = reader
            .read(ArchiveReadRequest {
                archive_ref: archive_ref.clone(),
                cursor,
                budget_tokens: Some(budget_tokens),
            })
            .await
            .unwrap();
        assert!(serde_json::to_vec(&result).unwrap().len() <= budget_tokens as usize * 4);
        for entry in result.entries {
            assert_eq!(entry.offset, bytes.len() as u64);
            assert!(!entry.complete || entry.total_bytes == entry.data.len() as u64);
            match entry.encoding.as_str() {
                "utf8" => bytes.extend_from_slice(entry.data.as_bytes()),
                "hex" => bytes.extend(
                    (0..entry.data.len())
                        .step_by(2)
                        .map(|i| u8::from_str_radix(&entry.data[i..i + 2], 16).unwrap()),
                ),
                _ => panic!("unexpected encoding"),
            }
        }
        if result.complete {
            return bytes;
        }
        cursor = result.next_cursor;
        assert!(cursor.is_some());
    }
    panic!("archive pagination failed to terminate")
}

#[tokio::test]
async fn archive_cold_reader_returns_exact_original_in_bounded_pages_without_mutation() {
    let text = "历史工具输入，不执行。\n\"long quoted code\": { x: 1 } ".repeat(450);
    let (session, payload, catalog) = committed_archive(&text).await;
    let before = session
        .resources
        .load_session_history(&session.thread_id)
        .await
        .unwrap();
    let cold = ContextArchiveReader::new(session.read_only_resources().await, session.thread_id());
    let recovered = collect(&cold, catalog.member_ref(0).unwrap(), 2048).await;
    assert_eq!(
        recovered,
        serialize_persisted_payload(&payload).unwrap().as_bytes()
    );
    assert_eq!(
        session
            .resources
            .load_session_history(&session.thread_id)
            .await
            .unwrap()
            .len(),
        before.len()
    );
}

#[tokio::test]
async fn archive_cross_session_forgery_digest_mismatch_and_rewind_are_explicit_errors() {
    let (session, payload, catalog) = committed_archive(&"data".repeat(1500)).await;
    let foreign = TestSession::open().await;
    let reader = ContextArchiveReader::new(foreign.resources(), foreign.thread_id());
    let request = ArchiveReadRequest {
        archive_ref: catalog.member_ref(0).unwrap(),
        cursor: None,
        budget_tokens: None,
    };
    assert!(matches!(
        reader.read(request.clone()).await,
        Err(ArchiveError::Forbidden)
    ));
    let mut forged = ArchiveReference::decode(&request.archive_ref).unwrap();
    if let ArchiveReference::Catalog { digest, .. } = &mut forged {
        *digest = "0".repeat(64);
    }
    let reader = ContextArchiveReader::new(session.resources(), session.thread_id());
    assert!(matches!(
        reader
            .read(ArchiveReadRequest {
                archive_ref: forged.encode().unwrap(),
                ..request.clone()
            })
            .await,
        Err(ArchiveError::ContentMismatch)
    ));
    session
        .resources
        .rewind_history(
            &session.thread_id,
            RewindBoundary::RemoveFrom(payload.id()),
            &[],
        )
        .await
        .unwrap();
    assert!(matches!(
        reader.read(request).await,
        Err(ArchiveError::NotFound)
    ));
}

#[tokio::test]
async fn archive_catalog_discovery_survives_exclusion_and_summary_without_ids() {
    let (session, payload, catalog) = committed_archive("historical canonical data").await;
    let summary = BaseMessage::ai("Summary deliberately contains no identifiers.");
    session
        .resources
        .apply_compaction(
            &session.thread_id,
            &peri_acp_types::store::CompactionChange {
                flag_updates: vec![(
                    payload.id(),
                    MessageFlags {
                        excluded: true,
                        truncated: false,
                        projection: Some(MessageProjectionDirective {
                            superseded_by: Some(summary.id()),
                            archive_catalog: Some(catalog.clone()),
                            ..Default::default()
                        }),
                    },
                )],
                appended_messages: vec![summary],
            },
        )
        .await
        .unwrap();
    let cold = ContextArchiveReader::new(session.read_only_resources().await, session.thread_id());
    let root = ArchiveRoot {
        source_thread_id: session.thread_id(),
        first_anchor: payload.id(),
        last_anchor: payload.id(),
        committed_tail: payload.id(),
    };
    let page = cold
        .read(ArchiveReadRequest {
            archive_ref: root.reference().unwrap(),
            cursor: None,
            budget_tokens: None,
        })
        .await
        .unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(
        collect(&cold, page.entries[0].archive_ref.clone(), 2048).await,
        serialize_persisted_payload(&payload).unwrap().as_bytes()
    );
}

#[tokio::test]
async fn archive_inherited_grant_is_frozen_and_does_not_authorize_later_parent_content() {
    use peri_acp_types::session_resources::{
        BindingState, ChildSnapshot, FrozenState, NewSession, NewSessionMeta,
    };
    let (parent, payload, catalog) = committed_archive("authorized original").await;
    let root = ArchiveRoot {
        source_thread_id: parent.thread_id(),
        first_anchor: payload.id(),
        last_anchor: payload.id(),
        committed_tail: payload.id(),
    };
    let mut snapshot = parent
        .resources
        .load_session_snapshot(&parent.thread_id)
        .await
        .unwrap();
    snapshot
        .flags
        .get_mut(&payload.id())
        .unwrap()
        .projection
        .as_mut()
        .unwrap()
        .archive_roots = vec![root];
    let BindingState::Bound(binding) = snapshot.binding else {
        panic!("bound fixture")
    };
    let FrozenState::Present(frozen) = snapshot.frozen else {
        panic!("frozen fixture")
    };
    let child_id = uuid::Uuid::now_v7().to_string();
    parent
        .resources
        .save_child(&ChildSnapshot {
            target: NewSession {
                thread_id: child_id.clone(),
                created_at: peri_time::now_utc_rfc3339(),
                meta: NewSessionMeta {
                    title: None,
                    cwd: snapshot.meta.cwd,
                    parent_thread_id: Some(parent.thread_id()),
                    hidden: true,
                    cancel_policy: Default::default(),
                    snapshot_at_message_id: Some(payload.id()),
                },
                binding,
                frozen,
            },
            parent_id: parent.thread_id(),
            root_id: parent.thread_id(),
            inherited: peri_acp_types::store::InheritedContext {
                payloads: snapshot.payloads,
                flags: snapshot.flags,
            },
        })
        .await
        .unwrap();
    let child = ContextArchiveReader::new(parent.read_only_resources().await, child_id);
    assert_eq!(
        collect(&child, catalog.member_ref(0).unwrap(), 2048).await,
        serialize_persisted_payload(&payload).unwrap().as_bytes()
    );
    let later = PersistedPayload::Message(BaseMessage::ai("not inherited"));
    parent
        .resources
        .append_history(&parent.thread_id, std::slice::from_ref(&later))
        .await
        .unwrap();
    let later_catalog = build_catalog(
        parent.thread_id(),
        "later".into(),
        later.id(),
        &[later],
        "later".into(),
    )
    .unwrap();
    assert!(matches!(
        child
            .read(ArchiveReadRequest {
                archive_ref: later_catalog.member_ref(0).unwrap(),
                cursor: None,
                budget_tokens: None
            })
            .await,
        Err(ArchiveError::Forbidden)
    ));
}

#[tokio::test]
async fn archive_fork_recomputes_content_identity_and_rejects_literal_source_reference() {
    use peri_acp_types::session_resources::{
        BindingState, ForkSnapshot, FrozenState, NewSession, NewSessionMeta,
    };
    let (source, payload, catalog) = committed_archive("exact old tool input").await;
    let snapshot = source
        .resources
        .load_session_snapshot(&source.thread_id)
        .await
        .unwrap();
    let target = uuid::Uuid::now_v7().to_string();
    let remapped = peri_acp_types::store::history::remap_fork_history(
        &target,
        &snapshot.payloads,
        &snapshot.flags,
        MessageId::new,
    )
    .unwrap();
    let BindingState::Bound(binding) = snapshot.binding else {
        panic!("bound fixture")
    };
    let FrozenState::Present(frozen) = snapshot.frozen else {
        panic!("frozen fixture")
    };
    let new_payload = remapped.payloads[0].clone();
    let new_catalog = remapped.flags[&new_payload.id()]
        .projection
        .as_ref()
        .unwrap()
        .archive_catalog
        .clone()
        .unwrap();
    assert_ne!(
        new_catalog.members[0].content_digest,
        catalog.members[0].content_digest
    );
    assert_ne!(new_payload.id(), payload.id());
    source
        .resources
        .save_fork(&ForkSnapshot {
            target: NewSession {
                thread_id: target.clone(),
                created_at: peri_time::now_utc_rfc3339(),
                meta: NewSessionMeta {
                    title: None,
                    cwd: snapshot.meta.cwd,
                    parent_thread_id: None,
                    hidden: false,
                    cancel_policy: Default::default(),
                    snapshot_at_message_id: None,
                },
                binding,
                frozen,
            },
            source_id: source.thread_id(),
            payloads: remapped.payloads,
            flags: remapped.flags,
        })
        .await
        .unwrap();
    let reader = ContextArchiveReader::new(source.read_only_resources().await, target);
    assert_eq!(
        collect(&reader, new_catalog.member_ref(0).unwrap(), 2048).await,
        serialize_persisted_payload(&new_payload)
            .unwrap()
            .as_bytes()
    );
    assert!(matches!(
        reader
            .read(ArchiveReadRequest {
                archive_ref: catalog.member_ref(0).unwrap(),
                cursor: None,
                budget_tokens: None
            })
            .await,
        Err(ArchiveError::Forbidden)
    ));
}

#[tokio::test]
async fn archive_recovers_original_tool_arguments_and_embedded_media_not_mutable_urls() {
    use peri_acp_types::messages::{ContentBlock, ImageSource, MessageContent};
    let session = TestSession::open().await;
    let payload = PersistedPayload::Message(BaseMessage::ai(MessageContent::Blocks(vec![
        ContentBlock::ToolUse {
            id: "write-1".into(),
            name: "Write".into(),
            input: serde_json::json!({"file_path":"historical.rs","content":"old code\n".repeat(2000)}),
        },
        ContentBlock::Image {
            source: ImageSource::Base64 {
                media_type: "image/png".into(),
                data: "aGVsbG8=".repeat(2000),
            },
        },
    ])));
    session
        .resources
        .append_history(&session.thread_id, std::slice::from_ref(&payload))
        .await
        .unwrap();
    let catalog = build_catalog(
        session.thread_id(),
        "mixed".into(),
        payload.id(),
        std::slice::from_ref(&payload),
        "historical data".into(),
    )
    .unwrap();
    let reader = ContextArchiveReader::new(session.resources(), session.thread_id());
    reader.verify_catalog(&catalog).await.unwrap();
    session
        .resources
        .apply_message_projections(
            &session.thread_id,
            &[(
                payload.id(),
                MessageFlags {
                    truncated: true,
                    excluded: false,
                    projection: Some(MessageProjectionDirective {
                        archive_catalog: Some(catalog.clone()),
                        ..Default::default()
                    }),
                },
            )],
        )
        .await
        .unwrap();
    assert_eq!(
        collect(&reader, catalog.member_ref(0).unwrap(), 1024).await,
        serialize_persisted_payload(&payload).unwrap().as_bytes()
    );
    let remote = PersistedPayload::Message(BaseMessage::ai(MessageContent::Blocks(vec![
        ContentBlock::Image {
            source: ImageSource::Url {
                url: "https://example.invalid/changes.png".into(),
            },
        },
    ])));
    assert!(matches!(
        build_catalog(
            session.thread_id(),
            "remote".into(),
            remote.id(),
            &[remote],
            "history".into()
        ),
        Err(ArchiveError::Unsupported)
    ));
}
