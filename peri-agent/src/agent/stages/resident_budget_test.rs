//! Compact 阶段的独立驻留字节预算触发、边界与失败传播。
//!
//! 全部使用小阈值（1 字节或快照算出的实际容量），不分配 128 MiB。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_model::{
    Model, ModelCapabilities, ModelError, ModelMessage, ModelRequest, ModelResponse, ModelResult,
    ModelStream, StopReason, TokenUsage,
};
use tokio_util::sync::CancellationToken;

use crate::agent::compact_v2::config::CompactConfig;
use crate::agent::compact_v2::planner::ResidentHistoryPressure;
use crate::agent::model_bridge::AgentModelBridge;
use crate::agent::stages::reason::run_reason;
use crate::agent::stages::{compact, CompactInput, ReasonInput, StageContext};
use crate::agent::token::ContextBudget;
use crate::messages::{BaseMessage, MessageContent};
use crate::middleware::capabilities as hook_state;
use crate::middleware::Middleware;
use crate::session::store::FrozenContext;
use crate::session::test_resources::mock::{MockSessionResources, ResumeLoadGate};
use crate::session::test_resources::TestSession;
use crate::session::{MessageTranscript, Session};
use crate::tools::{BaseTool, ContextRetention, ToolContext};

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

fn append_resident_exchanges(transcript: &mut MessageTranscript) {
    for index in 0..8 {
        transcript.append(make_human(&format!("q {index}")));
        transcript.append(make_ai_with_tool(
            "",
            "Bash",
            &format!("stage_resident_call_{index}"),
        ));
        transcript.append(make_tool_result(
            &format!("stage_resident_call_{index}"),
            &format!("stage resident output {index}"),
        ));
    }
}

fn build_transcript(bound: &TestSession) -> MessageTranscript {
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    append_resident_exchanges(&mut transcript);
    transcript
}

struct RecordingModel {
    requests: AtomicUsize,
    fail: bool,
    summary: String,
}

impl RecordingModel {
    fn new(fail: bool) -> Arc<Self> {
        Self::with_summary(fail, "stage resident full summary")
    }

    fn with_summary(fail: bool, summary: &str) -> Arc<Self> {
        Arc::new(Self {
            requests: AtomicUsize::new(0),
            fail,
            summary: summary.to_string(),
        })
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
            return Err(ModelError::http_status(
                500,
                "resident-stage-test",
                None::<&str>,
            ));
        }
        Ok(ModelResponse::new(
            ModelMessage::assistant_text(format!("<summary>{}</summary>", self.summary)),
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

fn build_context(
    bound: &TestSession,
    transcript: MessageTranscript,
    config: CompactConfig,
    model: Arc<RecordingModel>,
) -> (Arc<Session>, StageContext) {
    let session = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some(bound.thread_id.clone()),
    );
    *session.transcript().write() = transcript;
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(AgentModelBridge::from_arc(model.clone())))
    .with_compact_llm(model)
    .with_compact_config(config)
    .with_context_budget(ContextBudget::new(200_000))
    .build();
    (session, context)
}

async fn run_stage(context: &StageContext) -> crate::error::AgentResult<super::CompactOutput> {
    compact::run_compact(CompactInput {
        context: context.clone(),
        has_tool_calls: false,
    })
    .await
}

fn resident_config(limit: u64) -> CompactConfig {
    CompactConfig {
        max_resident_history_bytes: limit,
        ..Default::default()
    }
}

/// 继承区主导的夹具用的小预算：不分配 128 MiB，靠 `limit` 与继承区容量区分。
const RESIDUAL_LIMIT: u64 = 1_000;

fn make_small_tool_call(tool_id: &str) -> BaseMessage {
    BaseMessage::ai_with_tool_calls(
        MessageContent::text(String::new()),
        vec![crate::messages::ToolCallRequest::new(
            tool_id,
            "Bash",
            serde_json::json!({}),
        )],
    )
}

fn make_small_tool_result(tool_call_id: &str, body: &str) -> BaseMessage {
    BaseMessage::tool_result(
        tool_call_id.to_string(),
        MessageContent::text(body.to_string()),
    )
}

fn append_small_tool_exchange(transcript: &mut MessageTranscript, round: usize) {
    let id = format!("residual_call_{round}");
    transcript.append(make_small_tool_call(&id));
    transcript.append(make_small_tool_result(
        &id,
        &format!("small result {round}"),
    ));
}

/// 继承区本身超过预算、自有区很小的 transcript：Full 只能回收自有区。
fn build_residual_dominant_transcript(bound: &TestSession, limit: u64) -> MessageTranscript {
    let inherited = make_human(&"i".repeat(limit as usize * 3));
    let mut transcript = MessageTranscript::new()
        .with_persistence(bound.resources(), bound.thread_id.clone())
        .with_ancestor(vec![inherited]);
    transcript.append(make_human("own start"));
    transcript
}

/// own 只有 Full 不归档的 System（没有继承区）：残余完全来自不可回收 own。
fn build_irreducible_own_transcript(bound: &TestSession, limit: u64) -> MessageTranscript {
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    transcript.append(BaseMessage::system("s".repeat(limit as usize * 3)));
    transcript.append(make_human("own start"));
    transcript
}

#[tokio::test]
async fn resident_budget_forces_full_without_usage() {
    let bound = TestSession::open().await;
    let transcript = build_transcript(&bound);
    let resident = ResidentHistoryPressure::from_transcript(1, &transcript);
    assert!(resident.is_exceeded());

    let model = RecordingModel::new(false);
    let (session, context) = build_context(&bound, transcript, resident_config(1), model.clone());

    let output = run_stage(&context).await.unwrap();
    assert!(output.compacted, "无 usage、低 token 时驻留超限仍须 Full");
    assert_eq!(model.request_count(), 1);
    let view = session
        .transcript()
        .read()
        .visible_model_messages()
        .unwrap();
    assert!(
        view.iter()
            .any(|message| message.content().contains("stage resident full summary")),
        "Full 摘要必须进入可见历史"
    );
}

#[tokio::test]
async fn resident_budget_forces_full_with_consumed_pressure_sample() {
    let bound = TestSession::open().await;
    let transcript = build_transcript(&bound);
    let model = RecordingModel::new(false);
    let (_session, context) = build_context(&bound, transcript, resident_config(1), model.clone());

    // 构造一个已消费的 token 压力样本：普通 token 路径会在此 Skip。
    {
        let mut tracker = context.compact.token_tracker.write();
        tracker.accumulate(&TokenUsage {
            input_tokens: 1_000,
            output_tokens: 0,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
    }
    let key = context
        .compact
        .token_tracker
        .read()
        .pressure_sample_key()
        .expect("fixture 必须产生压力样本");
    context
        .compact
        .token_tracker
        .write()
        .consume_pressure_sample(key);
    assert!(context
        .compact
        .token_tracker
        .read()
        .is_pressure_sample_consumed(key));

    let output = run_stage(&context).await.unwrap();
    assert!(output.compacted, "已消费样本不得阻断驻留 Full");
    assert_eq!(model.request_count(), 1);
}

#[tokio::test]
async fn resident_budget_boundary_is_strictly_greater() {
    let bound = TestSession::open().await;
    let transcript = build_transcript(&bound);
    let total =
        ResidentHistoryPressure::from_transcript(u64::MAX, &transcript).total_reserved_bytes();
    let model = RecordingModel::new(false);
    let (_session, mut context) =
        build_context(&bound, transcript, resident_config(total), model.clone());

    let at_limit = run_stage(&context).await.unwrap();
    assert!(!at_limit.compacted, "恰好等于上限不得触发 Full");
    assert_eq!(model.request_count(), 0);

    context.compact.compact_config = Some(resident_config(total.saturating_sub(1)));
    let over_limit = run_stage(&context).await.unwrap();
    assert!(over_limit.compacted, "超过上限 1 字节必须触发 Full");
    assert_eq!(model.request_count(), 1);
}

#[tokio::test]
async fn resident_full_failure_propagates_at_low_token() {
    let bound = TestSession::open().await;
    let transcript = build_transcript(&bound);
    let model = RecordingModel::new(true);
    let (_session, context) = build_context(&bound, transcript, resident_config(1), model.clone());

    let error = match run_stage(&context).await {
        Ok(_) => panic!("低 token 下驻留 Full 失败不得被吞掉"),
        Err(error) => error,
    };
    let message = format!("{error}");
    assert!(!message.is_empty());
    assert_eq!(model.request_count(), 1);
}

#[tokio::test]
async fn resident_budget_shadow_reports_without_commit() {
    let bound = TestSession::open().await;
    let transcript = build_transcript(&bound);
    let before = transcript.memory_snapshot();
    let model = RecordingModel::new(false);
    let config = CompactConfig {
        max_resident_history_bytes: 1,
        shadow_mode_enabled: true,
        ..Default::default()
    };
    let (session, context) = build_context(&bound, transcript, config, model.clone());
    let (logs, _capture) = capture_logs();

    let output = run_stage(&context).await.unwrap();
    assert!(!output.compacted, "shadow 不得提交 Full");
    assert_eq!(model.request_count(), 0);
    assert_eq!(
        session.transcript().read().memory_snapshot().total_entries,
        before.total_entries,
        "shadow 不得改动 transcript"
    );
    let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(
        text.contains("Shadow mode: resident Full Compact planned"),
        "shadow 必须报告拟执行的 Full: {text}"
    );
}

#[tokio::test]
async fn resident_budget_auto_disabled_does_not_trigger_full() {
    let bound = TestSession::open().await;
    let transcript = build_transcript(&bound);
    let model = RecordingModel::new(false);
    let config = CompactConfig {
        auto_compact_enabled: false,
        max_resident_history_bytes: 1,
        ..Default::default()
    };
    let (_session, context) = build_context(&bound, transcript, config, model.clone());

    let output = run_stage(&context).await.unwrap();
    assert!(!output.compacted);
    assert_eq!(model.request_count(), 0);
}

#[tokio::test]
async fn resident_full_residual_is_recorded() {
    let bound = TestSession::open().await;
    let transcript = build_transcript(&bound);
    let model = RecordingModel::new(false);
    let (_session, context) = build_context(&bound, transcript, resident_config(1), model.clone());
    let (logs, _capture) = capture_logs();

    let output = run_stage(&context).await.unwrap();
    assert!(output.compacted);
    let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(
        text.contains("resident_residual_total_reserved_bytes"),
        "Full 后必须记录残余字节: {text}"
    );
}

/// 同一工作单元只强制一次驻留 Full；Full 追加的摘要与新提醒不算新工作。
#[tokio::test]
async fn resident_full_is_not_repeated_without_new_work() {
    let bound = TestSession::open().await;
    let transcript = build_transcript(&bound);
    let model = RecordingModel::new(false);
    let (session, context) = build_context(&bound, transcript, resident_config(1), model.clone());

    let first = run_stage(&context).await.unwrap();
    assert!(first.compacted, "首次驻留超限必须 Full");
    assert_eq!(model.request_count(), 1);

    let second = run_stage(&context).await.unwrap();
    assert!(!second.compacted, "无新 Human/Tool 时不得重复驻留 Full");
    assert_eq!(model.request_count(), 1, "第二次运行不得再次调用摘要模型");

    {
        let transcript_handle = session.transcript();
        let mut transcript = transcript_handle.write();
        transcript.append(make_human("新请求"));
        transcript.append(make_ai_with_tool("", "Bash", "stage_resident_new_call"));
        transcript.append(make_tool_result("stage_resident_new_call", "new output"));
    }
    let third = run_stage(&context).await.unwrap();
    assert!(third.compacted, "出现新 Human/Tool 后应可再次 Full");
    assert_eq!(model.request_count(), 2, "新工作单元允许第二次 Full");
}

/// 回归：继承区本身超过预算时，Full 无法把总量降回预算内。此后连续新增的小工具
/// 结果虽构成 new_work，也不得反复 Full；否则子 Agent 每轮都支付摘要延迟与模型成本。
#[tokio::test]
async fn resident_inherited_residual_does_not_repeat_full_for_small_new_work() {
    let bound = TestSession::open().await;
    let transcript = build_residual_dominant_transcript(&bound, RESIDUAL_LIMIT);
    let pressure = ResidentHistoryPressure::from_transcript(RESIDUAL_LIMIT, &transcript);
    assert!(
        pressure.irreducible_inherited_reserved_bytes() > RESIDUAL_LIMIT,
        "夹具必须让继承区本身超过完整预算"
    );
    assert!(
        pressure.reclaimable_own_reserved_bytes() < RESIDUAL_LIMIT,
        "夹具自有区必须小于完整预算"
    );

    let model = RecordingModel::new(false);
    let (session, context) = build_context(
        &bound,
        transcript,
        resident_config(RESIDUAL_LIMIT),
        model.clone(),
    );

    let first = run_stage(&context).await.unwrap();
    assert!(first.compacted, "首次超限必须执行一次 Full");
    assert_eq!(model.request_count(), 1);

    for round in 0..5 {
        {
            let handle = session.transcript();
            let mut transcript = handle.write();
            append_small_tool_exchange(&mut transcript, round);
        }
        let output = run_stage(&context).await.unwrap();
        assert!(
            !output.compacted,
            "第 {round} 轮：Full 后残余主导，小工具结果不得触发 Full"
        );
        assert_eq!(
            model.request_count(),
            1,
            "第 {round} 轮：不得再次调用摘要模型"
        );
    }
}

/// 同一残余主导场景下，可回收自有区自身超过完整预算后才允许下一次 Full。
#[tokio::test]
async fn resident_full_resumes_after_own_region_exceeds_limit() {
    let bound = TestSession::open().await;
    let transcript = build_residual_dominant_transcript(&bound, RESIDUAL_LIMIT);
    let model = RecordingModel::new(false);
    let (session, context) = build_context(
        &bound,
        transcript,
        resident_config(RESIDUAL_LIMIT),
        model.clone(),
    );

    let first = run_stage(&context).await.unwrap();
    assert!(first.compacted, "首次超限必须执行一次 Full");
    assert_eq!(model.request_count(), 1);

    {
        let handle = session.transcript();
        let mut transcript = handle.write();
        transcript.append(make_ai_with_tool("", "Bash", "own_growth_call"));
        transcript.append(BaseMessage::tool_result(
            "own_growth_call".to_string(),
            MessageContent::text("o".repeat(RESIDUAL_LIMIT as usize + 400)),
        ));
    }
    let pressure =
        ResidentHistoryPressure::from_transcript(RESIDUAL_LIMIT, &session.transcript().read());
    assert!(
        pressure.reclaimable_exceeds_limit(),
        "自有区必须超过完整预算才允许恢复 Full"
    );

    let second = run_stage(&context).await.unwrap();
    assert!(second.compacted, "自有区超过完整预算后应再次 Full");
    assert_eq!(model.request_count(), 2);
}

/// 回归：Full 会保留 System（以及 protected 后台结果）。这些不可回收 own 超过预算
/// 时，Full 后残余仍超限，但连续小工具结果不得反复 Full；只有可回收 own 自身
/// 超过完整预算才允许下一次 Full。
#[tokio::test]
async fn resident_irreducible_own_does_not_repeat_full_for_small_new_work() {
    let bound = TestSession::open().await;
    let transcript = build_irreducible_own_transcript(&bound, RESIDUAL_LIMIT);
    let pressure = ResidentHistoryPressure::from_transcript(RESIDUAL_LIMIT, &transcript);
    assert!(
        pressure.irreducible_own_reserved_bytes() > RESIDUAL_LIMIT,
        "夹具必须让 System 本身超过完整预算"
    );
    assert!(
        !pressure.reclaimable_exceeds_limit(),
        "可回收 own 必须小于完整预算"
    );
    assert!(pressure.is_exceeded(), "总量仍须超限");

    let model = RecordingModel::new(false);
    let (session, context) = build_context(
        &bound,
        transcript,
        resident_config(RESIDUAL_LIMIT),
        model.clone(),
    );

    let first = run_stage(&context).await.unwrap();
    assert!(first.compacted, "首次超限必须执行一次 Full");
    assert_eq!(model.request_count(), 1);

    let after_full =
        ResidentHistoryPressure::from_transcript(RESIDUAL_LIMIT, &session.transcript().read());
    assert!(
        after_full.irreducible_own_reserved_bytes() > RESIDUAL_LIMIT,
        "Full 必须保留 System"
    );
    assert!(
        after_full.reclaimable_own_reserved_bytes() < RESIDUAL_LIMIT,
        "Full 后残留的可回收正文必须低于预算"
    );

    for round in 0..5 {
        {
            let handle = session.transcript();
            let mut transcript = handle.write();
            append_small_tool_exchange(&mut transcript, round);
        }
        let output = run_stage(&context).await.unwrap();
        assert!(
            !output.compacted,
            "第 {round} 轮：不可回收 own 主导，小工具结果不得触发 Full"
        );
        assert_eq!(
            model.request_count(),
            1,
            "第 {round} 轮：不得再次调用摘要模型"
        );
    }

    {
        let handle = session.transcript();
        let mut transcript = handle.write();
        transcript.append(make_ai_with_tool("", "Bash", "own_reclaimable_growth"));
        transcript.append(BaseMessage::tool_result(
            "own_reclaimable_growth".to_string(),
            MessageContent::text("r".repeat(RESIDUAL_LIMIT as usize + 400)),
        ));
    }
    let grown =
        ResidentHistoryPressure::from_transcript(RESIDUAL_LIMIT, &session.transcript().read());
    assert!(
        grown.reclaimable_exceeds_limit(),
        "可回收 own 超过完整预算后才允许恢复 Full"
    );

    let resumed = run_stage(&context).await.unwrap();
    assert!(resumed.compacted, "可回收 own 超过完整预算后应再次 Full");
    assert_eq!(model.request_count(), 2);
}

/// 快照失效与已提交取消路径见 `resident_residual_test` 子模块。
#[path = "resident_residual_test.rs"]
mod residual_tests;

/// 防过度修改：继承区低于预算时，一次成功 Full 回到预算内，之后再次超限仍按
/// 「总量超限且存在新工作」触发 Full，而不是要求自有区单独超过预算。
#[tokio::test]
async fn resident_below_inherited_limit_keeps_new_work_gate() {
    const LIMIT: u64 = 2_000;
    let bound = TestSession::open().await;
    let mut transcript = MessageTranscript::new()
        .with_persistence(bound.resources(), bound.thread_id.clone())
        .with_ancestor(vec![make_human(&"i".repeat(400))]);
    transcript.append(make_human(&"o".repeat(1_800)));
    let pressure = ResidentHistoryPressure::from_transcript(LIMIT, &transcript);
    assert!(
        pressure.irreducible_inherited_reserved_bytes() < LIMIT,
        "夹具继承区必须低于预算"
    );
    assert!(pressure.is_exceeded(), "夹具初始总量必须超限");

    let model = RecordingModel::new(false);
    let (session, context) =
        build_context(&bound, transcript, resident_config(LIMIT), model.clone());

    let first = run_stage(&context).await.unwrap();
    assert!(first.compacted, "首次超限必须 Full");
    assert_eq!(model.request_count(), 1);
    let after_full = ResidentHistoryPressure::from_transcript(LIMIT, &session.transcript().read());
    assert!(
        !after_full.is_exceeded(),
        "继承区低于预算时 Full 后总量应回到预算内"
    );

    {
        let handle = session.transcript();
        let mut transcript = handle.write();
        transcript.append(make_ai_with_tool("", "Bash", "below_limit_call"));
        transcript.append(make_small_tool_result(
            "below_limit_call",
            &"n".repeat(1_000),
        ));
    }
    let before_second =
        ResidentHistoryPressure::from_transcript(LIMIT, &session.transcript().read());
    assert!(before_second.is_exceeded(), "新增工作后总量必须再次超限");
    assert!(
        !before_second.reclaimable_exceeds_limit(),
        "自有区本身仍未超过完整预算"
    );

    let second = run_stage(&context).await.unwrap();
    assert!(
        second.compacted,
        "继承区低于预算时，新工作触发的总量超限仍应 Full"
    );
    assert_eq!(model.request_count(), 2);
}

/// Shadow 只拟执行、不提交，因此不得更新「上次应用 Full 后的压力快照」。
#[tokio::test]
async fn resident_shadow_does_not_record_pressure_snapshot() {
    let bound = TestSession::open().await;
    let transcript = build_residual_dominant_transcript(&bound, RESIDUAL_LIMIT);
    let model = RecordingModel::new(false);
    let config = CompactConfig {
        max_resident_history_bytes: RESIDUAL_LIMIT,
        shadow_mode_enabled: true,
        ..Default::default()
    };
    let (_session, mut context) = build_context(&bound, transcript, config, model.clone());

    let shadowed = run_stage(&context).await.unwrap();
    assert!(!shadowed.compacted, "shadow 不得提交 Full");
    assert_eq!(model.request_count(), 0);

    context.compact.compact_config = Some(resident_config(RESIDUAL_LIMIT));
    let applied = run_stage(&context).await.unwrap();
    assert!(
        applied.compacted,
        "shadow 未记录 Full 后快照，下一次超限必须按首次门控执行 Full"
    );
    assert_eq!(model.request_count(), 1);
}

struct ArchiveReadTool;

#[async_trait]
impl BaseTool for ArchiveReadTool {
    fn name(&self) -> &str {
        "ReadContextArchive"
    }
    fn description(&self) -> &str {
        "bounded historical recovery"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    fn is_direct(&self) -> bool {
        true
    }
    fn context_retention(&self) -> ContextRetention {
        ContextRetention::ArchiveRead
    }
    async fn invoke(
        &self,
        _: serde_json::Value,
        _: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        unreachable!("resident fixture never executes archive reads")
    }
}

/// 驻留超限必须先于 Micro facts 探测：远程 archive metadata 失败不得把 Full 提前
/// 变成 `micro_failure`。
#[tokio::test]
async fn resident_full_does_not_probe_archive_metadata() {
    let bound = TestSession::open().await;
    let store = MockSessionResources::new();
    let (entered, _entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    release_tx.send(()).unwrap();
    *store.archive_probe_gate.lock().unwrap() = Some(ResumeLoadGate {
        entered,
        release: release_rx,
        dropped: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    });
    let mut transcript =
        MessageTranscript::new().with_persistence(store.clone(), "resident-archive-failure".into());
    append_resident_exchanges(&mut transcript);
    let resident = ResidentHistoryPressure::from_transcript(1, &transcript);
    assert!(resident.is_exceeded());

    let model = RecordingModel::new(false);
    let (_session, context) = build_context(&bound, transcript, resident_config(1), model.clone());
    context.runtime.tools.write().insert(
        "ReadContextArchive".into(),
        Arc::new(ArchiveReadTool) as Arc<dyn BaseTool>,
    );

    let output = run_stage(&context).await.unwrap();
    assert!(output.compacted, "archive metadata 失败不得阻断驻留 Full");
    assert_eq!(model.request_count(), 1);
    assert!(
        store.archive_probe_gate.lock().unwrap().is_some(),
        "驻留路径不得消费 archive metadata 探测"
    );
}

struct FinalAnswerLlm;

#[async_trait]
impl crate::agent::react::ReactLLM for FinalAnswerLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        Ok(crate::agent::react::Reasoning::with_answer(
            "post-check reasoning",
            "done",
        ))
    }

    fn model_name(&self) -> String {
        "resident-reason-fixture".into()
    }
}

struct AppendHumanBeforeModel;

#[async_trait]
impl Middleware for AppendHumanBeforeModel {
    fn name(&self) -> &str {
        "AppendHumanBeforeModel"
    }

    async fn before_model(
        &self,
        state: &mut dyn hook_state::BeforeModelState,
    ) -> crate::error::AgentResult<()> {
        state.add_message(BaseMessage::human("hooks injected input"));
        Ok(())
    }
}

fn reason_context(
    bound: &TestSession,
    transcript: MessageTranscript,
    limit: u64,
    compact_model: Arc<RecordingModel>,
    hook: bool,
) -> StageContext {
    let (_session, mut context) =
        build_context(bound, transcript, resident_config(limit), compact_model);
    context.runtime.llm = Arc::new(FinalAnswerLlm);
    if hook {
        let mut chain = crate::middleware::MiddlewareChain::new();
        chain.add(Box::new(AppendHumanBeforeModel));
        context.runtime.middleware_chain = Arc::new(chain);
    }
    context
}

/// hooks 后的 canonical 变化是独立的驻留补检证据：不要求投影 token 增长。
#[tokio::test]
async fn reason_rechecks_resident_after_hooks_canonical_change() {
    let bound = TestSession::open().await;
    let compact_model = RecordingModel::new(false);
    let context = reason_context(
        &bound,
        build_transcript(&bound),
        1,
        compact_model.clone(),
        true,
    );

    run_reason(ReasonInput {
        context,
        has_tool_calls: false,
    })
    .await
    .unwrap();
    assert_eq!(
        compact_model.request_count(),
        1,
        "hooks 后新增 canonical 且驻留超限必须补检一次 Full"
    );
}

#[tokio::test]
async fn reason_skips_resident_recheck_without_canonical_change() {
    let bound = TestSession::open().await;
    let compact_model = RecordingModel::new(false);
    let context = reason_context(
        &bound,
        build_transcript(&bound),
        1,
        compact_model.clone(),
        false,
    );

    run_reason(ReasonInput {
        context,
        has_tool_calls: false,
    })
    .await
    .unwrap();
    assert_eq!(
        compact_model.request_count(),
        0,
        "canonical 未变化时不得仅因驻留超限补检"
    );
}

#[tokio::test]
async fn reason_skips_resident_recheck_below_limit() {
    let bound = TestSession::open().await;
    let compact_model = RecordingModel::new(false);
    let context = reason_context(
        &bound,
        build_transcript(&bound),
        u64::MAX,
        compact_model.clone(),
        true,
    );

    run_reason(ReasonInput {
        context,
        has_tool_calls: false,
    })
    .await
    .unwrap();
    assert_eq!(
        compact_model.request_count(),
        0,
        "hooks 改变 canonical 但未超限时不得补检 Full"
    );
}

/// `context_budget` 缺失只关闭 token 压力，不得关闭与之独立的驻留字节门控。
#[tokio::test]
async fn resident_budget_without_context_budget_still_full() {
    let bound = TestSession::open().await;
    let transcript = build_transcript(&bound);
    let model = RecordingModel::new(false);
    let (_session, mut context) =
        build_context(&bound, transcript, resident_config(1), model.clone());
    context.compact.context_budget = None;

    let output = run_stage(&context).await.unwrap();
    assert!(
        output.compacted,
        "context_budget 缺失时驻留超限仍须执行 Full"
    );
    assert_eq!(model.request_count(), 1);

    // 未超限时保持原行为：无 token 压力、无驻留压力 → Skip，不调用模型。
    let bound = TestSession::open().await;
    let transcript = build_transcript(&bound);
    let model = RecordingModel::new(false);
    let (_session, mut context) =
        build_context(&bound, transcript, resident_config(u64::MAX), model.clone());
    context.compact.context_budget = None;

    let output = run_stage(&context).await.unwrap();
    assert!(
        !output.compacted,
        "context_budget 缺失且未超限时必须保持 Skip"
    );
    assert_eq!(model.request_count(), 0);
}
