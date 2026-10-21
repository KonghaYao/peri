//! Planner → persistence barrier → bounded verification → atomic publish, using a controlled remote read port.
use super::*;
use crate::agent::compact_v2::planner::{
    plan_micro_with_facts, ArchivePlanningScope, ContextPressure, MicroPlanningFacts,
};
use crate::agent::compact_v2::projection::{render_persisted_llm_view, ProviderCapabilities};
use crate::messages::{BaseMessage, ToolCallRequest};
use crate::session::test_resources::TestSession;
use crate::tools::ContextRetention;
use async_trait::async_trait;
use peri_acp_types::system_reminder::TrustedSystemReminder;
use peri_acp_types::{context_archive::*, session_resources::*, store::*, thread::*, workspace::*};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

#[derive(Default, Debug)]
struct Statistics {
    metadata_reads: usize,
    batch_reads: usize,
    chunks: usize,
    verified_bytes: usize,
    maximum_batch_bytes: usize,
    in_flight: usize,
    maximum_in_flight: usize,
    dropped_reads: usize,
    append_calls: usize,
    commits: usize,
    append_io: Duration,
    verification_io: Duration,
    metadata_io: Duration,
    commit_io: Duration,
    tails: Vec<MessageId>,
}

struct MeteredArchive {
    inner: Arc<dyn SessionResources>,
    stats: Arc<Mutex<Statistics>>,
    // Fake remote bytes exist only after the real durable append acknowledges them.
    canonical: Mutex<Vec<(MessageId, Vec<u8>)>>,
    hold_appends: AtomicBool,
    release_append: tokio::sync::Semaphore,
    block_read: AtomicBool,
    release: tokio::sync::Semaphore,
    corrupt_read: AtomicBool,
}
impl MeteredArchive {
    fn new(inner: Arc<dyn SessionResources>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            stats: Arc::new(Mutex::new(Statistics::default())),
            canonical: Mutex::new(Vec::new()),
            hold_appends: AtomicBool::new(false),
            release_append: tokio::sync::Semaphore::new(0),
            block_read: AtomicBool::new(false),
            release: tokio::sync::Semaphore::new(0),
            corrupt_read: AtomicBool::new(false),
        })
    }
}
struct InFlight {
    stats: Arc<Mutex<Statistics>>,
    completed: bool,
}
impl Drop for InFlight {
    fn drop(&mut self) {
        let mut stats = self.stats.lock().unwrap();
        stats.in_flight -= 1;
        if !self.completed {
            stats.dropped_reads += 1;
        }
    }
}

#[async_trait]
impl SessionResources for MeteredArchive {
    async fn read_archive_metadata(
        &self,
        id: &ThreadId,
        query: &ArchiveMetadataQuery,
    ) -> SessionResourceResult<ArchiveMetadataPage> {
        let start = Instant::now();
        let result = self.inner.read_archive_metadata(id, query).await;
        let mut stats = self.stats.lock().unwrap();
        stats.metadata_reads += 1;
        stats.metadata_io += start.elapsed();
        result
    }
    async fn read_archive_chunk(
        &self,
        id: &ThreadId,
        query: &ArchiveChunkQuery,
    ) -> SessionResourceResult<ArchiveChunk> {
        self.read_archive_chunks(id, &ArchiveChunkBatch::new(vec![query.clone()]).unwrap())
            .await
            .map(|mut chunks| chunks.remove(0))
    }
    async fn read_archive_chunks(
        &self,
        _id: &ThreadId,
        batch: &ArchiveChunkBatch,
    ) -> SessionResourceResult<Vec<ArchiveChunk>> {
        let start = Instant::now();
        {
            let mut stats = self.stats.lock().unwrap();
            stats.batch_reads += 1;
            stats.in_flight += 1;
            stats.maximum_in_flight = stats.maximum_in_flight.max(stats.in_flight);
            stats.maximum_batch_bytes = stats
                .maximum_batch_bytes
                .max(batch.queries().iter().map(|q| q.max_bytes as usize).sum());
            stats
                .tails
                .extend(batch.queries().iter().map(|query| query.committed_tail));
        }
        let mut guard = InFlight {
            stats: Arc::clone(&self.stats),
            completed: false,
        };
        if self.block_read.load(Ordering::SeqCst) {
            let _permit = self.release.acquire().await.unwrap();
        }
        let canonical = self.canonical.lock().unwrap();
        let tail = canonical
            .iter()
            .position(|(id, _)| *id == batch.queries()[0].committed_tail)
            .unwrap();
        let mut result = Vec::new();
        for query in batch.queries() {
            let bytes = &canonical
                .iter()
                .take(tail + 1)
                .find(|(id, _)| *id == query.message_id)
                .unwrap()
                .1;
            let mut chunk = bytes[query.offset as usize
                ..bytes
                    .len()
                    .min(query.offset as usize + query.max_bytes as usize)]
                .to_vec();
            if self.corrupt_read.load(Ordering::SeqCst) {
                chunk[0] ^= 1;
            }
            self.stats.lock().unwrap().verified_bytes += chunk.len();
            result.push(ArchiveChunk {
                bytes: chunk,
                total_bytes: bytes.len() as u64,
            });
        }
        let mut stats = self.stats.lock().unwrap();
        stats.chunks += result.len();
        stats.verification_io += start.elapsed();
        drop(stats);
        guard.completed = true;
        Ok(result)
    }
    async fn inspect_availability(
        &self,
        session: Option<&ThreadId>,
    ) -> SessionResourceResult<SessionAvailability> {
        self.inner.inspect_availability(session).await
    }
    async fn resolve_workspace(&self, cwd: &Path) -> SessionResourceResult<ResolvedWorkspace> {
        self.inner.resolve_workspace(cwd).await
    }
    async fn validate_session(
        &self,
        id: &ThreadId,
        workspace: &ResolvedWorkspace,
    ) -> SessionResourceResult<()> {
        self.inner.validate_session(id, workspace).await
    }
    async fn finish_close(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.inner.finish_close(id).await
    }
    async fn close_settlement(&self, id: &ThreadId) -> SessionResourceResult<CloseSettlement> {
        self.inner.close_settlement(id).await
    }
    async fn create_session(&self, input: &NewSession) -> SessionResourceResult<()> {
        self.inner.create_session(input).await
    }
    async fn abandon_initialization(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.inner.abandon_initialization(id).await
    }
    async fn begin_initialization(
        &self,
        draft: &NewSessionDraft,
    ) -> SessionResourceResult<Arc<dyn SessionInitialization>> {
        self.inner.begin_initialization(draft).await
    }
    async fn discard_incomplete_initialization(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.inner.discard_incomplete_initialization(id).await
    }
    async fn adopt_legacy_session(
        &self,
        id: &ThreadId,
        saved_cwd: &str,
        workspace: &ResolvedWorkspace,
        frozen: &FrozenSnapshotBytes,
    ) -> SessionResourceResult<()> {
        self.inner
            .adopt_legacy_session(id, saved_cwd, workspace, frozen)
            .await
    }
    async fn load_session_snapshot(&self, id: &ThreadId) -> SessionResourceResult<SessionSnapshot> {
        self.inner.load_session_snapshot(id).await
    }
    async fn load_session_binding(&self, id: &ThreadId) -> SessionResourceResult<BindingState> {
        self.inner.load_session_binding(id).await
    }
    async fn validate_bound_workspace(
        &self,
        id: &ThreadId,
        check: BindingRecheck,
    ) -> SessionResourceResult<ResolvedWorkspace> {
        self.inner.validate_bound_workspace(id, check).await
    }
    async fn load_session_history(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Vec<PersistedPayload>> {
        self.inner.load_session_history(id).await
    }
    async fn load_session_meta(&self, id: &ThreadId) -> SessionResourceResult<ThreadMeta> {
        self.inner.load_session_meta(id).await
    }
    async fn list_sessions(
        &self,
        query: &ScopedThreadQuery,
    ) -> SessionResourceResult<ScopedThreadPage> {
        self.inner.list_sessions(query).await
    }
    async fn list_children(&self, parent: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>> {
        self.inner.list_children(parent).await
    }
    async fn list_session_tree(&self, root: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>> {
        self.inner.list_session_tree(root).await
    }
    async fn append_history(
        &self,
        id: &ThreadId,
        payloads: &[PersistedPayload],
    ) -> SessionResourceResult<()> {
        if self.hold_appends.load(Ordering::SeqCst) {
            let _permit = self.release_append.acquire().await.unwrap();
        }
        let start = Instant::now();
        self.inner.append_history(id, payloads).await?;
        let elapsed = start.elapsed();
        self.canonical
            .lock()
            .unwrap()
            .extend(payloads.iter().map(|payload| {
                (
                    payload.id(),
                    serialize_persisted_payload(payload).unwrap().into_bytes(),
                )
            }));
        let mut stats = self.stats.lock().unwrap();
        stats.append_calls += 1;
        stats.append_io += elapsed;
        Ok(())
    }
    async fn append_reminder_if_absent(
        &self,
        id: &ThreadId,
        message_id: MessageId,
        reminder: &TrustedSystemReminder,
    ) -> SessionResourceResult<bool> {
        self.inner
            .append_reminder_if_absent(id, message_id, reminder)
            .await
    }
    async fn mark_session_closing(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.inner.mark_session_closing(id).await
    }
    async fn is_session_closing(&self, id: &ThreadId) -> SessionResourceResult<bool> {
        self.inner.is_session_closing(id).await
    }
    async fn save_fork(&self, fork: &ForkSnapshot) -> SessionResourceResult<()> {
        self.inner.save_fork(fork).await
    }
    async fn save_child(&self, child: &ChildSnapshot) -> SessionResourceResult<()> {
        self.inner.save_child(child).await
    }
    async fn claim_child_resume(
        &self,
        child: &ThreadId,
        root: &ThreadId,
    ) -> SessionResourceResult<Box<dyn ChildResumeClaim>> {
        self.inner.claim_child_resume(child, root).await
    }
    async fn apply_compaction(
        &self,
        id: &ThreadId,
        change: &CompactionChange,
    ) -> SessionResourceResult<()> {
        let start = Instant::now();
        self.inner.apply_compaction(id, change).await?;
        let mut stats = self.stats.lock().unwrap();
        stats.commits += 1;
        stats.commit_io += start.elapsed();
        Ok(())
    }
    async fn apply_message_projections(
        &self,
        id: &ThreadId,
        updates: &[(MessageId, MessageFlags)],
    ) -> SessionResourceResult<()> {
        self.inner.apply_message_projections(id, updates).await
    }
    async fn rewind_history(
        &self,
        id: &ThreadId,
        boundary: RewindBoundary,
        clear_excluded: &[MessageId],
    ) -> SessionResourceResult<RewindOutcome> {
        self.inner
            .rewind_history(id, boundary, clear_excluded)
            .await
    }
    async fn remove_history_entries(
        &self,
        id: &ThreadId,
        ids: &[MessageId],
    ) -> SessionResourceResult<()> {
        self.inner.remove_history_entries(id, ids).await
    }
    async fn update_session_meta(
        &self,
        id: &ThreadId,
        patch: &SessionMetaPatch,
    ) -> SessionResourceResult<()> {
        self.inner.update_session_meta(id, patch).await
    }
    async fn delete_session_tree(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.inner.delete_session_tree(id).await
    }
    async fn recover_session_persistence(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<PersistenceRecovery> {
        self.inner.recover_session_persistence(id).await
    }
    async fn drain_persistence(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.inner.drain_persistence(id).await
    }
}

fn make_fixture(
    transcript: &mut MessageTranscript,
    steps: usize,
    body_bytes: usize,
    session_id: String,
) -> MicroPlanningFacts {
    transcript.append(BaseMessage::human("Keep historical source recoverable."));
    let mut facts = MicroPlanningFacts::default();
    facts
        .tool_retention
        .insert("read".into(), ContextRetention::Recomputable);
    for index in 0..steps {
        let call = format!("read-{index}");
        let request = transcript.append(BaseMessage::ai_with_tool_calls(
            "",
            vec![ToolCallRequest::new(
                &call,
                "Read",
                serde_json::json!({"path": format!("source-{index}.rs")}),
            )],
        ));
        let result = transcript.append(BaseMessage::tool_result(
            &call,
            format!("historical-{index}:{}", "x".repeat(body_bytes)),
        ));
        facts.exposed_message_ids.extend([request, result]);
        facts.closed_call_ids.insert(call);
    }
    facts.archive_scope = Some(ArchivePlanningScope {
        thread_id: session_id,
        committed_tail: transcript.entries().last().unwrap().id(),
    });
    facts
}

fn plan_fixture(transcript: &MessageTranscript, facts: &MicroPlanningFacts) -> MicroCompactPlan {
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
        facts,
        &ProviderCapabilities::openai(),
    )
    .unwrap()
}

fn populate(
    transcript: &mut MessageTranscript,
    steps: usize,
    session_id: String,
) -> MicroCompactPlan {
    let facts = make_fixture(transcript, steps, 24000, session_id);
    plan_fixture(transcript, &facts)
}

#[tokio::test]
async fn test_micro_batches_entire_plan_under_one_tail_and_one_atomic_commit() {
    let bound = TestSession::open().await;
    let resources = MeteredArchive::new(bound.resources());
    let mut transcript =
        MessageTranscript::new().with_persistence(resources.clone(), bound.thread_id.clone());
    let plan = populate(&mut transcript, 80, bound.thread_id.clone());
    assert!(plan.catalogs.len() > 32);
    let tail = transcript.entries().last().unwrap().id();
    let chunks: usize = plan
        .catalogs
        .iter()
        .flat_map(|c| &c.members)
        .map(|m| m.chunk_digests.len())
        .sum();
    let expected_bytes: usize = plan
        .catalogs
        .iter()
        .flat_map(|c| &c.members)
        .map(|m| m.canonical_bytes as usize)
        .sum();
    apply_micro_plan(&mut transcript, &plan).await.unwrap();
    {
        let stats = resources.stats.lock().unwrap();
        assert_eq!(stats.metadata_reads, 1, "整个计划只固定一次持久尾界");
        assert_eq!(stats.batch_reads, chunks.div_ceil(ARCHIVE_CHUNKS_PER_BATCH));
        assert_eq!(stats.chunks, chunks);
        assert_eq!(stats.verified_bytes, expected_bytes);
        assert!(stats.maximum_batch_bytes <= ARCHIVE_CHUNKS_PER_BATCH * ARCHIVE_CHUNK_BYTES);
        assert_eq!(stats.maximum_in_flight, 1, "同一连接不并发启动事务");
        assert_eq!(stats.in_flight, 0);
        assert!(stats.tails.iter().all(|seen| *seen == tail));
        assert!(stats.append_calls > 0, "原文必须先越过真实持久化barrier");
        assert_eq!(stats.commits, 1, "投影仍然只提交一次");
    }
    let snapshot = bound
        .read_only_resources()
        .await
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    let mut restored = MessageTranscript::new().with_own_payloads(snapshot.payloads);
    restored.set_flags_batch(snapshot.flags);
    assert_eq!(
        serde_json::to_value(
            render_persisted_llm_view(&transcript, &ProviderCapabilities::openai()).unwrap()
        )
        .unwrap(),
        serde_json::to_value(
            render_persisted_llm_view(&restored, &ProviderCapabilities::openai()).unwrap()
        )
        .unwrap()
    );
}

#[tokio::test]
async fn test_micro_cancel_drops_the_inflight_batch_without_publishing_projection() {
    let bound = TestSession::open().await;
    let resources = MeteredArchive::new(bound.resources());
    resources.block_read.store(true, Ordering::SeqCst);
    let mut transcript =
        MessageTranscript::new().with_persistence(resources.clone(), bound.thread_id.clone());
    let plan = populate(&mut transcript, 40, bound.thread_id.clone());
    let revision = transcript.view_revision();
    {
        let work = apply_micro_plan(&mut transcript, &plan);
        tokio::pin!(work);
        peri_time::timeout(Duration::from_secs(5), async {
            loop {
                tokio::select! {
                    result = &mut work => panic!("验证在取消前意外结束: {result:?}"),
                    _ = tokio::task::yield_now() => {
                        if resources.stats.lock().unwrap().in_flight > 0 { break; }
                    }
                }
            }
        })
        .await
        .unwrap();
    }
    let stats = resources.stats.lock().unwrap();
    assert_eq!(
        (
            stats.batch_reads,
            stats.dropped_reads,
            stats.in_flight,
            stats.commits
        ),
        (1, 1, 0, 0)
    );
    assert_eq!(transcript.view_revision(), revision);
    assert!(transcript
        .entries()
        .iter()
        .all(|entry| transcript.projection_ref(entry.id()).is_none()));
}

#[tokio::test]
async fn test_micro_corrupt_batch_fails_before_any_projection_is_published() {
    let bound = TestSession::open().await;
    let resources = MeteredArchive::new(bound.resources());
    resources.corrupt_read.store(true, Ordering::SeqCst);
    let mut transcript =
        MessageTranscript::new().with_persistence(resources.clone(), bound.thread_id.clone());
    let plan = populate(&mut transcript, 40, bound.thread_id.clone());
    let revision = transcript.view_revision();
    let error = apply_micro_plan(&mut transcript, &plan).await.unwrap_err();
    assert!(
        error.to_string().contains("archive_content_mismatch"),
        "错误必须保留原文认证失败: {error}"
    );
    assert_eq!(resources.stats.lock().unwrap().commits, 0);
    assert_eq!(transcript.view_revision(), revision);
    assert!(transcript
        .entries()
        .iter()
        .all(|entry| transcript.projection_ref(entry.id()).is_none()));
}

#[cfg(unix)]
fn cpu_seconds() -> f64 {
    // Process CPU is separate from wall time and excludes other verification processes.
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
        0
    );
    let usage = unsafe { usage.assume_init() };
    usage.ru_utime.tv_sec as f64
        + usage.ru_utime.tv_usec as f64 / 1e6
        + usage.ru_stime.tv_sec as f64
        + usage.ru_stime.tv_usec as f64 / 1e6
}

// Test-only baseline: the original per-catalog, per-chunk verification strategy.
// The writer barrier, exact update assembly and atomic hot publication remain the production ones.
#[cfg(unix)]
async fn apply_serial_baseline(
    transcript: &mut MessageTranscript,
    plan: &MicroCompactPlan,
) -> AgentResult<usize> {
    validate_source_flags(transcript, plan)?;
    validate_plan_metadata(transcript, plan).unwrap();
    transcript.flush_persistence().await?;
    let (resources, session_id, _) = transcript.idempotent_reminder_port().unwrap();
    let mut tails = std::collections::HashMap::new();
    for catalog in &plan.catalogs {
        catalog.validate().unwrap();
        let page = resources
            .read_archive_metadata(
                &session_id,
                &ArchiveMetadataQuery {
                    anchor: Some(catalog.anchor_message_id),
                    limit: 1,
                    ..Default::default()
                },
            )
            .await
            .map_err(anyhow::Error::new)?;
        for member in &catalog.members {
            for (index, digest) in member.chunk_digests.iter().enumerate() {
                let chunk = resources
                    .read_archive_chunk(
                        &session_id,
                        &ArchiveChunkQuery {
                            message_id: member.message_id,
                            committed_tail: page.committed_tail,
                            offset: (index * ARCHIVE_CHUNK_BYTES) as u64,
                            max_bytes: ARCHIVE_CHUNK_BYTES as u32,
                        },
                    )
                    .await
                    .map_err(anyhow::Error::new)?;
                assert_eq!(chunk.total_bytes, member.canonical_bytes);
                assert_eq!(&content_digest(&chunk.bytes), digest);
            }
        }
        tails.insert(catalog.source_thread_id.clone(), page.committed_tail);
    }
    validate_source_flags(transcript, plan)?;
    let flag_updates = prepare_updates(transcript, plan, &tails).unwrap();
    let affected = flag_updates.len();
    transcript
        .commit_compaction_lifecycle(CompactionChange {
            flag_updates,
            appended_messages: Vec::new(),
        })
        .await?;
    Ok(affected)
}

#[cfg(unix)]
async fn measure_complete_path(case: &str, steps: usize, body_bytes: usize) {
    for serial in [true, false] {
        let bound = TestSession::open().await;
        let resources = MeteredArchive::new(bound.resources());
        resources.hold_appends.store(true, Ordering::SeqCst);
        let mut transcript =
            MessageTranscript::new().with_persistence(resources.clone(), bound.thread_id.clone());
        let facts = make_fixture(&mut transcript, steps, body_bytes, bound.thread_id.clone());
        let start = Instant::now();
        let cpu_start = cpu_seconds();
        resources.hold_appends.store(false, Ordering::SeqCst);
        resources.release_append.add_permits(1);
        let plan = plan_fixture(&transcript, &facts);
        let planner_wall = start.elapsed();
        let planner_cpu = cpu_seconds() - cpu_start;
        let apply_start = Instant::now();
        let affected = if serial {
            apply_serial_baseline(&mut transcript, &plan).await.unwrap()
        } else {
            apply_micro_plan(&mut transcript, &plan).await.unwrap()
        };
        let total_cpu = cpu_seconds() - cpu_start;
        let stats = resources.stats.lock().unwrap();
        eprintln!("MICRO_COMPLETE_PATH case={case} strategy={} steps={steps} body_bytes={body_bytes} catalogs={} affected={affected} chunks={} canonical_bytes={} planner_wall_ms={} planner_process_cpu_ms={} apply_wall_ms={} apply_process_cpu_ms={} total_wall_ms={} total_process_cpu_ms={} metadata_api_calls={} body_batch_api_calls={} maximum_in_flight_batches={} maximum_body_bytes={} append_calls={} commit_calls={} sqlite_append_port_wall_ms={} sqlite_metadata_port_wall_ms={} controlled_body_port_wall_ms={} sqlite_commit_port_wall_ms={} network_rtt=not_measured", if serial { "serial_baseline" } else { "bounded_batch" }, plan.catalogs.len(), stats.chunks, stats.verified_bytes, planner_wall.as_millis(), (planner_cpu * 1000.0) as u64, apply_start.elapsed().as_millis(), ((total_cpu - planner_cpu) * 1000.0) as u64, start.elapsed().as_millis(), (total_cpu * 1000.0) as u64, stats.metadata_reads, stats.batch_reads, stats.maximum_in_flight, stats.maximum_batch_bytes, stats.append_calls, stats.commits, stats.append_io.as_millis(), stats.metadata_io.as_millis(), stats.verification_io.as_millis(), stats.commit_io.as_millis());
        assert_eq!(
            stats.batch_reads,
            if serial {
                stats.chunks
            } else {
                stats.chunks.div_ceil(ARCHIVE_CHUNKS_PER_BATCH)
            }
        );
        assert_eq!(stats.commits, 1);
    }
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "manual complete-path comparison; controlled remote byte port, no network RTT"]
async fn micro_complete_path_small_results_benchmark() {
    measure_complete_path("small_results", 500, 4096).await;
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "manual complete-path comparison; controlled remote byte port, no network RTT"]
async fn micro_complete_path_large_results_benchmark() {
    measure_complete_path("large_results", 32, 500_000).await;
}
