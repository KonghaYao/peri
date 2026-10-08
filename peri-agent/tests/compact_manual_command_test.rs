//! Compact 手动命令契约：`/compact` 直接执行路径的结果、诊断与取消语义。
//!
//! 与 `compact_session_adversarial_test` 的分工：本套件只驱动 `execute_compact`
//! 的手动路径（命令结果 / 反馈 / 取消 / 诊断），不经过 ReAct 循环；循环内预算与
//! 投影用例留在那边。夹具按仓库测试规范在文件内局部定义。

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use async_trait::async_trait;
use parking_lot::Mutex;
use peri_acp_types::{
    command::{CommandContext, DependencyBag, FeedbackLevel, PromptStopReason},
    event::{EventSink, ExecutorEvent},
    session_resources::{FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources},
    thread::CancelPolicy,
    workspace::SessionBinding,
};
use peri_agent::{messages::BaseMessage, session::exec::compact_pipeline::execute_compact};
use peri_model::{
    Model, ModelCapabilities, ModelMessage, ModelRequest, ModelResponse, ModelResult, ModelStream,
    StopReason,
};
use tokio_util::sync::CancellationToken;

/// 临时 SQLite 会话夹具（本套件局部定义，见 docs/standards/testing.md §5.1）。
struct BoundSession {
    resources: Arc<dyn SessionResources>,
    thread_id: String,
    cwd: String,
    _directory: tempfile::TempDir,
}

impl BoundSession {
    async fn open() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let db_path = directory.path().join("compact-audit.db");
        let resources: Arc<dyn SessionResources> = Arc::new(
            peri_resources::sessions::SessionResourcesImpl::open(&db_path)
                .await
                .unwrap(),
        );
        let workspace = resources.resolve_workspace(directory.path()).await.unwrap();
        let thread_id = uuid::Uuid::now_v7().to_string();
        let cwd = workspace.cwd.to_string_lossy().into_owned();
        resources
            .create_session(&NewSession {
                thread_id: thread_id.clone(),
                created_at: "2026-09-28T00:00:00Z".into(),
                meta: NewSessionMeta {
                    title: Some("compact audit".into()),
                    cwd: cwd.clone(),
                    parent_thread_id: None,
                    hidden: false,
                    cancel_policy: CancelPolicy::default(),
                    snapshot_at_message_id: None,
                },
                binding: SessionBinding::from_workspace(&workspace),
                frozen: FrozenSnapshotBytes::new("{\"version\":1,\"test\":true}"),
            })
            .await
            .unwrap();
        Self {
            resources,
            thread_id,
            cwd,
            _directory: directory,
        }
    }
}

struct SummaryModel {
    failures: usize,
    calls: AtomicUsize,
    cancel: Option<CancellationToken>,
}

#[async_trait]
impl Model for SummaryModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
    async fn stream(&self, _: ModelRequest, _: CancellationToken) -> ModelResult<ModelStream> {
        unreachable!("摘要必须走 complete")
    }
    async fn complete(&self, _: ModelRequest, _: CancellationToken) -> ModelResult<ModelResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
            std::future::pending::<()>().await;
        }
        ModelResponse::new(
            ModelMessage::assistant_text(if call < self.failures {
                "<analysis>no summary</analysis>"
            } else {
                "<summary>RECOVERED: continue the work.</summary>"
            }),
            StopReason::EndTurn,
            None,
            None,
        )
    }
}

#[derive(Default)]
struct RecordingSink(Mutex<Vec<ExecutorEvent>>);

#[async_trait]
impl EventSink for RecordingSink {
    async fn push_event(&self, _: &str, event: &ExecutorEvent, _: u32) {
        self.0.lock().push(event.clone());
    }
    async fn push_done(&self, _: &str, _: &str, _: Option<&str>) {}
}

async fn manual_case(
    failures: usize,
    cancel_during_summary: bool,
) -> (
    BoundSession,
    peri_acp_types::command::CommandResult,
    Arc<SummaryModel>,
    Arc<RecordingSink>,
    Vec<BaseMessage>,
) {
    let token = CancellationToken::new();
    let model = Arc::new(SummaryModel {
        failures,
        calls: AtomicUsize::new(0),
        cancel: cancel_during_summary.then(|| token.clone()),
    });
    let (bound, result, sink, history) = manual_with_model(model.clone(), token).await;
    (bound, result, model, sink, history)
}

async fn manual_with_model(
    model: Arc<dyn Model>,
    token: CancellationToken,
) -> (
    BoundSession,
    peri_acp_types::command::CommandResult,
    Arc<RecordingSink>,
    Vec<BaseMessage>,
) {
    let bound = BoundSession::open().await;
    let history = vec![
        BaseMessage::human("retain original task"),
        BaseMessage::ai("retain original answer"),
    ];
    let sink = Arc::new(RecordingSink::default());
    let mut command = CommandContext::new(
        bound.thread_id.clone(),
        history.clone(),
        bound.cwd.clone(),
        sink.clone(),
        token,
        DependencyBag::new(),
    );
    command.auxiliary_model = Some(model.clone());
    command.session_resources = Some(bound.resources.clone());
    command.thread_id = Some(bound.thread_id.clone());
    let result = execute_compact(command).await;
    (bound, result, sink, history)
}

#[tokio::test]
async fn test_compact_session_manual_empty_summary_retries_without_next_prompt() {
    let (bound, result, model, sink, history) = manual_case(2, false).await;
    assert_eq!(model.calls.load(Ordering::SeqCst), 3);
    assert!(matches!(result.stop_reason, PromptStopReason::EndTurn));
    assert!(matches!(
        result.feedback.unwrap().level,
        FeedbackLevel::Info
    ));
    assert!(result.messages[0].content().contains("RECOVERED"));
    let snapshot = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    for original in history {
        assert!(snapshot.flags[&original.id()].excluded);
    }
    let events = sink.0.lock();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ExecutorEvent::CompactStarted { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ExecutorEvent::CompactCompleted { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn test_compact_session_manual_failure_preserves_history_and_reports_feedback() {
    let (bound, result, model, sink, history) = manual_case(usize::MAX, false).await;
    assert_eq!(model.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        serde_json::to_value(&result.messages).unwrap(),
        serde_json::to_value(&history).unwrap()
    );
    let feedback = result.feedback.unwrap();
    assert!(matches!(feedback.level, FeedbackLevel::Error));
    assert_eq!(
        feedback.message,
        "Full Compact failed after 3 attempts. Retry or change the compact model."
    );
    let snapshot = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert!(snapshot.flags.values().all(|flag| !flag.excluded));
    assert_eq!(snapshot.payloads.len(), history.len());
    assert_eq!(
        sink.0.lock().len(),
        1,
        "当前失败仅发送 Started，反馈由命令编排投影"
    );
}

#[tokio::test]
async fn test_compact_session_manual_cancel_preserves_history() {
    let (bound, result, model, sink, history) = manual_case(0, true).await;
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    assert!(matches!(result.stop_reason, PromptStopReason::Cancelled));
    assert_eq!(
        serde_json::to_value(&result.messages).unwrap(),
        serde_json::to_value(&history).unwrap()
    );
    let feedback = result.feedback.unwrap();
    assert!(matches!(feedback.level, FeedbackLevel::Warning));
    assert_eq!(feedback.message, "compact cancelled");
    let snapshot = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert!(snapshot.flags.values().all(|flag| !flag.excluded));
    assert_eq!(snapshot.payloads.len(), history.len());
    assert_eq!(
        sink.0.lock().len(),
        1,
        "当前取消仅发送 Started，上游用 TurnDone 结束 loading"
    );
}

struct RejectedSummary {
    protocol: bool,
    cancel: Option<CancellationToken>,
}

#[async_trait]
impl Model for RejectedSummary {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
    async fn stream(&self, _: ModelRequest, _: CancellationToken) -> ModelResult<ModelStream> {
        unreachable!("摘要必须走 complete")
    }
    async fn complete(&self, _: ModelRequest, _: CancellationToken) -> ModelResult<ModelResponse> {
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
        Err(if self.protocol {
            peri_model::ModelError::protocol_with_summary(
                peri_model::ProtocolErrorKind::InvalidJsonObject,
                "fixture-private-body: sk-not-a-real-key https://private.example/path",
            )
        } else {
            peri_model::ModelError::http_status(401, "fixture", Some("req-compact-401"))
        })
    }
}

/// [回归测试] 手动 compact 必须保留逐字分类诊断（含 provider 标识），并保全原历史。
///
/// 现行权威 ARC-SECRET-001（2026-10-06 修订，见 `2026-10-06-p0-agent-internal-error-diagnostic-loss`）：
/// 运行时诊断不因 token/URL/路径形状遮蔽或替换实际错误信息，因此这里锁定
/// 「分类事实 + provider 标识」逐字存在，而不是旧版的脱敏后措辞。
#[tokio::test]
async fn test_compact_session_manual_http_failure_reports_classified_cause() {
    let (bound, result, _, history) = manual_with_model(
        Arc::new(RejectedSummary {
            protocol: false,
            cancel: None,
        }),
        CancellationToken::new(),
    )
    .await;
    let feedback = result.feedback.unwrap();
    assert!(matches!(feedback.level, FeedbackLevel::Error));
    assert_eq!(
        feedback.message,
        "An LLM API error occurred (HTTP 401, request id: req-compact-401, provider: fixture). Please try again."
    );
    assert_eq!(
        serde_json::to_value(result.messages).unwrap(),
        serde_json::to_value(history).unwrap()
    );
    let snapshot = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert!(snapshot.flags.values().all(|flag| !flag.excluded));
}

/// [回归测试] 协议诊断保留 allowlist 事实集合与逐字实际错误文本。
///
/// 同 ARC-SECRET-001（2026-10-06 修订）：协议分类与 `message` 事实并存，
/// 不以上下文形状替换底层原因；旧用例断言的「遮蔽 provider 正文」已不是
/// 现行契约。这里换成更明确的锁定：分类事实与有界实际文本都必须出现。
#[tokio::test]
async fn test_compact_session_manual_protocol_failure_preserves_classified_cause() {
    let (_, result, _, _) = manual_with_model(
        Arc::new(RejectedSummary {
            protocol: true,
            cancel: None,
        }),
        CancellationToken::new(),
    )
    .await;
    let feedback = result.feedback.unwrap();
    assert!(matches!(feedback.level, FeedbackLevel::Error));
    assert_eq!(
        feedback.message,
        "An LLM API error occurred (protocol failure: invalid JSON object, message: fixture-private-body: sk-not-a-real-key https://private.example/path). Please try again."
    );
}

/// [回归测试] provider 同 poll 返回错误并触发取消时，终态仍必须为取消。
#[tokio::test]
async fn test_compact_session_manual_cancel_wins_over_ready_provider_error() {
    let token = CancellationToken::new();
    let (_, result, _, history) = manual_with_model(
        Arc::new(RejectedSummary {
            protocol: false,
            cancel: Some(token.clone()),
        }),
        token,
    )
    .await;
    assert!(matches!(result.stop_reason, PromptStopReason::Cancelled));
    assert_eq!(result.feedback.unwrap().message, "compact cancelled");
    assert_eq!(
        serde_json::to_value(result.messages).unwrap(),
        serde_json::to_value(history).unwrap()
    );
}
