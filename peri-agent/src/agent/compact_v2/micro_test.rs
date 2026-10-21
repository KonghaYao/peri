//! Production planner → durable Micro commit → fresh restored model view.
use super::*;
use crate::agent::compact_v2::planner::{
    plan_micro_with_facts, ArchivePlanningScope, ContextPressure, MicroPlanningFacts,
};
use crate::agent::compact_v2::projection::{
    render_persisted_llm_view, ProjectionAction, ProjectionActionEntry, ProjectionTarget,
    ProviderCapabilities, PROJECTION_POLICY_VERSION,
};
use crate::messages::{BaseMessage, ToolCallRequest};
use crate::session::test_resources::{mock::MockSessionResources, TestSession};
use crate::tools::ContextRetention;

fn append_steps(transcript: &mut MessageTranscript) {
    transcript.append(BaseMessage::human("Keep the public interface unchanged."));
    for i in 0..6 {
        let call = format!("call-{i}");
        transcript.append(BaseMessage::ai_with_tool_calls(
            "",
            vec![ToolCallRequest::new(
                &call,
                "Read",
                serde_json::json!({"path":"historical.rs","body":"original".repeat(2000)}),
            )],
        ));
        transcript.append(BaseMessage::tool_result(
            &call,
            format!("original-{i}:{}", "source".repeat(4000)),
        ));
    }
}

fn exposed_plan(transcript: &MessageTranscript, session_id: String) -> MicroCompactPlan {
    let mut facts = MicroPlanningFacts {
        archive_scope: Some(ArchivePlanningScope {
            thread_id: session_id,
            committed_tail: transcript.entries().last().unwrap().id(),
        }),
        ..Default::default()
    };
    for entry in transcript.entries() {
        facts.exposed_message_ids.insert(entry.id());
        if let Some(message) = entry.as_message() {
            for call in message.tool_calls() {
                facts.closed_call_ids.insert(call.id.clone());
                facts
                    .tool_retention
                    .insert(call.name.to_lowercase(), ContextRetention::Recomputable);
            }
        }
    }
    plan_micro_with_facts(
        transcript,
        &CompactConfig {
            micro_compact_stale_steps: 0,
            ..Default::default()
        },
        &ContextPressure {
            estimated_tokens: 6000,
            context_window: 8000,
            output_reserve: 1000,
            predicted_tool_growth: 0,
            safety_buffer: 0,
            cache_hit_rate: 0.0,
        },
        &facts,
        &ProviderCapabilities::openai(),
    )
    .unwrap()
}

#[tokio::test]
async fn test_micro_cold_history_keeps_every_byte_without_exposure_evidence() {
    let store = MockSessionResources::new();
    let mut transcript = MessageTranscript::new().with_persistence(store.clone(), "cold".into());
    append_steps(&mut transcript);
    let revision = transcript.view_revision();
    assert_eq!(
        micro_compact(&mut transcript, &CompactConfig::default())
            .await
            .unwrap(),
        0
    );
    assert_eq!(transcript.view_revision(), revision);
    assert_eq!(store.compaction_calls(), 0);
    assert!(transcript
        .entries()
        .iter()
        .all(|entry| !transcript.flags(entry.id()).truncated));
}

#[tokio::test]
async fn test_micro_planner_commit_and_fresh_restore_share_one_view() {
    let bound = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    append_steps(&mut transcript);
    let plan = exposed_plan(&transcript, bound.thread_id.clone());
    assert!(!plan.catalogs.is_empty());
    assert!(plan.estimated_tokens_saved > 0);
    let revision = transcript.view_revision();
    let changed = apply_micro_plan(&mut transcript, &plan).await.unwrap();
    assert!(changed > 0);
    assert_ne!(transcript.view_revision(), revision);
    let hot = render_persisted_llm_view(&transcript, &ProviderCapabilities::openai()).unwrap();
    let fresh = bound.read_only_resources().await;
    let snapshot = fresh.load_session_snapshot(&bound.thread_id).await.unwrap();
    assert_eq!(snapshot.payloads.len(), 13, "原始消息不能被Micro删除");
    assert!(snapshot.payloads.iter().any(|p| p
        .as_message()
        .is_some_and(|m| m.content().contains(&"source".repeat(4000)))));
    let mut restored = MessageTranscript::new().with_own_payloads(snapshot.payloads);
    restored.set_flags_batch(snapshot.flags);
    let cold = render_persisted_llm_view(&restored, &ProviderCapabilities::openai()).unwrap();
    assert_eq!(
        serde_json::to_value(hot).unwrap(),
        serde_json::to_value(cold).unwrap()
    );
}

#[tokio::test]
async fn test_micro_late_result_is_not_part_of_an_earlier_plan() {
    let bound = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    append_steps(&mut transcript);
    let plan = exposed_plan(&transcript, bound.thread_id.clone());
    let late = BaseMessage::human("Late result remains a current obligation");
    transcript.append(late.clone());
    apply_micro_plan(&mut transcript, &plan).await.unwrap();
    assert_eq!(transcript.flags(late.id()), MessageFlags::default());
    let view = render_persisted_llm_view(&transcript, &ProviderCapabilities::openai()).unwrap();
    assert!(view.iter().any(|message| message.id() == late.id()));
}

#[tokio::test]
async fn test_micro_rejects_changed_source_flags_before_any_projection_write() {
    let bound = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    append_steps(&mut transcript);
    let plan = exposed_plan(&transcript, bound.thread_id.clone());
    let source = plan.actions[0].message_id;
    transcript.set_excluded(source, true);
    let revision = transcript.view_revision();
    let error = apply_micro_plan(&mut transcript, &plan).await.unwrap_err();
    assert!(error.to_string().contains("source changed"));
    assert_eq!(transcript.view_revision(), revision);
    assert!(transcript.flags(source).excluded);
    assert!(transcript
        .entries()
        .iter()
        .all(|entry| transcript.projection_ref(entry.id()).is_none()));
}

#[tokio::test]
async fn test_micro_rejects_replaced_source_content_with_the_same_id() {
    let bound = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    append_steps(&mut transcript);
    let plan = exposed_plan(&transcript, bound.thread_id.clone());
    let source = plan.actions[0].message_id;
    transcript.replace_by_id(BaseMessage::ai("replacement content").with_message_id(source));
    let revision = transcript.view_revision();
    let error = apply_micro_plan(&mut transcript, &plan).await.unwrap_err();
    assert!(error.to_string().contains("source changed"));
    assert_eq!(transcript.view_revision(), revision);
    assert!(transcript
        .entries()
        .iter()
        .all(|entry| transcript.projection_ref(entry.id()).is_none()));
}

#[tokio::test]
async fn test_micro_rewind_atomically_revokes_the_removed_anchor_in_hot_and_cold_views() {
    let bound = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    append_steps(&mut transcript);
    let keep = transcript.entries()[1].id();
    let plan = exposed_plan(&transcript, bound.thread_id.clone());
    apply_micro_plan(&mut transcript, &plan).await.unwrap();
    assert!(!transcript.projection_ref(keep).unwrap().entries.is_empty());
    transcript.rewind_to(keep).await.unwrap();
    assert_eq!(transcript.len(), 2);
    assert!(transcript.projection_ref(keep).unwrap().entries.is_empty());
    let snapshot = bound
        .read_only_resources()
        .await
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert_eq!(snapshot.payloads.len(), 2);
    assert_eq!(transcript.flags(keep), snapshot.flags[&keep]);
}

#[tokio::test]
async fn test_micro_unreadable_archive_rejects_before_committing_any_member() {
    let store = MockSessionResources::new();
    let mut transcript =
        MessageTranscript::new().with_persistence(store.clone(), "unsupported".into());
    append_steps(&mut transcript);
    let plan = exposed_plan(&transcript, "unsupported".into());
    let revision = transcript.view_revision();
    let error = apply_micro_plan(&mut transcript, &plan).await.unwrap_err();
    assert!(error.to_string().contains("recovery_unavailable"));
    assert_eq!(transcript.view_revision(), revision);
    assert_eq!(store.compaction_calls(), 0);
    assert!(transcript
        .entries()
        .iter()
        .all(|entry| !transcript.flags(entry.id()).truncated));
}

#[tokio::test]
async fn test_micro_known_batch_failure_preserves_previous_revision_and_flags() {
    let store = MockSessionResources::new();
    let mut transcript = MessageTranscript::new().with_persistence(store.clone(), "failure".into());
    let first = transcript.append(BaseMessage::tool_result("a", "long".repeat(1000)));
    let second = transcript.append(BaseMessage::tool_result("b", "long".repeat(1000)));
    transcript.flush_persistence().await.unwrap();
    let plan = MicroCompactPlan {
        policy_version: PROJECTION_POLICY_VERSION,
        actions: [first, second]
            .into_iter()
            .map(|message_id| ProjectionActionEntry {
                message_id,
                target: ProjectionTarget::Message,
                action: ProjectionAction::CompactToolResult {
                    keep_head: 20,
                    keep_tail: 10,
                    preserve_recovery_handle: true,
                },
            })
            .collect(),
        ..Default::default()
    };
    let revision = transcript.view_revision();
    store.fail_compaction();
    let error = apply_micro_plan(&mut transcript, &plan).await.unwrap_err();
    assert!(error.to_string().contains("injected compaction failure"));
    assert_eq!(transcript.view_revision(), revision);
    assert_eq!(store.compaction_calls(), 1, "成员必须一次批量提交");
    assert!(!transcript.flags(first).truncated && !transcript.flags(second).truncated);
    assert!(!transcript.compaction_commit_state().is_uncertain());
}

#[tokio::test]
async fn test_micro_rejects_full_directive_overflow_without_partial_commit_or_lost_recovery() {
    use peri_acp_types::context_archive::{
        build_catalog, ArchiveReadRequest, ArchiveRoot, ContextArchiveReadPort,
        ARCHIVE_CHUNK_BYTES, ARCHIVE_MAX_METADATA_BYTES,
    };
    use peri_acp_types::store::{serialize_persisted_payload, PersistedPayload};

    let bound = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    let small = BaseMessage::tool_result("small", "readable original ".repeat(40));
    let small_payload = PersistedPayload::Message(small.clone());
    let small_id = transcript.append(small);
    let small_catalog = build_catalog(
        bound.thread_id.clone(),
        "small".into(),
        small_id,
        std::slice::from_ref(&small_payload),
        "earlier archived output".into(),
    )
    .unwrap();
    let action = |message_id, keep_head| ProjectionActionEntry {
        message_id,
        target: ProjectionTarget::Message,
        action: ProjectionAction::CompactToolResult {
            keep_head,
            keep_tail: 10,
            preserve_recovery_handle: true,
        },
    };
    apply_micro_plan(
        &mut transcript,
        &MicroCompactPlan {
            policy_version: PROJECTION_POLICY_VERSION,
            actions: vec![action(small_id, 100)],
            catalogs: vec![small_catalog.clone()],
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let large = BaseMessage::tool_result("large", "x".repeat(ARCHIVE_CHUNK_BYTES * 962));
    let large_payload = PersistedPayload::Message(large.clone());
    let large_id = transcript.append(large);
    transcript.flush_persistence().await.unwrap();
    let mut large_catalog = build_catalog(
        bound.thread_id.clone(),
        "large".into(),
        large_id,
        std::slice::from_ref(&large_payload),
        "historical output".into(),
    )
    .unwrap();
    let catalog_bytes = serde_json::to_vec(&large_catalog).unwrap().len();
    assert!(catalog_bytes < ARCHIVE_MAX_METADATA_BYTES - 128);
    large_catalog
        .receipt
        .push_str(&"r".repeat(ARCHIVE_MAX_METADATA_BYTES - 128 - catalog_bytes));
    large_catalog.validate().unwrap();
    assert_eq!(
        serde_json::to_vec(&large_catalog).unwrap().len(),
        ARCHIVE_MAX_METADATA_BYTES - 128
    );
    let oversized = MicroCompactPlan {
        policy_version: PROJECTION_POLICY_VERSION,
        actions: vec![action(small_id, 10), action(large_id, 10)],
        catalogs: vec![large_catalog],
        archive_roots: vec![ArchiveRoot {
            source_thread_id: bound.thread_id.clone(),
            first_anchor: large_id,
            last_anchor: large_id,
            committed_tail: large_id,
        }],
        receipt_budget_tokens: 2048,
        ..Default::default()
    };
    assert!(matches!(
        validate_plan_metadata(&transcript, &oversized),
        Err(ArchiveError::Unsupported)
    ));
    let revision = transcript.view_revision();
    let old_small_flags = transcript.flags(small_id);
    let error = apply_micro_plan(&mut transcript, &oversized)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("recovery_unavailable"));
    assert_eq!(transcript.view_revision(), revision);
    assert_eq!(transcript.flags(small_id), old_small_flags);
    assert_eq!(transcript.flags(large_id), MessageFlags::default());

    let fresh = bound.read_only_resources().await;
    let snapshot = fresh.load_session_snapshot(&bound.thread_id).await.unwrap();
    assert_eq!(snapshot.flags[&small_id], old_small_flags);
    assert_eq!(
        snapshot.flags.get(&large_id).cloned().unwrap_or_default(),
        MessageFlags::default()
    );
    assert_eq!(
        serialize_persisted_payload(
            snapshot
                .payloads
                .iter()
                .find(|payload| payload.id() == large_id)
                .unwrap()
        )
        .unwrap(),
        serialize_persisted_payload(&large_payload).unwrap()
    );
    let recovered =
        crate::context_archive::ContextArchiveReader::new(fresh, bound.thread_id.clone())
            .read(ArchiveReadRequest {
                archive_ref: small_catalog.member_ref(0).unwrap(),
                cursor: None,
                budget_tokens: None,
            })
            .await
            .unwrap();
    assert!(recovered.complete);
    assert_eq!(
        recovered.entries[0].data,
        serialize_persisted_payload(&small_payload).unwrap()
    );
}
