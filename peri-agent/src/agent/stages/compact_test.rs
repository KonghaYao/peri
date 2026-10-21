//! Tests for compact

use super::*;
use crate::agent::compact_v2::config::CompactConfig;
use crate::agent::events_v2::{EventBus, EventBusConfig, EventHandles, ObserveEvent};
use crate::agent::stages::StageContext;
use crate::agent::token::ContextBudget;
use crate::messages::{BaseMessage, MessageContent, ToolCallRequest};
use crate::session::store::FrozenContext;
use crate::session::Session;
use peri_model::TokenUsage;
use std::sync::Arc;

fn make_context() -> StageContext {
    let cwd: Arc<str> = Arc::from("/tmp/test");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    StageContext::new(turn, session.transcript(), session.queue().clone())
}

fn make_context_with_observe() -> (StageContext, EventHandles) {
    let mut ctx = make_context();
    let (event_bus, handles) = EventBus::new(EventBusConfig::default());
    ctx.runtime.event_bus = Arc::new(event_bus);
    (ctx, handles)
}

fn append_compactable_history(ctx: &StageContext) {
    let long_output = "x".repeat(2_000);
    let mut transcript = ctx.session.transcript.write();
    for i in 0..8 {
        transcript.append(BaseMessage::human(MessageContent::text(format!("q {}", i))));
        transcript.append(BaseMessage::ai_with_tool_calls(
            MessageContent::text(""),
            vec![ToolCallRequest::new(
                format!("c_{}", i),
                "Bash",
                serde_json::json!({}),
            )],
        ));
        transcript.append(BaseMessage::tool_result(
            format!("c_{}", i),
            MessageContent::text(&long_output),
        ));
    }
}

struct CompactFixtureTool(&'static str, crate::tools::ContextRetention);

#[async_trait::async_trait]
impl crate::tools::BaseTool for CompactFixtureTool {
    fn name(&self) -> &str {
        self.0
    }
    fn description(&self) -> &str {
        "fixture policy"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type":"object"})
    }
    fn context_retention(&self) -> crate::tools::ContextRetention {
        self.1
    }
    fn is_direct(&self) -> bool {
        true
    }
    async fn invoke(
        &self,
        _: serde_json::Value,
        _: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        panic!("compact must not execute historical calls")
    }
}

async fn confirm_fixture_exposure(
    ctx: &StageContext,
) -> crate::session::test_resources::TestSession {
    use crate::agent::compact_v2::exposure::RequestExposure;
    use crate::tools::ContextRetention;
    let bound = crate::session::test_resources::TestSession::open().await;
    let original = std::mem::take(&mut *ctx.session.transcript.write());
    let mut transcript = crate::session::MessageTranscript::new()
        .with_persistence(bound.resources(), bound.thread_id.clone());
    for entry in original.entries() {
        transcript.append(entry.message().clone());
    }
    transcript.flush_persistence().await.unwrap();
    let request: Vec<_> = transcript.visible_messages().into_iter().cloned().collect();
    let exposure = RequestExposure::capture(&request, &transcript).unwrap();
    let closed = request
        .iter()
        .flat_map(|message| message.tool_calls().iter().map(|call| call.id.clone()))
        .collect::<Vec<_>>();
    *ctx.session.transcript.write() = transcript;
    let mut ledger = ctx.compact.exposure.lock();
    ledger.confirm_closed(closed);
    ledger.confirm(&exposure);
    drop(ledger);
    let mut tools = ctx.runtime.tools.write();
    tools.insert(
        "Bash".into(),
        Arc::new(CompactFixtureTool("Bash", ContextRetention::Recomputable)),
    );
    tools.insert(
        "ReadContextArchive".into(),
        Arc::new(CompactFixtureTool(
            "ReadContextArchive",
            ContextRetention::ArchiveRead,
        )),
    );
    bound
}

fn observe_events(handles: &mut EventHandles) -> Vec<ObserveEvent> {
    std::iter::from_fn(|| handles.try_observe()).collect()
}

#[tokio::test]
async fn test_micro_applied_then_full_failure_does_not_reset_token_tracker() {
    // 高压力下先应用 Micro，随后无 compact LLM 的 Full 失败。
    let mut ctx = make_context();
    let long_output = "x".repeat(2_000);
    {
        let mut transcript = ctx.session.transcript.write();
        for i in 0..8 {
            transcript.append(BaseMessage::human(MessageContent::text(format!("q {}", i))));
            transcript.append(BaseMessage::ai_with_tool_calls(
                MessageContent::text(""),
                vec![ToolCallRequest::new(
                    format!("c_{}", i),
                    "Bash",
                    serde_json::json!({}),
                )],
            ));
            transcript.append(BaseMessage::tool_result(
                format!("c_{}", i),
                MessageContent::text(&long_output),
            ));
        }
    }
    let _bound = confirm_fixture_exposure(&ctx).await;
    ctx.compact.context_budget = Some(ContextBudget::new(200_000));
    ctx.compact.compact_config = Some(CompactConfig {
        micro_compact_stale_steps: 1,
        ..Default::default()
    });
    ctx.compact.token_tracker.write().accumulate(&TokenUsage {
        input_tokens: 196_000,
        output_tokens: 0,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    });
    let token_tracker = ctx.compact.token_tracker.clone();

    let error = run_compact(CompactInput {
        context: ctx.clone(),
        has_tool_calls: true,
    })
    .await
    .err()
    .expect("高压 Full 失败必须结束本轮");

    assert!(matches!(error, crate::error::AgentError::CompactNoLlm));
    assert!(
        ctx.session.transcript.read().entries().iter().any(|entry| {
            ctx.session
                .transcript
                .read()
                .flags(entry.id())
                .projection
                .is_some()
        }),
        "Full 失败后必须保留已应用的 Micro/Smart projection"
    );
    assert_eq!(
        token_tracker.read().estimated_context_tokens(),
        Some(196_000),
        "Full 失败后不得将已应用 Micro 伪装为 Full completion 并 reset token tracker"
    );
}

#[tokio::test]
async fn test_compact_stage_smart_applied_then_full_failure_stops_without_tracker_reset() {
    // 高压力下先应用 Smart，随后无 compact LLM 的 Full 失败。
    let mut ctx = make_context();
    let long_output = "x".repeat(2_000);
    {
        let mut transcript = ctx.session.transcript.write();
        for i in 0..8 {
            transcript.append(BaseMessage::human(MessageContent::text(format!("q {}", i))));
            transcript.append(BaseMessage::ai_with_tool_calls(
                MessageContent::text(""),
                vec![ToolCallRequest::new(
                    format!("c_{}", i),
                    "Bash",
                    serde_json::json!({}),
                )],
            ));
            transcript.append(BaseMessage::tool_result(
                format!("c_{}", i),
                MessageContent::text(&long_output),
            ));
        }
    }
    let _bound = confirm_fixture_exposure(&ctx).await;
    ctx.compact.context_budget = Some(ContextBudget::new(200_000));
    ctx.compact.compact_config = Some(CompactConfig {
        smart_compact_enabled: true,
        micro_compact_stale_steps: 1,
        ..Default::default()
    });
    ctx.compact.token_tracker.write().accumulate(&TokenUsage {
        input_tokens: 196_000,
        output_tokens: 0,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    });
    let token_tracker = ctx.compact.token_tracker.clone();

    let error = run_compact(CompactInput {
        context: ctx.clone(),
        has_tool_calls: true,
    })
    .await
    .err()
    .expect("高压 Full 失败必须结束本轮");

    assert!(matches!(error, crate::error::AgentError::CompactNoLlm));
    assert!(
        ctx.session.transcript.read().entries().iter().any(|entry| {
            ctx.session
                .transcript
                .read()
                .flags(entry.id())
                .projection
                .is_some()
        }),
        "Full 失败后必须保留已应用的 Micro/Smart projection"
    );
    assert_eq!(
        token_tracker.read().estimated_context_tokens(),
        Some(196_000),
        "Full 失败后不得将已应用 Smart 伪装为 Full completion 并 reset token tracker"
    );
}

#[tokio::test]
async fn test_compact_stage_micro_shadow_mode_is_not_compacted() {
    let mut ctx = make_context();
    let long_output = "x".repeat(2_000);
    {
        let mut transcript = ctx.session.transcript.write();
        for i in 0..8 {
            transcript.append(BaseMessage::human(MessageContent::text(format!("q {}", i))));
            transcript.append(BaseMessage::ai_with_tool_calls(
                MessageContent::text(""),
                vec![ToolCallRequest::new(
                    format!("c_{}", i),
                    "Bash",
                    serde_json::json!({}),
                )],
            ));
            transcript.append(BaseMessage::tool_result(
                format!("c_{}", i),
                MessageContent::text(&long_output),
            ));
        }
    }
    ctx.compact.context_budget = Some(ContextBudget::new(200_000));
    ctx.compact.compact_config = Some(CompactConfig {
        micro_compact_stale_steps: 1,
        shadow_mode_enabled: true,
        ..Default::default()
    });
    ctx.compact.token_tracker.write().accumulate(&TokenUsage {
        input_tokens: 160_000,
        output_tokens: 0,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    });

    let output = run_compact(CompactInput {
        context: ctx,
        has_tool_calls: true,
    })
    .await
    .unwrap();

    assert!(
        !output.compacted,
        "Micro shadow mode 不得向 stage 报告 compacted=true"
    );
}

#[tokio::test]
async fn test_compact_stage_smart_shadow_mode_is_not_compacted() {
    let mut ctx = make_context();
    let long_output = "x".repeat(2_000);
    {
        let mut transcript = ctx.session.transcript.write();
        for i in 0..8 {
            transcript.append(BaseMessage::human(MessageContent::text(format!("q {}", i))));
            transcript.append(BaseMessage::ai_with_tool_calls(
                MessageContent::text(""),
                vec![ToolCallRequest::new(
                    format!("c_{}", i),
                    "Bash",
                    serde_json::json!({}),
                )],
            ));
            transcript.append(BaseMessage::tool_result(
                format!("c_{}", i),
                MessageContent::text(&long_output),
            ));
        }
    }
    ctx.compact.context_budget = Some(ContextBudget::new(200_000));
    ctx.compact.compact_config = Some(CompactConfig {
        smart_compact_enabled: true,
        micro_compact_stale_steps: 1,
        shadow_mode_enabled: true,
        ..Default::default()
    });
    ctx.compact.token_tracker.write().accumulate(&TokenUsage {
        input_tokens: 160_000,
        output_tokens: 0,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    });

    let output = run_compact(CompactInput {
        context: ctx,
        has_tool_calls: true,
    })
    .await
    .unwrap();
    assert!(
        !output.compacted,
        "Smart shadow mode 不得向 stage 报告 compacted=true"
    );
}

#[tokio::test]
async fn test_compact_stage_shadow_mode_emits_no_messages_compacted() {
    let (mut ctx, mut handles) = make_context_with_observe();
    append_compactable_history(&ctx);
    ctx.compact.context_budget = Some(ContextBudget::new(200_000));
    ctx.compact.compact_config = Some(CompactConfig {
        micro_compact_stale_steps: 1,
        shadow_mode_enabled: true,
        ..Default::default()
    });
    ctx.compact.token_tracker.write().accumulate(&TokenUsage {
        input_tokens: 160_000,
        output_tokens: 0,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    });

    let output = run_compact(CompactInput {
        context: ctx,
        has_tool_calls: true,
    })
    .await
    .unwrap();

    assert!(!output.compacted);
    let events = observe_events(&mut handles);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ObserveEvent::CompactStarted { .. }))
            .count(),
        1,
        "shadow mode 仍应保留 CompactStarted begin 观测"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ObserveEvent::MessagesCompacted { .. }))
            .count(),
        0,
        "Shadowed 不得伪装为 MessagesCompacted"
    );
}

#[tokio::test]
async fn test_compact_stage_failure_limit_emits_no_messages_compacted() {
    let (mut ctx, mut handles) = make_context_with_observe();
    ctx.compact.context_budget = Some(ContextBudget::new(200_000));
    ctx.compact.compact_config = Some(CompactConfig {
        max_consecutive_failures: 1,
        ..Default::default()
    });
    ctx.compact
        .compact_consecutive_failures
        .store(1, std::sync::atomic::Ordering::Relaxed);
    ctx.compact.token_tracker.write().accumulate(&TokenUsage {
        input_tokens: 160_000,
        output_tokens: 0,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    });

    let output = run_compact(CompactInput {
        context: ctx,
        has_tool_calls: true,
    })
    .await
    .unwrap();

    assert!(!output.compacted);
    let events = observe_events(&mut handles);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ObserveEvent::CompactStarted { .. }))
            .count(),
        1,
        "failure limit 在 stage action 已确定后仍会发出 CompactStarted"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ObserveEvent::MessagesCompacted { .. }))
            .count(),
        0,
        "Skipped 不得伪装为 MessagesCompacted"
    );
}

#[tokio::test]
async fn test_compact_stage_applied_mixed_emits_one_messages_compacted_with_snapshot() {
    let (mut ctx, mut handles) = make_context_with_observe();
    append_compactable_history(&ctx);
    let _bound = confirm_fixture_exposure(&ctx).await;
    ctx.compact.context_budget = Some(ContextBudget::new(200_000));
    ctx.compact.compact_config = Some(CompactConfig {
        micro_compact_stale_steps: 1,
        ..Default::default()
    });
    ctx.compact.token_tracker.write().accumulate(&TokenUsage {
        input_tokens: 196_000,
        output_tokens: 0,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    });

    let error = run_compact(CompactInput {
        context: ctx.clone(),
        has_tool_calls: true,
    })
    .await
    .err()
    .expect("高压 Full 失败必须结束本轮");

    assert!(matches!(error, crate::error::AgentError::CompactNoLlm));
    assert!(
        ctx.session.transcript.read().entries().iter().any(|entry| {
            ctx.session
                .transcript
                .read()
                .flags(entry.id())
                .projection
                .is_some()
        }),
        "Full 失败后必须保留已应用的 Micro/Smart projection"
    );
    let events = observe_events(&mut handles);
    let completions: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            ObserveEvent::MessagesCompacted {
                messages,
                affected_count,
                ..
            } => Some((messages, affected_count)),
            _ => None,
        })
        .collect();
    assert_eq!(
        completions.len(),
        1,
        "Applied mixed outcome 只能产生一个 completion"
    );
    assert!(
        !completions[0].0.is_empty(),
        "Applied mixed completion 必须携带 transcript snapshot"
    );
    assert!(
        *completions[0].1 > 0,
        "Applied mixed completion 必须反映实际变更"
    );
}

#[tokio::test]
async fn test_auto_compact_consumes_each_pressure_sample_once() {
    let (mut ctx, mut handles) = make_context_with_observe();
    append_compactable_history(&ctx);
    let _bound = confirm_fixture_exposure(&ctx).await;
    ctx.compact.context_budget = Some(ContextBudget::new(200_000));
    ctx.compact.compact_config = Some(CompactConfig {
        micro_compact_stale_steps: 1,
        ..Default::default()
    });
    ctx.compact.token_tracker.write().accumulate(&TokenUsage {
        input_tokens: 160_000,
        output_tokens: 0,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    });

    let first = run_compact(CompactInput {
        context: ctx.clone(),
        has_tool_calls: true,
    })
    .await
    .unwrap();
    assert!(first.compacted);
    let first_started = observe_events(&mut handles)
        .into_iter()
        .filter(|event| matches!(event, ObserveEvent::CompactStarted { .. }))
        .count();
    assert_eq!(first_started, 1);

    ctx.compact.token_tracker.write().accumulate(&TokenUsage {
        input_tokens: 0,
        output_tokens: 100,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    });
    let repeated = run_compact(CompactInput {
        context: ctx.clone(),
        has_tool_calls: true,
    })
    .await
    .unwrap();
    assert!(!repeated.compacted);
    assert!(observe_events(&mut handles).is_empty());

    ctx.compact.token_tracker.write().accumulate(&TokenUsage {
        input_tokens: 160_000,
        output_tokens: 100,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    });
    run_compact(CompactInput {
        context: ctx.clone(),
        has_tool_calls: true,
    })
    .await
    .unwrap();
    assert_eq!(
        observe_events(&mut handles)
            .into_iter()
            .filter(|event| matches!(event, ObserveEvent::CompactStarted { .. }))
            .count(),
        1,
        "新有效 provider usage generation 必须重新评估"
    );

    ctx.compact
        .token_tracker
        .write()
        .add_estimated_tool_tokens("new tool output");
    run_compact(CompactInput {
        context: ctx,
        has_tool_calls: true,
    })
    .await
    .unwrap();
    assert_eq!(
        observe_events(&mut handles)
            .into_iter()
            .filter(|event| matches!(event, ObserveEvent::CompactStarted { .. }))
            .count(),
        1,
        "同 provider generation 下新增工具输出必须重新评估"
    );
}

/// [S1.4] cancel 且未提交变更时 CompactStarted 必须有配对结束事件：
/// emit `CompactEnded { outcome: Interrupted }`，且**不得** emit
/// `MessagesCompacted`（那会误导遥测以为压缩发生了）。
///
/// 覆盖 `compact.rs` select cancel arm 的"未提交"分支（:203）：
/// 预先取消 turn token → biased cancel arm 立即胜出，run_compact 未执行，
/// post_compact_flagged == pre_compact_flagged。
#[tokio::test]
async fn test_compact_stage_cancel_without_commit_emits_compact_ended() {
    let (mut ctx, mut handles) = make_context_with_observe();
    append_compactable_history(&ctx);
    ctx.compact.context_budget = Some(ContextBudget::new(200_000));
    ctx.compact.compact_config = Some(CompactConfig {
        micro_compact_stale_steps: 1,
        ..Default::default()
    });
    ctx.compact.token_tracker.write().accumulate(&TokenUsage {
        input_tokens: 196_000,
        output_tokens: 0,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    });
    // 预先取消 turn token：select biased cancel arm 立即胜出
    ctx.session.turn.cancel_token.cancel();

    let result = run_compact(CompactInput {
        context: ctx,
        has_tool_calls: true,
    })
    .await;

    assert!(
        matches!(result, Err(crate::error::AgentError::Interrupted)),
        "cancel 且未提交变更应返回 Interrupted"
    );

    let events = observe_events(&mut handles);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ObserveEvent::CompactStarted { .. }))
            .count(),
        1,
        "cancel arm 已 emit CompactStarted"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ObserveEvent::MessagesCompacted { .. }))
            .count(),
        0,
        "未提交变更不得 emit MessagesCompacted（会误导遥测以为压缩发生了）"
    );
    let ended: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            ObserveEvent::CompactEnded { outcome, .. } => Some(*outcome),
            _ => None,
        })
        .collect();
    assert_eq!(
        ended,
        vec![crate::agent::compact_v2::CompactOutcome::Interrupted],
        "cancel 未提交应恰好 emit 一次 CompactEnded(Interrupted)"
    );
}

/// The unchanged trigger uses provider usage when available and the request estimator otherwise.
#[tokio::test]
async fn test_auto_gate_7499_7500_7501_with_and_without_usage() {
    struct FixedEstimate(u64);
    #[async_trait::async_trait]
    impl crate::agent::react::ReactLLM for FixedEstimate {
        fn estimate_request_tokens(
            &self,
            _: &[BaseMessage],
            _: &[&dyn crate::tools::BaseTool],
        ) -> u64 {
            self.0
        }
        async fn generate_reasoning(
            &self,
            _: &[BaseMessage],
            _: &[&dyn crate::tools::BaseTool],
            _: Option<crate::agent::react::StreamingContext>,
        ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
            panic!("shadow compaction must not invoke a model")
        }
    }
    for with_usage in [false, true] {
        for tokens in [74_990_u32, 75_000, 75_010] {
            let (mut ctx, mut handles) = make_context_with_observe();
            ctx.session
                .transcript
                .write()
                .append(BaseMessage::human("current requirement"));
            ctx.compact.context_budget = Some(ContextBudget::new(100_000));
            ctx.compact.compact_config = Some(CompactConfig {
                shadow_mode_enabled: true,
                ..Default::default()
            });
            ctx.runtime.llm = Arc::new(FixedEstimate(if with_usage {
                1000
            } else {
                u64::from(tokens)
            }));
            if with_usage {
                ctx.compact.token_tracker.write().accumulate(&TokenUsage {
                    input_tokens: tokens,
                    output_tokens: 0,
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: None,
                });
            }
            let outcome = run_compact(CompactInput {
                context: ctx.clone(),
                has_tool_calls: false,
            })
            .await
            .unwrap();
            assert!(!outcome.compacted, "shadow mode never commits");
            assert_eq!(
                ctx.compact.token_tracker.read().estimated_context_tokens(),
                Some(u64::from(tokens))
            );
            let started = observe_events(&mut handles)
                .iter()
                .filter(|event| matches!(event, ObserveEvent::CompactStarted { .. }))
                .count();
            assert_eq!(
                started,
                usize::from(tokens >= 75_000),
                "usage={with_usage}, tokens={tokens}"
            );
            assert!(ctx
                .session
                .transcript
                .read()
                .entries()
                .iter()
                .all(|entry| ctx
                    .session
                    .transcript
                    .read()
                    .projection_ref(entry.id())
                    .is_none()));
        }
    }
}

#[tokio::test]
async fn test_archive_probe_cancel_and_failure_restore_history_and_close_events() {
    use crate::agent::compact_v2::CompactOutcome;
    use crate::session::test_resources::mock::{MockSessionResources, ResumeLoadGate};
    use std::sync::atomic::{AtomicBool, Ordering};

    for cancel in [true, false] {
        let (mut ctx, mut handles) = make_context_with_observe();
        let store = MockSessionResources::new();
        let (entered_tx, entered) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let dropped = Arc::new(AtomicBool::new(false));
        *store.archive_probe_gate.lock().unwrap() = Some(ResumeLoadGate {
            entered: entered_tx,
            release: released,
            dropped: dropped.clone(),
        });
        let source = BaseMessage::human("Preserve my canonical instruction.");
        let mut transcript = crate::session::MessageTranscript::new()
            .with_persistence(store, "archive-probe".into());
        transcript.append(source.clone());
        transcript.flush_persistence().await.unwrap();
        *ctx.session.transcript.write() = transcript;
        ctx.runtime.tools.write().insert(
            "ReadContextArchive".into(),
            Arc::new(CompactFixtureTool(
                "ReadContextArchive",
                crate::tools::ContextRetention::ArchiveRead,
            )),
        );
        ctx.compact.context_budget = Some(ContextBudget::new(100_000));
        ctx.compact.compact_config = Some(CompactConfig::default());
        ctx.compact.token_tracker.write().accumulate(&TokenUsage {
            input_tokens: 75_000,
            output_tokens: 0,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
        let context = ctx.clone();
        let run = tokio::spawn(async move {
            run_compact(CompactInput {
                context,
                has_tool_calls: false,
            })
            .await
        });
        entered.await.unwrap();
        if cancel {
            ctx.session.turn.cancel_token.cancel();
        } else {
            release.send(()).unwrap();
        }
        let error = tokio::time::timeout(std::time::Duration::from_secs(2), run)
            .await
            .expect("archive probing must not delay cancellation")
            .unwrap()
            .err()
            .expect("cancelled or failed archive probe must fail compaction");
        if cancel {
            assert!(matches!(error, crate::error::AgentError::Interrupted));
        } else {
            assert!(error
                .to_string()
                .contains("archive capability probe failed"));
        }
        assert!(dropped.load(Ordering::SeqCst));
        let transcript = ctx.session.transcript.read();
        assert_eq!(transcript.entries().len(), 1);
        assert_eq!(
            serde_json::to_value(transcript.get(source.id()).unwrap().message()).unwrap(),
            serde_json::to_value(source).unwrap()
        );
        let events = observe_events(&mut handles);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ObserveEvent::CompactStarted { .. }))
                .count(),
            1
        );
        assert!(!events
            .iter()
            .any(|event| matches!(event, ObserveEvent::MessagesCompacted { .. })));
        let ended = events
            .iter()
            .filter_map(|event| match event {
                ObserveEvent::CompactEnded { outcome, .. } => Some(*outcome),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            ended,
            vec![if cancel {
                CompactOutcome::Interrupted
            } else {
                CompactOutcome::MicroFailed
            }]
        );
    }
}
