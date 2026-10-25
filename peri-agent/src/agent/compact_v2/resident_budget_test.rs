//! 独立于 token 的驻留字节预算触发 Full。
//!
//! 夹具只使用小阈值（1 字节或快照算出的实际容量），不分配 128 MiB；断言的是触发
//! 顺序、Micro planner 未被调用、失败传播与 Full 后残余记录。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_model::{
    Model, ModelCapabilities, ModelError, ModelMessage, ModelRequest, ModelResponse, ModelResult,
    ModelStream, StopReason,
};
use tokio_util::sync::CancellationToken;

use super::*;
use crate::agent::compact_v2::planner::{
    plan_micro_call_count, reset_plan_micro_calls, FullEscalationReason, MicroPlanningFacts,
};
use crate::agent::compact_v2::{
    run_compact_with_facts_and_resident, CompactOutcome, ProviderCapabilities,
    ResidentHistoryPressure,
};
use crate::agent::events::CompactStrategy;
use crate::messages::{BaseMessage, MessageContent};
use crate::session::async_result_mailbox::ProtectedResults;
use crate::session::test_resources::TestSession;
use crate::session::transcript::MessageTranscript;

fn make_human(text: &str) -> BaseMessage {
    BaseMessage::human(MessageContent::text(text.to_string()))
}

fn make_ai_with_tool(text: &str, tool_name: &str, tool_id: &str) -> BaseMessage {
    BaseMessage::ai_with_tool_calls(
        MessageContent::text(text.to_string()),
        vec![crate::messages::ToolCallRequest::new(
            tool_id,
            tool_name,
            serde_json::json!({"content": "x".repeat(501)}),
        )],
    )
}

fn make_tool_result(tool_call_id: &str, text: &str) -> BaseMessage {
    let body = format!(
        "{text}{}",
        "x".repeat(8_000usize.saturating_sub(text.len()))
    );
    BaseMessage::tool_result(tool_call_id.to_string(), MessageContent::text(body))
}

fn build_transcript(
    resources: Arc<dyn peri_acp_types::session_resources::SessionResources>,
    thread_id: String,
) -> MessageTranscript {
    let mut transcript = MessageTranscript::new().with_persistence(resources, thread_id);
    for index in 0..8 {
        transcript.append(make_human(&format!("q {index}")));
        transcript.append(make_ai_with_tool(
            "",
            "Bash",
            &format!("resident_call_{index}"),
        ));
        transcript.append(make_tool_result(
            &format!("resident_call_{index}"),
            &format!("resident output {index}"),
        ));
    }
    transcript
}

fn exposed_facts(transcript: &MessageTranscript, session_id: &str) -> MicroPlanningFacts {
    let mut facts = MicroPlanningFacts {
        archive_scope: transcript.entries().last().map(|entry| {
            crate::agent::compact_v2::planner::ArchivePlanningScope {
                thread_id: session_id.into(),
                committed_tail: entry.id(),
            }
        }),
        ..Default::default()
    };
    for entry in transcript.entries().iter().skip(transcript.ancestor_len()) {
        facts.exposed_message_ids.insert(entry.id());
        if let Some(message) = entry.as_message() {
            for call in message.tool_calls() {
                facts.closed_call_ids.insert(call.id.clone());
                facts.tool_retention.insert(
                    call.name.to_lowercase(),
                    crate::tools::ContextRetention::Recomputable,
                );
            }
        }
    }
    facts
}

fn pressure(budget_pct: f64) -> ContextPressure {
    let context_window = 200_000u32;
    ContextPressure {
        estimated_tokens: (budget_pct * context_window as f64) as u64,
        context_window,
        output_reserve: 8_000,
        predicted_tool_growth: 0,
        safety_buffer: 5_000,
        cache_hit_rate: 0.0,
    }
}

struct RecordingModel {
    requests: AtomicUsize,
    fail: bool,
}

impl RecordingModel {
    fn new(fail: bool) -> Self {
        Self {
            requests: AtomicUsize::new(0),
            fail,
        }
    }

    fn request_count(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Model for RecordingModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_streaming: true,
            ..Default::default()
        }
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        Err(ModelError::cancelled())
    }

    async fn complete(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelResult<ModelResponse> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(ModelError::http_status(500, "resident-test", None::<&str>));
        }
        Ok(ModelResponse::new(
            ModelMessage::assistant_text("<summary>resident full summary</summary>"),
            StopReason::EndTurn,
            None,
            None,
        )?)
    }
}

#[derive(Clone, Default)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
    type Writer = LogBuffer;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn install_global_test_subscriber() {
    static INSTALL: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    INSTALL.get_or_init(|| {
        // Thread-local guards restore the process default when dropped. Without a global
        // default, that rebuild disables callsites for other concurrently running tests and
        // makes log assertions flaky.
        let _ = tracing::subscriber::set_global_default(
            tracing_subscriber::fmt()
                .with_max_level(tracing::Level::INFO)
                .with_writer(std::io::sink)
                .finish(),
        );
    });
}

fn capture_logs() -> (LogBuffer, tracing::subscriber::DefaultGuard) {
    install_global_test_subscriber();
    let logs = LogBuffer::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_writer(logs.clone())
        .finish();
    (logs, tracing::subscriber::set_default(subscriber))
}

#[test]
fn resident_pressure_uses_own_plus_inherited_reserved_without_excluded() {
    let pressure = ResidentHistoryPressure {
        limit_bytes: 10,
        // Full 可归档 own。
        reclaimable_own_bytes: 4,
        // Full 保留的 own（System/protected/已 excluded）。
        irreducible_own_bytes: 0,
        irreducible_inherited_bytes: 6,
    };
    assert_eq!(pressure.total_reserved_bytes(), 10);
    assert_eq!(pressure.reclaimable_own_reserved_bytes(), 4);
    assert_eq!(pressure.irreducible_own_reserved_bytes(), 0);
    assert_eq!(pressure.irreducible_inherited_reserved_bytes(), 6);
    assert!(!pressure.is_exceeded(), "恰好等于上限不触发");

    let over = ResidentHistoryPressure {
        limit_bytes: 9,
        ..pressure
    };
    assert!(over.is_exceeded());
    assert_eq!(over.excess_bytes(), 1);

    assert!(
        !ResidentHistoryPressure::DISABLED.is_exceeded(),
        "limit=0 关闭门控"
    );
    let zero_limit_with_data = ResidentHistoryPressure {
        limit_bytes: 0,
        reclaimable_own_bytes: 100,
        irreducible_own_bytes: 100,
        irreducible_inherited_bytes: 100,
    };
    assert!(
        !zero_limit_with_data.is_exceeded(),
        "limit=0 是关闭门控，不是立即超限"
    );

    let saturated = ResidentHistoryPressure {
        limit_bytes: u64::MAX,
        reclaimable_own_bytes: u64::MAX,
        irreducible_own_bytes: u64::MAX,
        irreducible_inherited_bytes: u64::MAX,
    };
    assert_eq!(saturated.total_reserved_bytes(), u64::MAX, "饱和相加");
}

#[test]
fn resident_pressure_separates_system_and_protected_own() {
    let mut transcript = MessageTranscript::new();
    transcript.append(BaseMessage::system("s".repeat(512)));
    let protected = make_human("async result");
    let protected_id = protected.id();
    transcript.append(protected);
    transcript.append(make_human("reclaimable own"));
    let protected_ids = std::collections::HashSet::from([protected_id]);

    let pressure =
        ResidentHistoryPressure::from_transcript_with_protected(1, &transcript, &protected_ids);
    assert!(
        pressure.irreducible_own_reserved_bytes() > 0,
        "System 与 protected 必须计入不可回收 own"
    );
    assert!(
        pressure.reclaimable_own_reserved_bytes() > 0,
        "普通 own 必须计入可回收"
    );
    assert_eq!(pressure.irreducible_inherited_reserved_bytes(), 0);
    assert_eq!(
        pressure.own_reserved_bytes(),
        pressure
            .reclaimable_own_reserved_bytes()
            .saturating_add(pressure.irreducible_own_reserved_bytes())
    );
}

#[tokio::test]
async fn resident_budget_forces_full_at_zero_token_pressure() {
    let bound = TestSession::open().await;
    let mut transcript = build_transcript(bound.resources(), bound.thread_id.clone());
    let resident = ResidentHistoryPressure::from_transcript(1, &transcript);
    assert!(resident.is_exceeded());

    let config = CompactConfig {
        max_resident_history_bytes: 1,
        ..Default::default()
    };
    let model = RecordingModel::new(false);
    let mut failures = 0u32;
    let facts = exposed_facts(&transcript, &bound.thread_id);
    reset_plan_micro_calls();

    let result = run_compact_with_facts_and_resident(
        &mut transcript,
        Some(&model),
        &config,
        &pressure(0.0),
        false,
        &mut failures,
        "/tmp",
        None,
        &ProtectedResults::default(),
        &facts,
        &ProviderCapabilities::openai(),
        &resident,
    )
    .await;

    assert_eq!(result.outcome, CompactOutcome::FullApplied);
    assert_eq!(
        result.full_escalation_reason,
        Some(FullEscalationReason::ResidentHistoryBudgetExceeded)
    );
    assert!(result.summary.is_some());
    assert_eq!(model.request_count(), 1);
    assert_eq!(failures, 0);
    assert_eq!(
        plan_micro_call_count(),
        0,
        "驻留 Full 早于 Micro planner，不得先规划 Micro"
    );
}

#[tokio::test]
async fn resident_budget_forces_full_even_when_micro_would_meet_target() {
    let bound = TestSession::open().await;
    let mut transcript = build_transcript(bound.resources(), bound.thread_id.clone());
    let resident = ResidentHistoryPressure::from_transcript(1, &transcript);
    assert!(resident.is_exceeded());

    let config = CompactConfig {
        max_resident_history_bytes: 1,
        ..Default::default()
    };
    let model = RecordingModel::new(false);
    let mut failures = 0u32;
    let facts = exposed_facts(&transcript, &bound.thread_id);
    reset_plan_micro_calls();

    let result = run_compact_with_facts_and_resident(
        &mut transcript,
        Some(&model),
        &config,
        &pressure(0.80),
        false,
        &mut failures,
        "/tmp",
        None,
        &ProtectedResults::default(),
        &facts,
        &ProviderCapabilities::openai(),
        &resident,
    )
    .await;

    assert_eq!(
        result.outcome,
        CompactOutcome::FullApplied,
        "Micro 本可满足目标，但驻留超限必须先 Full"
    );
    assert_eq!(model.request_count(), 1);
    assert_eq!(plan_micro_call_count(), 0);
}

#[tokio::test]
async fn resident_budget_below_limit_keeps_micro_only() {
    let bound = TestSession::open().await;
    let mut transcript = build_transcript(bound.resources(), bound.thread_id.clone());
    let total =
        ResidentHistoryPressure::from_transcript(u64::MAX, &transcript).total_reserved_bytes();
    let resident = ResidentHistoryPressure::from_transcript(total, &transcript);
    assert!(!resident.is_exceeded(), "边界值不触发驻留 Full");

    let config = CompactConfig {
        max_resident_history_bytes: total,
        ..Default::default()
    };
    let model = RecordingModel::new(false);
    let mut failures = 0u32;
    let facts = exposed_facts(&transcript, &bound.thread_id);
    reset_plan_micro_calls();

    let result = run_compact_with_facts_and_resident(
        &mut transcript,
        Some(&model),
        &config,
        &pressure(0.80),
        false,
        &mut failures,
        "/tmp",
        None,
        &ProtectedResults::default(),
        &facts,
        &ProviderCapabilities::openai(),
        &resident,
    )
    .await;

    assert_eq!(result.outcome, CompactOutcome::MicroApplied);
    assert!(result.summary.is_none());
    assert_eq!(model.request_count(), 0, "未超限时不应调用摘要模型");
    assert_eq!(plan_micro_call_count(), 1, "未超限时保留 Micro 规划");
}

#[tokio::test]
async fn resident_full_failure_is_explicit() {
    let bound = TestSession::open().await;
    let mut transcript = build_transcript(bound.resources(), bound.thread_id.clone());
    let resident = ResidentHistoryPressure::from_transcript(1, &transcript);

    let config = CompactConfig {
        max_resident_history_bytes: 1,
        ..Default::default()
    };
    let model = RecordingModel::new(true);
    let mut failures = 0u32;
    let facts = exposed_facts(&transcript, &bound.thread_id);
    reset_plan_micro_calls();

    let result = run_compact_with_facts_and_resident(
        &mut transcript,
        Some(&model),
        &config,
        &pressure(0.0),
        false,
        &mut failures,
        "/tmp",
        None,
        &ProtectedResults::default(),
        &facts,
        &ProviderCapabilities::openai(),
        &resident,
    )
    .await;

    assert_eq!(result.outcome, CompactOutcome::FullFailed);
    assert!(result.failure.is_some(), "Full 失败必须保留结构化原因");
    assert_eq!(
        result.full_escalation_reason,
        Some(FullEscalationReason::ResidentHistoryBudgetExceeded)
    );
    assert_eq!(failures, 1);
    assert_eq!(plan_micro_call_count(), 0);
}

#[tokio::test]
async fn resident_full_residual_is_recorded_after_success() {
    let bound = TestSession::open().await;
    let mut transcript = build_transcript(bound.resources(), bound.thread_id.clone());
    let resident = ResidentHistoryPressure::from_transcript(1, &transcript);

    let config = CompactConfig {
        max_resident_history_bytes: 1,
        ..Default::default()
    };
    let model = RecordingModel::new(false);
    let mut failures = 0u32;
    let facts = exposed_facts(&transcript, &bound.thread_id);
    let (logs, _capture) = capture_logs();

    let result = run_compact_with_facts_and_resident(
        &mut transcript,
        Some(&model),
        &config,
        &pressure(0.0),
        false,
        &mut failures,
        "/tmp",
        None,
        &ProtectedResults::default(),
        &facts,
        &ProviderCapabilities::openai(),
        &resident,
    )
    .await;

    assert_eq!(result.outcome, CompactOutcome::FullApplied);
    let residual = ResidentHistoryPressure::from_transcript(1, &transcript);
    assert!(
        residual.is_exceeded(),
        "1 字节上限在 Full 摘要后仍会被超过，用于验证残余记录"
    );
    let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(
        text.contains("resident_residual_total_reserved_bytes"),
        "Full 后必须记录残余字节: {text}"
    );
}

#[tokio::test]
async fn resident_budget_shadow_reports_without_commit() {
    let bound = TestSession::open().await;
    let mut transcript = build_transcript(bound.resources(), bound.thread_id.clone());
    let resident = ResidentHistoryPressure::from_transcript(1, &transcript);
    let before = transcript.memory_snapshot();

    let config = CompactConfig {
        max_resident_history_bytes: 1,
        shadow_mode_enabled: true,
        ..Default::default()
    };
    let model = RecordingModel::new(false);
    let mut failures = 0u32;
    let facts = exposed_facts(&transcript, &bound.thread_id);
    reset_plan_micro_calls();

    let result = run_compact_with_facts_and_resident(
        &mut transcript,
        Some(&model),
        &config,
        &pressure(0.0),
        false,
        &mut failures,
        "/tmp",
        None,
        &ProtectedResults::default(),
        &facts,
        &ProviderCapabilities::openai(),
        &resident,
    )
    .await;

    assert_eq!(result.outcome, CompactOutcome::Shadowed);
    assert_eq!(result.strategy, CompactStrategy::Skip);
    assert_eq!(
        result.full_escalation_reason,
        Some(FullEscalationReason::ResidentHistoryBudgetExceeded),
        "shadow 必须报告拟执行的 Full 原因"
    );
    assert_eq!(model.request_count(), 0);
    assert_eq!(plan_micro_call_count(), 0);
    assert_eq!(
        transcript.memory_snapshot().total_entries,
        before.total_entries,
        "shadow 不得提交任何变更"
    );
}

#[tokio::test]
async fn resident_budget_auto_disabled_does_not_trigger_full() {
    let bound = TestSession::open().await;
    let mut transcript = build_transcript(bound.resources(), bound.thread_id.clone());
    let resident = ResidentHistoryPressure::from_transcript(1, &transcript);

    let config = CompactConfig {
        auto_compact_enabled: false,
        max_resident_history_bytes: 1,
        ..Default::default()
    };
    let model = RecordingModel::new(false);
    let mut failures = 0u32;
    let facts = exposed_facts(&transcript, &bound.thread_id);
    reset_plan_micro_calls();

    let result = run_compact_with_facts_and_resident(
        &mut transcript,
        Some(&model),
        &config,
        &pressure(0.0),
        false,
        &mut failures,
        "/tmp",
        None,
        &ProtectedResults::default(),
        &facts,
        &ProviderCapabilities::openai(),
        &resident,
    )
    .await;

    assert_eq!(result.outcome, CompactOutcome::Skipped);
    assert_eq!(model.request_count(), 0);
    assert_eq!(plan_micro_call_count(), 0);
}

#[tokio::test]
async fn manual_force_still_precedes_resident_budget() {
    let bound = TestSession::open().await;
    let mut transcript = build_transcript(bound.resources(), bound.thread_id.clone());
    let resident = ResidentHistoryPressure::from_transcript(1, &transcript);

    let config = CompactConfig {
        auto_compact_enabled: false,
        max_resident_history_bytes: 1,
        ..Default::default()
    };
    let model = RecordingModel::new(false);
    let mut failures = 0u32;
    let facts = exposed_facts(&transcript, &bound.thread_id);

    let result = run_compact_with_facts_and_resident(
        &mut transcript,
        Some(&model),
        &config,
        &pressure(0.0),
        true,
        &mut failures,
        "/tmp",
        None,
        &ProtectedResults::default(),
        &facts,
        &ProviderCapabilities::openai(),
        &resident,
    )
    .await;

    assert_eq!(result.outcome, CompactOutcome::FullApplied);
    assert_eq!(
        result.full_escalation_reason,
        Some(FullEscalationReason::ManualForce),
        "manual force 优先于驻留原因"
    );
    assert_eq!(model.request_count(), 1);
}
