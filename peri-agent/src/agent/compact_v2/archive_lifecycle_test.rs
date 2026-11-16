//! Production planner/commit/Full lifecycle, followed by recovery in a separate process.
use crate::agent::compact_v2::config::CompactConfig;

use crate::agent::compact_v2::planner::ArchivePlanningScope;
use crate::agent::compact_v2::planner::ContextPressure;
use crate::agent::compact_v2::planner::MicroPlanningFacts;

use crate::agent::compact_v2::projection::ProviderCapabilities;
use crate::context_archive::ContextArchiveReader;
use crate::messages::BaseMessage;
use crate::messages::ToolCallRequest;
use crate::session::MessageTranscript;
use crate::tools::ContextRetention;

use peri_acp_types::context_archive::ArchiveReadRequest;
use peri_acp_types::context_archive::ContextArchiveReadPort;
use peri_acp_types::session_resources::FrozenSnapshotBytes;
use peri_acp_types::session_resources::NewSession;
use peri_acp_types::session_resources::NewSessionMeta;
use peri_acp_types::session_resources::SessionResources;

use peri_acp_types::workspace::SessionBinding;
use peri_model::{
    Model, ModelCapabilities, ModelMessage, ModelRequest, ModelResponse, ModelResult, ModelStream,
    ModelStreamEvent, StopReason,
};
use peri_resources::sessions::SessionResourcesImpl;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

struct NoIdentifierSummary {
    calls: AtomicUsize,
}

#[tokio::test]
async fn archive_preview_upgrade_earlier_anchors_survive_full_and_enumerate_all_catalogs() {
    let bound = crate::session::test_resources::TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    transcript.append(BaseMessage::human("Keep the API unchanged."));
    for step in 0..6 {
        let id = format!("reverse-{step}");
        transcript.append(BaseMessage::ai_with_tool_calls(
            "",
            vec![ToolCallRequest::new(
                &id,
                "Read",
                serde_json::json!({"path":"old.rs","input":"past input".repeat(2000)}),
            )],
        ));
        transcript.append(BaseMessage::tool_result(id, "past result".repeat(3000)));
    }
    let config = CompactConfig {
        micro_compact_stale_steps: 0,
        ..Default::default()
    };
    let mut facts = MicroPlanningFacts {
        archive_scope: Some(ArchivePlanningScope {
            thread_id: bound.thread_id.clone(),
            committed_tail: transcript
                .entries()
                .last()
                .expect("transcript entries fixture must contain the expected element")
                .id(),
        }),
        ..Default::default()
    };
    for entry in transcript.entries() {
        facts.exposed_message_ids.insert(entry.id());
        if let Some(message) = entry.as_message() {
            facts
                .closed_call_ids
                .extend(message.tool_calls().iter().map(|call| call.id.clone()));
        }
    }
    let pressure = ContextPressure {
        estimated_tokens: 6000,
        context_window: 8000,
        output_reserve: 1000,
        predicted_tool_growth: 0,
        safety_buffer: 0,
        cache_hit_rate: 0.0,
    };
    let mut all_anchors = std::collections::HashSet::new();
    let mut first_round = Vec::new();
    let paired_items = [
        ContextRetention::PreviewOnly,
        ContextRetention::Recomputable,
    ]
    .into_iter()
    .enumerate();
    for (round, policy) in paired_items {
        facts.tool_retention.insert("read".into(), policy);
        let plan = crate::agent::compact_v2::planner::plan_micro_with_facts(
            &transcript,
            &config,
            &pressure,
            &facts,
            &ProviderCapabilities::openai(),
        )
        .expect("the plan micro with facts fixture result is required for this assertion");
        assert!(!plan.catalogs.is_empty());
        let anchors: Vec<_> = plan
            .catalogs
            .iter()
            .map(|catalog| catalog.anchor_message_id)
            .collect();
        if round == 0 {
            first_round = anchors.clone();
        } else {
            let position = |id| {
                transcript
                    .entries()
                    .iter()
                    .position(|entry| entry.id() == id)
                    .expect("the position fixture result is required for this assertion")
            };
            assert!(
                anchors
                    .iter()
                    .any(|id| position(*id) < position(first_round[0])),
                "an upgraded exchange allocates its catalog on the earlier AI call"
            );
        }
        all_anchors.extend(anchors);
        crate::agent::compact_v2::micro::apply_micro_plan(&mut transcript, &plan)
            .await
            .expect("the apply micro plan fixture result is required for this assertion");
    }
    let model = NoIdentifierSummary {
        calls: AtomicUsize::new(0),
    };
    let _ = crate::agent::compact_v2::full::full_compact_inner(
        &mut transcript,
        Some(&model),
        &config,
        ".",
    )
    .await
    .expect("the full compact inner fixture result is required for this assertion");
    let fresh = bound.read_only_resources().await;
    let active = fresh
        .load_active_snapshot(&bound.thread_id)
        .await
        .expect("the load active snapshot fixture result is required for this assertion");
    let roots = active
        .flags
        .values()
        .flat_map(|flags| {
            flags
                .projection
                .iter()
                .flat_map(|directive| directive.archive_roots.clone())
        })
        .collect::<Vec<_>>();
    assert_eq!(roots.len(), 1);
    let reader = ContextArchiveReader::new(fresh, bound.thread_id.clone());
    let mut cursor = None;
    let mut discovered = std::collections::HashSet::new();
    for _ in 0..100 {
        let page = reader
            .read(ArchiveReadRequest {
                archive_ref: roots[0]
                    .reference()
                    .expect("the reference fixture result is required for this assertion"),
                cursor,
                budget_tokens: Some(2048),
            })
            .await
            .expect("the archive fixture must be readable through the canonical recovery port");
        discovered.extend(page.entries.iter().map(|entry| entry.message_id));
        if page.complete {
            break;
        }
        cursor = page.next_cursor;
    }
    assert!(
        all_anchors.is_subset(&discovered),
        "earlier upgraded anchors must not shorten the range and lose later preview catalogs"
    );
}
#[async_trait::async_trait]
impl Model for NoIdentifierSummary {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_streaming: true,
            ..Default::default()
        }
    }
    async fn complete(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelResult<ModelResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        ModelResponse::try_new(ModelMessage::assistant_text("<summary>Preserve the public interface. Historical checks completed; continue the next requested change.</summary>"),StopReason::EndTurn,None,None)
    }
    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        let response = self.complete(request, cancellation.clone()).await?;
        Ok(ModelStream::with_parent_cancellation(
            futures::stream::iter(vec![Ok(ModelStreamEvent::Completed(response))]),
            cancellation,
        ))
    }
}

async fn recover(reader: &ContextArchiveReader, archive_ref: &str) -> Vec<u8> {
    let mut cursor = None;
    let mut bytes = Vec::new();
    for _ in 0..1000 {
        let page = reader
            .read(ArchiveReadRequest {
                archive_ref: archive_ref.into(),
                cursor,
                budget_tokens: Some(2048),
            })
            .await
            .expect("the archive fixture must be readable through the canonical recovery port");
        for entry in page.entries {
            assert_eq!(entry.offset, bytes.len() as u64);
            match entry.encoding.as_str() {
                "utf8" => bytes.extend_from_slice(entry.data.as_bytes()),
                "hex" => bytes.extend((0..entry.data.len()).step_by(2).map(|i| {
                    u8::from_str_radix(&entry.data[i..i + 2], 16)
                        .expect("the from str radix fixture result is required for this assertion")
                })),
                other => panic!("unexpected recovery encoding {other}"),
            }
        }
        if page.complete {
            return bytes;
        }
        cursor = page.next_cursor;
    }
    panic!("recovery did not terminate")
}

#[tokio::test]
async fn archive_two_micro_two_full_keep_original_references_after_process_restart() {
    let directory = tempfile::tempdir().expect("the temporary test directory must be available");
    let database = directory.path().join("archive-lifecycle.db");
    let workspace_dir =
        tempfile::tempdir().expect("the temporary test directory must be available");
    let resources: Arc<dyn SessionResources> = Arc::new(
        SessionResourcesImpl::open(&database)
            .await
            .expect("the open fixture result is required for this assertion"),
    );
    let workspace = resources
        .resolve_workspace(workspace_dir.path())
        .await
        .expect("the resolve workspace fixture result is required for this assertion");
    let thread_id = uuid::Uuid::now_v7().to_string();
    resources
        .create_session(&NewSession {
            thread_id: thread_id.clone(),
            created_at: peri_time::now_utc_rfc3339(),
            meta: NewSessionMeta {
                title: None,
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: SessionBinding::from_workspace(&workspace),
            frozen: FrozenSnapshotBytes::new("{\"version\":1}"),
        })
        .await
        .expect("the create session fixture result is required for this assertion");
    let mut transcript =
        MessageTranscript::new().with_persistence(resources.clone(), thread_id.clone());
    transcript.append(BaseMessage::human("Keep the public interface unchanged."));
    let model = NoIdentifierSummary {
        calls: AtomicUsize::new(0),
    };
    let config = CompactConfig {
        micro_compact_stale_steps: 0,
        ..Default::default()
    };
    let mut references = Vec::new();
    let mut anchors = std::collections::HashSet::new();
    for round in 0..2 {
        for step in 0..6 {
            let call = format!("round-{round}-step-{step}");
            transcript.append(BaseMessage::ai_with_tool_calls("",vec![ToolCallRequest::new(&call,"Write",serde_json::json!({"file_path":"historical.rs","content":format!("round-{round}-step-{step}:{}","original code\n".repeat(1800))}))]));
            transcript.append(BaseMessage::tool_result(
                &call,
                format!(
                    "round-{round}-step-{step}:{}",
                    "historical results\n".repeat(1800)
                ),
            ));
        }
        let mut facts = MicroPlanningFacts {
            archive_scope: Some(ArchivePlanningScope {
                thread_id: thread_id.clone(),
                committed_tail: transcript
                    .entries()
                    .last()
                    .expect("transcript entries fixture must contain the expected element")
                    .id(),
            }),
            ..Default::default()
        };
        for entry in transcript.entries() {
            facts.exposed_message_ids.insert(entry.id());
            if let Some(message) = entry.as_message() {
                for call in message.tool_calls() {
                    facts.closed_call_ids.insert(call.id.clone());
                    facts.tool_retention.insert(
                        call.name.to_lowercase(),
                        ContextRetention::SideEffectReceipt,
                    );
                }
            }
        }
        let plan = crate::agent::compact_v2::planner::plan_micro_with_facts(
            &transcript,
            &config,
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
        .expect("the plan micro with facts fixture result is required for this assertion");
        assert!(
            !plan.catalogs.is_empty(),
            "production planner must create recoverable groups in each round"
        );
        for catalog in &plan.catalogs {
            anchors.insert(catalog.anchor_message_id);
        }
        let catalog = &plan.catalogs[0];
        let source = transcript
            .entries()
            .iter()
            .find(|entry| entry.id() == catalog.members[0].message_id)
            .expect("the find fixture result is required for this assertion")
            .clone()
            .into_payload();
        references.push((
            catalog
                .member_ref(0)
                .expect("the member ref fixture result is required for this assertion"),
            peri_acp_types::store::serialize_persisted_payload(&source)
                .expect(
                    "the serialize persisted payload fixture result is required for this assertion",
                )
                .into_bytes(),
        ));
        assert!(
            crate::agent::compact_v2::micro::apply_micro_plan(&mut transcript, &plan)
                .await
                .expect("the apply micro plan fixture result is required for this assertion")
                > 0
        );
        let full = crate::agent::compact_v2::full::full_compact_inner(
            &mut transcript,
            Some(&model),
            &config,
            ".",
        )
        .await
        .expect("the full compact inner fixture result is required for this assertion");
        assert!(full.affected_count > 0);
        let summary = full
            .summary
            .expect("the summary fixture result is required for this assertion");
        assert!(
            !summary.contains("pa1:")
                && !summary.contains(&catalog.anchor_message_id.as_uuid().to_string()),
            "summary model must not carry archive identifiers"
        );
    }
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    transcript
        .flush_persistence()
        .await
        .expect("the flush persistence fixture result is required for this assertion");
    drop(transcript);
    drop(resources);
    let fresh: Arc<dyn SessionResources> = Arc::new(
        SessionResourcesImpl::open_existing_read_only(&database)
            .await
            .expect("the open existing read only fixture result is required for this assertion"),
    );
    let active = fresh
        .load_active_snapshot(&thread_id)
        .await
        .expect("the load active snapshot fixture result is required for this assertion");
    let roots = active
        .flags
        .values()
        .flat_map(|flags| {
            flags
                .projection
                .iter()
                .flat_map(|p| p.archive_roots.clone())
        })
        .collect::<Vec<_>>();
    assert!(
        !roots.is_empty(),
        "Full must transfer archive roots independently of summary prose"
    );
    let mut cold = MessageTranscript::new().with_own_payloads(active.payloads);
    cold.set_flags_batch(active.flags);
    let visible = crate::agent::compact_v2::projection::render_persisted_llm_view(
        &cold,
        &ProviderCapabilities::openai(),
    )
    .expect("the render persisted llm view fixture result is required for this assertion");
    assert!(visible
        .iter()
        .any(|message| message.content().contains("ReadContextArchive")));
    let reader = ContextArchiveReader::new(fresh, thread_id.clone());
    let mut discovered = std::collections::HashSet::new();
    for root in roots {
        let mut cursor = None;
        for _ in 0..100 {
            let page = reader
                .read(ArchiveReadRequest {
                    archive_ref: root
                        .reference()
                        .expect("the reference fixture result is required for this assertion"),
                    cursor,
                    budget_tokens: None,
                })
                .await
                .expect("the archive fixture must be readable through the canonical recovery port");
            discovered.extend(page.entries.iter().map(|entry| entry.message_id));
            if page.complete {
                break;
            }
            cursor = page.next_cursor;
        }
    }
    assert!(
        anchors.is_subset(&discovered),
        "bounded root enumeration must retain both Micro rounds"
    );
    for (archive_ref, expected) in &references {
        assert_eq!(&recover(&reader, archive_ref).await, expected);
    }
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::process::Command::new(
            std::env::current_exe()
                .expect("the current exe fixture result is required for this assertion"),
        )
        .kill_on_drop(true)
        .args([
            "--exact",
            "agent::compact_v2::archive_lifecycle_tests::archive_recovery_process_entry",
            "--nocapture",
        ])
        .env("PERI_ARCHIVE_TEST_DATABASE", &database)
        .env("PERI_ARCHIVE_TEST_SESSION", &thread_id)
        .env("PERI_ARCHIVE_TEST_REFERENCE", &references[0].0)
        .env(
            "PERI_ARCHIVE_TEST_EXPECTED_DIGEST",
            peri_acp_types::context_archive::content_digest(&references[0].1),
        )
        .output(),
    )
    .await
    .expect("archive reader subprocess timed out")
    .expect("the expect fixture result is required for this assertion");
    assert!(
        output.status.success(),
        "fresh process recovery failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("ARCHIVE_FRESH_PROCESS_VERIFIED"));
}

/// Subprocess harness entry; only the lifecycle test supplies these fixture identities.
#[tokio::test]
async fn archive_recovery_process_entry() {
    let Some(database) = std::env::var_os("PERI_ARCHIVE_TEST_DATABASE") else {
        return;
    };
    let resources: Arc<dyn SessionResources> = Arc::new(
        SessionResourcesImpl::open_existing_read_only(std::path::PathBuf::from(database))
            .await
            .expect("the open existing read only fixture result is required for this assertion"),
    );
    let reader = ContextArchiveReader::new(
        resources,
        std::env::var("PERI_ARCHIVE_TEST_SESSION")
            .expect("the var fixture result is required for this assertion"),
    );
    let recovered = recover(
        &reader,
        &std::env::var("PERI_ARCHIVE_TEST_REFERENCE")
            .expect("the var fixture result is required for this assertion"),
    )
    .await;
    assert_eq!(
        peri_acp_types::context_archive::content_digest(&recovered),
        std::env::var("PERI_ARCHIVE_TEST_EXPECTED_DIGEST")
            .expect("the var fixture result is required for this assertion")
    );
    println!("ARCHIVE_FRESH_PROCESS_VERIFIED");
}
