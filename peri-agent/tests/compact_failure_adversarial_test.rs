//! 独立对抗审查：Full 失败必须阻止未恢复预算的 Reason，并保全真实持久化历史。

use peri_acp_types::session_resources::{
    FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources,
};
use peri_acp_types::workspace::{SessionBinding, SESSION_BINDING_VERSION};
use peri_agent::agent::compact_v2::CompactConfig;
use peri_agent::agent::events_v2::{EventBus, EventHandles, ObserveEvent};
use peri_agent::agent::react::{ReactLLM, Reasoning, StreamingContext};
use peri_agent::agent::stages::{run_react_loop, LoopResult, StageContext};
use peri_agent::agent::token::ContextBudget;
use peri_agent::error::{AgentError, AgentResult};
use peri_agent::messages::{BaseMessage, ToolCallRequest};
use peri_agent::session::{FrozenContext, MessageSource, QueuedMessage, Session};
use peri_agent::tools::BaseTool;
use peri_model::{
    ModelCapabilities, ModelError, ModelMessage, ModelRequest, ModelResponse, ModelStream,
    StopReason, TokenUsage,
};
use peri_resources::sessions::SessionResourcesImpl;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio_util::sync::CancellationToken;

struct BoundSession {
    resources: Arc<dyn SessionResources>,
    thread_id: String,
    db: tempfile::TempDir,
    repo: tempfile::TempDir,
}

impl BoundSession {
    async fn open() -> Self {
        let repo = tempfile::tempdir().unwrap();
        for args in [
            vec!["init", "-q"],
            vec![
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-qm",
                "base",
            ],
        ] {
            let result = std::process::Command::new("git")
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .env("HOME", repo.path())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .arg("-C")
                .arg(repo.path())
                .args(args)
                .output()
                .unwrap();
            assert!(result.status.success(), "临时工作区创建失败");
        }
        let db = tempfile::tempdir().unwrap();
        let resources: Arc<dyn SessionResources> = Arc::new(
            SessionResourcesImpl::open(db.path().join("threads.db"))
                .await
                .unwrap(),
        );
        let workspace = resources.resolve_workspace(repo.path()).await.unwrap();
        let thread_id = uuid::Uuid::now_v7().to_string();
        resources
            .create_session(&NewSession {
                thread_id: thread_id.clone(),
                created_at: "2026-09-28T00:00:00Z".into(),
                meta: NewSessionMeta {
                    title: Some("compact audit".into()),
                    cwd: workspace.cwd.to_string_lossy().into_owned(),
                    parent_thread_id: None,
                    hidden: false,
                    cancel_policy: Default::default(),
                    snapshot_at_message_id: None,
                },
                binding: SessionBinding {
                    schema_version: SESSION_BINDING_VERSION,
                    revision: 1,
                    project_id: workspace.project_id,
                    workspace_id: workspace.execution_registration_id,
                    cwd_relative_to_workspace: workspace.relative_cwd,
                },
                frozen: FrozenSnapshotBytes::new("{\"version\":1,\"test\":true}"),
            })
            .await
            .unwrap();
        Self {
            resources,
            thread_id,
            db,
            repo,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Failure {
    Http,
    Transport,
    ProviderRetryExhausted,
    MaxTokens,
    ToolUse,
    AnalysisOnly,
    Success,
}

struct SummaryModel {
    failure: Failure,
    calls: AtomicUsize,
    successful_after: usize,
    cancel_after_call: Option<(usize, Arc<CancellationToken>)>,
}

#[async_trait::async_trait]
impl peri_model::Model for SummaryModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
    async fn stream(
        &self,
        _: ModelRequest,
        _: CancellationToken,
    ) -> peri_model::ModelResult<ModelStream> {
        unreachable!("摘要使用 complete")
    }
    async fn complete(
        &self,
        _: ModelRequest,
        _: CancellationToken,
    ) -> peri_model::ModelResult<ModelResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some((target, token)) = &self.cancel_after_call {
            if call == *target {
                token.cancel();
            }
        }
        let failure = if call > self.successful_after {
            Failure::Success
        } else {
            self.failure
        };
        let (text, reason) = match failure {
            Failure::Http => return Err(ModelError::http_status(503, "fixture", None::<&str>)),
            Failure::Transport => {
                return Err(ModelError::transport(
                    peri_model::TransportErrorKind::Timeout,
                    Some("fixture"),
                ))
            }
            Failure::ProviderRetryExhausted => {
                return Err(
                    ModelError::retry_exhausted(3, peri_model::RetryErrorKind::HttpStatus).unwrap(),
                )
            }
            Failure::MaxTokens if call == 1 => ("<summary>incomplete task", StopReason::MaxTokens),
            Failure::MaxTokens => (" still pending", StopReason::MaxTokens),
            Failure::ToolUse => (
                "<summary>untrusted tool pause</summary>",
                StopReason::ToolUse,
            ),
            Failure::AnalysisOnly => (
                "<analysis>no usable summary</analysis>",
                StopReason::EndTurn,
            ),
            Failure::Success if matches!(self.failure, Failure::MaxTokens) => {
                (" RECOVERED task</summary>", StopReason::EndTurn)
            }
            Failure::Success => ("<summary>RECOVERED task</summary>", StopReason::EndTurn),
        };
        ModelResponse::new(ModelMessage::assistant_text(text), reason, None, None)
    }
}

#[derive(Default)]
struct ReasonModel {
    requests: parking_lot::Mutex<Vec<Vec<BaseMessage>>>,
}

struct SummaryText(&'static str);

#[async_trait::async_trait]
impl peri_model::Model for SummaryText {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
    async fn stream(
        &self,
        _: ModelRequest,
        _: CancellationToken,
    ) -> peri_model::ModelResult<ModelStream> {
        unreachable!("摘要使用 complete")
    }
    async fn complete(
        &self,
        _: ModelRequest,
        _: CancellationToken,
    ) -> peri_model::ModelResult<ModelResponse> {
        ModelResponse::new(
            ModelMessage::assistant_text(self.0),
            StopReason::EndTurn,
            None,
            None,
        )
    }
}

#[async_trait::async_trait]
impl ReactLLM for ReasonModel {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _: &[&dyn BaseTool],
        _: Option<StreamingContext>,
    ) -> AgentResult<Reasoning> {
        self.requests.lock().push(messages.to_vec());
        Ok(Reasoning::with_answer("", "done"))
    }
}

async fn make_case(
    failure: Failure,
    successful_after: usize,
    micro: bool,
    cancel_on: Option<usize>,
) -> (
    BoundSession,
    StageContext,
    Arc<ReasonModel>,
    Arc<SummaryModel>,
    EventHandles,
) {
    let bound = BoundSession::open().await;
    let session = Session::new(
        Arc::from(bound.repo.path().to_string_lossy().as_ref()),
        FrozenContext::builder().build(),
        Some(bound.thread_id.clone()),
    );
    {
        let transcript = session.transcript();
        let mut guard = transcript.write();
        *guard = std::mem::take(&mut *guard)
            .with_persistence(bound.resources.clone(), bound.thread_id.clone());
        guard.append(BaseMessage::human("original task"));
        if micro {
            for index in 0..8 {
                let call = format!("call-{index}");
                guard.append(BaseMessage::ai_with_tool_calls(
                    "",
                    vec![ToolCallRequest::new(
                        &call,
                        "Bash",
                        serde_json::json!({"command":"fixed command"}),
                    )],
                ));
                guard.append(BaseMessage::tool_result(call, "output ".repeat(100)));
                guard.append(BaseMessage::human(format!("continue {index}")));
            }
        }
    }
    let turn = session.start_turn();
    let summary = Arc::new(SummaryModel {
        failure,
        calls: AtomicUsize::new(0),
        successful_after,
        cancel_after_call: cancel_on.map(|call| (call, turn.cancel_token.clone())),
    });
    let reason = Arc::new(ReasonModel::default());
    let (bus, handles) = EventBus::new(Default::default());
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(reason.clone())
        .with_compact_llm(summary.clone())
        .with_context_budget(ContextBudget::new(100_000))
        .with_compact_config(CompactConfig::default())
        .with_event_bus(Arc::new(bus))
        .build();
    ctx.compact.token_tracker.write().accumulate(&TokenUsage {
        input_tokens: 109_000,
        output_tokens: 0,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    });
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("continue now"),
    ));
    (bound, ctx, reason, summary, handles)
}

async fn assert_unusable_summary_blocks_reason(failure: Failure) {
    let (bound, ctx, reason, summary, mut handles) =
        make_case(failure, usize::MAX, false, None).await;
    let result = run_react_loop(ctx.clone(), 4).await;
    let transcript = std::mem::take(&mut *ctx.session.transcript.write());
    transcript.flush_persistence().await.unwrap();
    let reopened = SessionResourcesImpl::open(bound.db.path().join("threads.db"))
        .await
        .unwrap();
    let snapshot = reopened
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert!(
        snapshot.payloads.iter().any(|payload| payload
            .as_message()
            .is_some_and(|message| message.content() == "original task")),
        "失败不得丢失 canonical 原文"
    );
    assert!(
        snapshot.flags.values().all(|flags| !flags.excluded),
        "不完整摘要不得提交 excluded"
    );
    assert_eq!(
        std::iter::from_fn(|| handles.try_observe())
            .filter(|event| matches!(event, ObserveEvent::CompactEnded { .. }))
            .count(),
        1,
        "失败应关闭 compact 事件"
    );
    assert!(reason.requests.lock().is_empty(), "{failure:?}: 109% 且摘要不可用时不得 Reason；实际 {result:?}，summary calls={}，reason calls={}", summary.calls.load(Ordering::SeqCst), reason.requests.lock().len());
    let LoopResult::Error(error) = result else {
        panic!("{failure:?} 必须明确失败");
    };
    let public = peri_acp_types::session::ExecutionFailure::from_agent_error(&error);
    match failure {
        Failure::Http => {
            assert!(matches!(error, AgentError::ModelError(_)));
            assert_eq!(public.http_status, Some(503));
            assert_eq!(public.diagnostic.unwrap().status(), Some(503));
        }
        Failure::Transport => {
            assert!(matches!(error, AgentError::ModelError(_)));
            assert_eq!(
                public.diagnostic.unwrap().transport(),
                Some(peri_model::TransportErrorKind::Timeout)
            );
        }
        Failure::ProviderRetryExhausted => {
            assert!(matches!(error, AgentError::ModelError(_)));
            assert_eq!(public.diagnostic.unwrap().retry_attempts(), Some(3));
        }
        Failure::ToolUse => assert!(matches!(
            error,
            AgentError::CompactIncompleteResponse {
                stop_reason: StopReason::ToolUse
            }
        )),
        Failure::AnalysisOnly => assert!(matches!(
            error,
            AgentError::CompactRetriesExhausted { attempts: 3, .. }
        )),
        Failure::MaxTokens | Failure::Success => unreachable!(),
    }
    assert_eq!(
        summary.calls.load(Ordering::SeqCst),
        if matches!(failure, Failure::AnalysisOnly) {
            3
        } else {
            1
        },
        "空摘要重试有界；Provider 失败不能在 Compact 层重启"
    );
}

/// [回归测试] Provider HTTP 失败不应丢掉原因并继续高压推理。
#[tokio::test]
async fn test_http_failure_blocks_reason() {
    assert_unusable_summary_blocks_reason(Failure::Http).await;
}

/// [回归测试] Provider transport 失败不应丢掉原因并继续高压推理。
#[tokio::test]
async fn test_transport_failure_blocks_reason() {
    assert_unusable_summary_blocks_reason(Failure::Transport).await;
}

/// [回归测试] Provider 已耗尽自身重试时，Compact 不应重新启动普通推理。
#[tokio::test]
async fn test_provider_retry_exhaustion_blocks_reason() {
    assert_unusable_summary_blocks_reason(Failure::ProviderRetryExhausted).await;
}

#[tokio::test]
async fn test_max_tokens_summary_uses_two_chunks_without_third_call() {
    let (_bound, ctx, reason, summary, _) =
        make_case(Failure::MaxTokens, usize::MAX, false, None).await;
    let result = run_react_loop(ctx, 4).await;
    assert!(matches!(result, LoopResult::Completed), "{result:?}");
    assert_eq!(summary.calls.load(Ordering::SeqCst), 2);
    let requests = reason.requests.lock();
    assert_eq!(requests.len(), 1);
    assert!(requests[0]
        .iter()
        .any(|message| message.content().contains("incomplete task still pending")));
    assert!(requests[0]
        .iter()
        .any(|message| message.content().contains("remaining tail was omitted")));
}

/// [回归测试] 最后一次续写成功后，下一次 Reason 必须看到所有摘要片段而不是原历史。
#[tokio::test]
async fn test_max_tokens_summary_continuation_resumes_reason_with_complete_summary() {
    let (_bound, ctx, reason, summary, _) = make_case(Failure::MaxTokens, 1, false, None).await;
    let result = run_react_loop(ctx, 4).await;
    assert!(matches!(result, LoopResult::Completed), "{result:?}");
    assert_eq!(summary.calls.load(Ordering::SeqCst), 2);
    let requests = reason.requests.lock();
    assert_eq!(requests.len(), 1);
    assert!(requests[0]
        .iter()
        .any(|message| message.content().contains("incomplete task RECOVERED task")));
    assert!(!requests[0]
        .iter()
        .any(|message| message.content() == "original task"));
}

/// [回归测试] 摘要截断后的续写必须服从外层取消，不得继续消耗请求或提交半截摘要。
#[tokio::test]
async fn test_cancel_before_summary_continuation_preserves_history() {
    let (bound, ctx, reason, summary, _) =
        make_case(Failure::MaxTokens, usize::MAX, false, Some(1)).await;
    let result = run_react_loop(ctx.clone(), 4).await;
    assert!(matches!(result, LoopResult::Interrupted), "{result:?}");
    assert_eq!(summary.calls.load(Ordering::SeqCst), 1);
    assert!(reason.requests.lock().is_empty());
    let transcript = std::mem::take(&mut *ctx.session.transcript.write());
    transcript.flush_persistence().await.unwrap();
    let stored = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert!(stored.flags.values().all(|flags| !flags.excluded));
    assert_eq!(stored.payloads.len(), 2, "只有原始 task 和本轮输入");
}

/// [回归测试] 工具调用停止不能伪装成成功摘要。
#[tokio::test]
async fn test_tool_use_summary_blocks_reason() {
    assert_unusable_summary_blocks_reason(Failure::ToolUse).await;
}

#[tokio::test]
async fn test_analysis_only_exhaustion_blocks_reason() {
    assert_unusable_summary_blocks_reason(Failure::AnalysisOnly).await;
}

#[tokio::test]
async fn test_analysis_only_recovers_at_last_attempt_and_preserves_canonical() {
    let (bound, ctx, reason, summary, _) = make_case(Failure::AnalysisOnly, 2, false, None).await;
    let result = run_react_loop(ctx.clone(), 4).await;
    assert!(matches!(result, LoopResult::Completed), "{result:?}");
    assert_eq!(summary.calls.load(Ordering::SeqCst), 3);
    assert_eq!(reason.requests.lock().len(), 1);
    assert!(reason.requests.lock()[0]
        .iter()
        .any(|message| message.content().contains("RECOVERED")));
    let transcript = std::mem::take(&mut *ctx.session.transcript.write());
    transcript.flush_persistence().await.unwrap();
    let reopened = SessionResourcesImpl::open(bound.db.path().join("threads.db"))
        .await
        .unwrap();
    let snapshot = reopened
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    let original = snapshot
        .payloads
        .iter()
        .find_map(|payload| {
            payload
                .as_message()
                .filter(|message| message.content() == "original task")
        })
        .unwrap();
    assert!(
        snapshot.flags[&original.id()].excluded,
        "成功摘要应隐藏原文但保留磁盘历史"
    );
}

#[tokio::test]
async fn test_cancel_on_last_empty_attempt_wins_over_exhaustion() {
    let (_, ctx, reason, summary, _) =
        make_case(Failure::AnalysisOnly, usize::MAX, false, Some(3)).await;
    let result = run_react_loop(ctx, 4).await;
    assert!(
        matches!(result, LoopResult::Interrupted),
        "取消应优先于摘要耗尽：{result:?}"
    );
    assert_eq!(summary.calls.load(Ordering::SeqCst), 3);
    assert!(reason.requests.lock().is_empty());
}

#[tokio::test]
async fn test_micro_persists_when_full_empty_attempts_exhaust() {
    let (bound, ctx, reason, summary, _) =
        make_case(Failure::AnalysisOnly, usize::MAX, true, None).await;
    let result = run_react_loop(ctx.clone(), 4).await;
    assert!(
        matches!(
            result,
            LoopResult::Error(AgentError::CompactRetriesExhausted { attempts: 3, .. })
        ),
        "{result:?}"
    );
    assert_eq!(summary.calls.load(Ordering::SeqCst), 3);
    assert!(reason.requests.lock().is_empty());
    let transcript = std::mem::take(&mut *ctx.session.transcript.write());
    transcript.flush_persistence().await.unwrap();
    let reopened = SessionResourcesImpl::open(bound.db.path().join("threads.db"))
        .await
        .unwrap();
    let snapshot = reopened
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert!(
        snapshot
            .flags
            .values()
            .any(|flags| flags.projection.is_some()),
        "Full 失败不能丢掉 Micro projection"
    );
    assert!(
        snapshot.flags.values().all(|flags| !flags.excluded),
        "Full 失败不得隐藏原文"
    );
    assert_eq!(
        snapshot.payloads.len(),
        26,
        "原始 task + 8 轮 + 新输入必须完整保存"
    );
}

#[tokio::test]
async fn test_cancel_after_micro_preserves_projection_without_reason() {
    let (bound, ctx, reason, summary, mut handles) =
        make_case(Failure::AnalysisOnly, usize::MAX, true, Some(1)).await;
    let result = run_react_loop(ctx.clone(), 4).await;
    assert!(matches!(result, LoopResult::Interrupted), "{result:?}");
    assert_eq!(summary.calls.load(Ordering::SeqCst), 1);
    assert!(reason.requests.lock().is_empty());
    let transcript = std::mem::take(&mut *ctx.session.transcript.write());
    transcript.flush_persistence().await.unwrap();
    let snapshot = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert!(
        snapshot
            .flags
            .values()
            .any(|flags| flags.projection.is_some()),
        "取消应保留 Micro projection"
    );
    assert!(
        snapshot.flags.values().all(|flags| !flags.excluded),
        "空摘要不得 excluded"
    );
    assert_eq!(snapshot.payloads.len(), 26);
    assert_eq!(
        std::iter::from_fn(|| handles.try_observe())
            .filter(|event| matches!(
                event,
                ObserveEvent::MessagesCompacted {
                    outcome: peri_agent::agent::compact_v2::CompactOutcome::InterruptedAfterCommit,
                    ..
                }
            ))
            .count(),
        1
    );
}

#[tokio::test]
async fn test_missing_summary_model_blocks_high_pressure_reason() {
    let (_, mut ctx, reason, summary, _) = make_case(Failure::Success, 0, false, None).await;
    ctx.compact.compact_llm = None;
    let result = run_react_loop(ctx, 4).await;
    assert!(
        matches!(result, LoopResult::Error(AgentError::CompactNoLlm)),
        "{result:?}"
    );
    assert!(reason.requests.lock().is_empty());
    assert_eq!(summary.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn test_shadow_empty_micro_plan_never_calls_full_at_high_pressure() {
    let (bound, mut ctx, reason, summary, mut handles) =
        make_case(Failure::Success, 0, false, None).await;
    ctx.compact
        .compact_config
        .as_mut()
        .unwrap()
        .shadow_mode_enabled = true;
    let result = run_react_loop(ctx.clone(), 4).await;
    assert!(matches!(result, LoopResult::Completed), "{result:?}");
    assert_eq!(
        summary.calls.load(Ordering::SeqCst),
        0,
        "shadow 空 plan 也不得升级真实 Full"
    );
    assert_eq!(reason.requests.lock().len(), 1);
    let transcript = std::mem::take(&mut *ctx.session.transcript.write());
    transcript.flush_persistence().await.unwrap();
    let snapshot = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert!(snapshot.flags.values().all(|flags| !flags.excluded));
    assert_eq!(
        std::iter::from_fn(|| handles.try_observe())
            .filter(|event| matches!(event, ObserveEvent::MessagesCompacted { .. }))
            .count(),
        0
    );
}

#[tokio::test]
async fn test_manual_force_still_applies_full_when_shadow_enabled() {
    let (bound, mut ctx, _, summary, _) = make_case(Failure::Success, 0, false, None).await;
    ctx.compact
        .compact_config
        .as_mut()
        .unwrap()
        .shadow_mode_enabled = true;
    let mut transcript = std::mem::take(&mut *ctx.session.transcript.write());
    let mut failures = 0;
    let result = peri_agent::agent::compact_v2::run_compact(
        &mut transcript,
        Some(summary.as_ref()),
        ctx.compact.compact_config.as_ref().unwrap(),
        &peri_agent::agent::compact_v2::ContextPressure {
            estimated_tokens: 109_000,
            context_window: 100_000,
            output_reserve: 0,
            predicted_tool_growth: 0,
            safety_buffer: 0,
            cache_hit_rate: 0.0,
        },
        true,
        &mut failures,
        bound.repo.path().to_str().unwrap(),
    )
    .await;
    assert!(result.outcome().is_full_applied());
    assert_eq!(summary.calls.load(Ordering::SeqCst), 1);
    assert!(result.failure.is_none());
}

#[tokio::test]
async fn test_cancel_on_provider_failure_wins_after_micro_commit() {
    let (bound, ctx, reason, summary, _) =
        make_case(Failure::Http, usize::MAX, true, Some(1)).await;
    let result = run_react_loop(ctx.clone(), 4).await;
    assert!(matches!(result, LoopResult::Interrupted), "{result:?}");
    assert!(reason.requests.lock().is_empty());
    assert_eq!(summary.calls.load(Ordering::SeqCst), 1);
    let transcript = std::mem::take(&mut *ctx.session.transcript.write());
    transcript.flush_persistence().await.unwrap();
    let snapshot = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert_eq!(snapshot.payloads.len(), 26);
    assert!(snapshot
        .flags
        .values()
        .any(|flags| flags.projection.is_some()));
    assert!(snapshot.flags.values().all(|flags| !flags.excluded));
}

#[tokio::test]
async fn test_force_with_zero_retry_budget_reports_exhaustion_without_model_call() {
    let (bound, mut ctx, _, summary, _) = make_case(Failure::Success, 0, false, None).await;
    ctx.compact
        .compact_config
        .as_mut()
        .unwrap()
        .max_consecutive_failures = 0;
    let mut transcript = std::mem::take(&mut *ctx.session.transcript.write());
    let mut failures = 0;
    let result = peri_agent::agent::compact_v2::run_compact(
        &mut transcript,
        Some(summary.as_ref()),
        ctx.compact.compact_config.as_ref().unwrap(),
        &peri_agent::agent::compact_v2::ContextPressure {
            estimated_tokens: 0,
            context_window: u32::MAX,
            output_reserve: 0,
            predicted_tool_growth: 0,
            safety_buffer: 0,
            cache_hit_rate: 0.0,
        },
        true,
        &mut failures,
        bound.repo.path().to_str().unwrap(),
    )
    .await;
    assert!(matches!(
        result.failure,
        Some(AgentError::CompactRetriesExhausted { attempts: 0, .. })
    ));
    assert_eq!(summary.calls.load(Ordering::SeqCst), 0);
    assert_eq!(transcript.visible_messages().len(), 1);
}

#[tokio::test]
async fn test_exhausted_full_budget_does_not_fail_low_pressure_disabled_or_shadow() {
    let (bound, ctx, _, summary, _) = make_case(Failure::Success, 0, false, None).await;
    let mut transcript = std::mem::take(&mut *ctx.session.transcript.write());
    for (tokens, disabled, shadow) in [
        (1_000, false, false),
        (109_000, true, false),
        (109_000, false, true),
    ] {
        let config = CompactConfig {
            max_consecutive_failures: 0,
            auto_compact_enabled: !disabled,
            shadow_mode_enabled: shadow,
            ..Default::default()
        };
        let mut failures = 0;
        let result = peri_agent::agent::compact_v2::run_compact(
            &mut transcript,
            Some(summary.as_ref()),
            &config,
            &peri_agent::agent::compact_v2::ContextPressure {
                estimated_tokens: tokens,
                context_window: 100_000,
                output_reserve: 0,
                predicted_tool_growth: 0,
                safety_buffer: 0,
                cache_hit_rate: 0.0,
            },
            false,
            &mut failures,
            bound.repo.path().to_str().unwrap(),
        )
        .await;
        assert!(
            result.failure.is_none(),
            "无需 Full 的分支不应因预算耗尽失败"
        );
        assert!(!result.outcome().has_applied_change());
    }
    assert_eq!(summary.calls.load(Ordering::SeqCst), 0);
    assert_eq!(transcript.visible_messages().len(), 1);
}

/// [回归测试] 讨论解析器时，闭合摘要中的思考标签字面量必须进入下一次 Reason。
#[tokio::test]
async fn test_closed_summary_preserves_literal_reasoning_tags() {
    let (bound, mut ctx, reason, _, _) = make_case(Failure::Success, 0, false, None).await;
    ctx.compact.compact_llm = Some(Arc::new(SummaryText(
        "<summary>parser strips <think> and </analysis> tags</summary>",
    )));
    let result = run_react_loop(ctx.clone(), 4).await;
    assert!(
        matches!(result, LoopResult::Completed),
        "有效正文不得被误判为空摘要：{result:?}"
    );
    assert_eq!(reason.requests.lock().len(), 1);
    let request_text = reason.requests.lock()[0]
        .iter()
        .map(|message| message.content())
        .collect::<Vec<_>>()
        .join("\n");
    // Reason 使用既有 system-reminder XML 转义；canonical 摘要仍保存原始字面量。
    assert!(
        request_text.contains("parser strips &lt;think&gt; and &lt;/analysis&gt; tags"),
        "{request_text}"
    );
    let transcript = std::mem::take(&mut *ctx.session.transcript.write());
    transcript.flush_persistence().await.unwrap();
    let stored = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    let original = stored
        .payloads
        .iter()
        .find_map(|payload| {
            payload
                .as_message()
                .filter(|message| message.content() == "original task")
        })
        .unwrap();
    assert!(stored.flags[&original.id()].excluded);
    assert!(stored
        .payloads
        .iter()
        .any(|payload| payload.as_message().is_some_and(|message| message
            .content()
            .contains("parser strips <think> and </analysis> tags"))));
}

/// [回归测试] 思考块内的闭合 summary 是草稿，不能提交并替代历史。
#[tokio::test]
async fn test_summary_inside_reasoning_cannot_replace_history() {
    let (bound, mut ctx, reason, _, _) = make_case(Failure::Success, 0, false, None).await;
    ctx.compact.compact_llm = Some(Arc::new(SummaryText(
        "<analysis><summary>draft</summary></analysis>",
    )));
    let result = run_react_loop(ctx.clone(), 4).await;
    assert!(
        matches!(
            result,
            LoopResult::Error(AgentError::CompactRetriesExhausted { attempts: 3, .. })
        ),
        "思考草稿不得成为摘要：{result:?}"
    );
    assert!(reason.requests.lock().is_empty());
    let transcript = std::mem::take(&mut *ctx.session.transcript.write());
    transcript.flush_persistence().await.unwrap();
    let stored = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert!(stored.flags.values().all(|flags| !flags.excluded));
    assert_eq!(stored.payloads.len(), 2, "只保留原始任务与本次继续输入");
}

/// [回归测试] 外层闭合摘要正文是普通文本，内部标签和外围尾部均不能污染它。
#[tokio::test]
async fn test_closed_summary_after_reasoning_preserves_body_and_ignores_draft() {
    let (_bound, mut ctx, reason, _, _) = make_case(Failure::Success, 0, false, None).await;
    ctx.compact.compact_llm = Some(Arc::new(SummaryText("<analysis><thinking><summary>draft</summary></thinking></analysis><summary>literal <analysis> </thinking> <think> </analysis> <thinking> </think> tags</summary></analysis>")));
    let result = run_react_loop(ctx, 4).await;
    assert!(
        matches!(result, LoopResult::Completed),
        "闭合正文必须优先于尾部标签：{result:?}"
    );
    let requests = reason.requests.lock();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].iter().any(|message| message
        .content()
        .contains("literal &lt;analysis&gt; &lt;/thinking&gt; &lt;think&gt; &lt;/analysis&gt; &lt;thinking&gt; &lt;/think&gt; tags")));
    assert!(requests[0]
        .iter()
        .all(|message| !message.content().contains("draft")));
}
