//! 统一收益准入的确定性测试。
//!
//! 决策函数是唯一权威：`M = ceil(C/10)`，G 使用最终完整渲染后的净减少量。
//! 端到端用例复用真实 planner 与提交路径，验证未达门槛时零写入。

use super::decision::{decide_auto_compact, min_micro_gain_tokens, GainDecision};
use super::planner::{
    ArchivePlanningScope, ContextPressure, FullEscalationReason, MicroPlanningFacts,
};
use super::{run_compact_with_facts, CompactConfig, CompactOutcome, ProviderCapabilities};
use crate::agent::events::CompactStrategy;
use crate::messages::{BaseMessage, MessageContent, MessageId, ToolCallRequest};
use crate::session::async_result_mailbox::ProtectedResults;
use crate::session::test_resources::TestSession;
use crate::session::transcript::MessageTranscript;

fn pressure(estimated_tokens: u64, context_window: u32) -> ContextPressure {
    ContextPressure {
        estimated_tokens,
        context_window,
        output_reserve: 8_000,
        predicted_tool_growth: 0,
        safety_buffer: 5_000,
        cache_hit_rate: 0.0,
    }
}

fn exposed_facts(transcript: &MessageTranscript, session_id: &str) -> MicroPlanningFacts {
    let mut facts = MicroPlanningFacts {
        archive_scope: transcript
            .entries()
            .last()
            .map(|entry| ArchivePlanningScope {
                thread_id: session_id.into(),
                committed_tail: entry.id(),
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

fn append_round(transcript: &mut MessageTranscript, round: usize) {
    transcript.append(BaseMessage::human(MessageContent::text(format!(
        "q {round}"
    ))));
    transcript.append(BaseMessage::ai_with_tool_calls(
        MessageContent::text(format!("thinking {round}")),
        vec![ToolCallRequest::new(
            format!("call_{round}"),
            "Read",
            serde_json::json!({"path": "fixture.rs"}),
        )],
    ));
    transcript.append(BaseMessage::tool_result(
        format!("call_{round}"),
        MessageContent::text(format!("out {round} {}", "x".repeat(24_000))),
    ));
}

async fn compact_once(
    transcript: &mut MessageTranscript,
    session_id: &str,
    config: &CompactConfig,
    pressure: &ContextPressure,
) -> super::CompactResult {
    let facts = exposed_facts(transcript, session_id);
    let mut failures = 0u32;
    run_compact_with_facts(
        transcript,
        None,
        config,
        pressure,
        false,
        &mut failures,
        "/tmp",
        None,
        &ProtectedResults::default(),
        &facts,
        &ProviderCapabilities::openai(),
    )
    .await
}

fn has_persisted_projection(transcript: &MessageTranscript) -> bool {
    transcript.entries().iter().any(|entry| {
        let flags = transcript.flags(entry.id());
        flags.projection.is_some()
    })
}

// ── 门槛与整数取整 ──────────────────────────────────────────────────────────

#[test]
fn minimum_gain_is_one_tenth_of_the_full_window_rounded_up() {
    assert_eq!(min_micro_gain_tokens(0), 0);
    assert_eq!(min_micro_gain_tokens(1), 1);
    assert_eq!(min_micro_gain_tokens(200_000), 20_000);
    assert_eq!(
        min_micro_gain_tokens(200_001),
        20_001,
        "不得向下取整丢掉 1 token"
    );
    assert_eq!(min_micro_gain_tokens(199_999), 20_000);
}

#[test]
fn micro_zone_defers_one_token_below_the_minimum_gain() {
    let config = CompactConfig::default();
    let pressure = pressure(160_000, 200_000);

    let below = decide_auto_compact(&pressure, &config, 19_999, true);
    let at = decide_auto_compact(&pressure, &config, 20_000, true);

    match below {
        GainDecision::Deferred(deferral) => {
            assert_eq!(
                deferral.reason,
                peri_acp_types::compact::CompactDeferralReason::InsufficientGain
            );
            assert_eq!(deferral.estimated_gain_tokens, 19_999);
            assert_eq!(deferral.min_gain_tokens, 20_000);
        }
        other => panic!("expected Deferred, got {other:?}"),
    }
    assert_eq!(at, GainDecision::Micro);
}

#[test]
fn micro_zone_without_candidates_defers_instead_of_failing() {
    let decision = decide_auto_compact(
        &pressure(160_000, 200_000),
        &CompactConfig::default(),
        0,
        false,
    );
    match decision {
        GainDecision::Deferred(deferral) => assert_eq!(
            deferral.reason,
            peri_acp_types::compact::CompactDeferralReason::NoCandidates
        ),
        other => panic!("expected Deferred, got {other:?}"),
    }
}

#[test]
fn full_condition_requires_the_larger_of_minimum_gain_and_reclaim() {
    let config = CompactConfig::default();
    // P=190k, target=200k-13k=187k → R=4k（2% 下限），max(M,R)=20k。
    let below = pressure(190_000, 200_000);
    assert_eq!(below.target_reclaim_tokens(), 4_000);
    assert_eq!(
        decide_auto_compact(&below, &config, 19_000, true),
        GainDecision::Full(FullEscalationReason::InsufficientReclaim)
    );
    assert_eq!(
        decide_auto_compact(&below, &config, 20_000, true),
        GainDecision::Micro
    );

    // 设计 7.2：P=210k，输出预留 8k、安全余量 5k → R=23k，max(M,R)=23k。
    let high = pressure(210_000, 200_000);
    assert_eq!(high.target_reclaim_tokens(), 23_000);
    assert_eq!(
        decide_auto_compact(&high, &config, 20_000, true),
        GainDecision::Full(FullEscalationReason::InsufficientReclaim),
        "不能只因达到 10% 就选择 Micro"
    );
    assert_eq!(
        decide_auto_compact(&high, &config, 23_000, true),
        GainDecision::Micro
    );
}

#[test]
fn full_condition_without_candidates_keeps_the_existing_full_semantics() {
    assert_eq!(
        decide_auto_compact(
            &pressure(196_000, 200_000),
            &CompactConfig::default(),
            0,
            false
        ),
        GainDecision::Full(FullEscalationReason::ForceThresholdExceeded)
    );
}

#[test]
fn zero_context_window_never_uses_a_zero_threshold_auto_micro() {
    let config = CompactConfig {
        micro_compact_threshold: 0.0,
        ..Default::default()
    };
    let decision = decide_auto_compact(&pressure(0, 0), &config, u64::MAX, true);
    match decision {
        GainDecision::Deferred(deferral) => assert_eq!(
            deferral.reason,
            peri_acp_types::compact::CompactDeferralReason::MissingContextBudget
        ),
        other => panic!("expected Deferred, got {other:?}"),
    }
}

#[test]
fn below_the_micro_threshold_skips() {
    assert_eq!(
        decide_auto_compact(
            &pressure(100_000, 200_000),
            &CompactConfig::default(),
            50_000,
            true
        ),
        GainDecision::Skip
    );
}

// ── 端到端：延后零写入，足量收益允许连续 Micro ─────────────────────────────

#[tokio::test]
async fn deferred_round_persists_no_projection_and_verifies_no_archive() {
    let bound = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    // 3 轮 × 24k 字符 ≈ 18k 可回收 token，低于 200k 窗口的 20k 门槛。
    for round in 0..3 {
        append_round(&mut transcript, round);
    }
    let config = CompactConfig {
        micro_compact_stale_steps: 1,
        ..Default::default()
    };
    let result = compact_once(
        &mut transcript,
        &bound.thread_id,
        &config,
        &pressure(160_000, 200_000),
    )
    .await;

    assert_eq!(result.outcome(), CompactOutcome::Deferred);
    assert_eq!(result.affected_count, 0);
    assert_eq!(result.changed_messages, 0);
    assert!(result.failure.is_none(), "延后不是错误");
    let deferral = result.deferral.expect("延后必须携带原因");
    assert!(deferral.estimated_gain_tokens < deferral.min_gain_tokens);
    assert!(
        !has_persisted_projection(&transcript),
        "延后不得持久化任何投影/目录（因此也没有归档正文验证）"
    );
}

#[tokio::test]
async fn sufficient_gain_commits_once_and_allows_successive_micro_rounds() {
    let bound = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    for round in 0..8 {
        append_round(&mut transcript, round);
    }
    let config = CompactConfig {
        micro_compact_stale_steps: 1,
        ..Default::default()
    };
    let first = compact_once(
        &mut transcript,
        &bound.thread_id,
        &config,
        &pressure(160_000, 200_000),
    )
    .await;
    assert_eq!(first.outcome(), CompactOutcome::MicroApplied);
    assert!(first.estimated_tokens_saved >= 20_000);
    assert!(has_persisted_projection(&transcript));

    // 新增内容足以再次跨过 10% 门槛：每一轮都确有足量合法收益。
    for round in 8..14 {
        append_round(&mut transcript, round);
    }
    let second = compact_once(
        &mut transcript,
        &bound.thread_id,
        &config,
        &pressure(160_000, 200_000),
    )
    .await;
    assert_eq!(second.outcome(), CompactOutcome::MicroApplied);
    assert!(second.estimated_tokens_saved >= 20_000);
}

#[tokio::test]
async fn smart_flag_uses_the_same_admission_conclusion() {
    let bound = TestSession::open().await;
    let mut plain =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    for round in 0..3 {
        append_round(&mut plain, round);
    }
    let mut smart =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    for round in 0..3 {
        append_round(&mut smart, round);
    }
    let plain_result = compact_once(
        &mut plain,
        &bound.thread_id,
        &CompactConfig {
            micro_compact_stale_steps: 1,
            ..Default::default()
        },
        &pressure(160_000, 200_000),
    )
    .await;
    let smart_result = compact_once(
        &mut smart,
        &bound.thread_id,
        &CompactConfig {
            smart_compact_enabled: true,
            micro_compact_stale_steps: 1,
            ..Default::default()
        },
        &pressure(160_000, 200_000),
    )
    .await;

    assert_eq!(plain_result.outcome(), CompactOutcome::Deferred);
    assert_eq!(smart_result.outcome(), CompactOutcome::Deferred);
    assert_eq!(plain_result.affected_count, smart_result.affected_count);
    assert_eq!(
        plain_result.deferral.map(|d| d.reason),
        smart_result.deferral.map(|d| d.reason)
    );
    assert!(!has_persisted_projection(&plain));
    assert!(!has_persisted_projection(&smart));

    // 足量收益时两者都提交；Smart 仅保留兼容的 strategy/outcome 标签。
    let mut plain_large =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    for round in 0..8 {
        append_round(&mut plain_large, round);
    }
    let mut smart_large =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    for round in 0..8 {
        append_round(&mut smart_large, round);
    }
    let plain_applied = compact_once(
        &mut plain_large,
        &bound.thread_id,
        &CompactConfig {
            micro_compact_stale_steps: 1,
            ..Default::default()
        },
        &pressure(160_000, 200_000),
    )
    .await;
    let smart_applied = compact_once(
        &mut smart_large,
        &bound.thread_id,
        &CompactConfig {
            smart_compact_enabled: true,
            micro_compact_stale_steps: 1,
            ..Default::default()
        },
        &pressure(160_000, 200_000),
    )
    .await;
    assert_eq!(plain_applied.outcome(), CompactOutcome::MicroApplied);
    assert_eq!(smart_applied.outcome(), CompactOutcome::SmartApplied);
    assert_eq!(plain_applied.affected_count, smart_applied.affected_count);
    assert_eq!(
        smart_applied.strategy,
        CompactStrategy::Smart,
        "保留 Smart 策略兼容值"
    );
}

#[test]
fn deferred_end_event_keeps_a_serializable_contract() {
    let event = crate::agent::events_v2::ObserveEvent::CompactEnded {
        turn_id: peri_acp_types::session::TurnId::new(),
        agent_id: peri_acp_types::identity::AgentId::new(),
        step: 3,
        strategy: CompactStrategy::Micro,
        outcome: CompactOutcome::Deferred,
        deferral: Some(peri_acp_types::compact::CompactDeferral {
            reason: peri_acp_types::compact::CompactDeferralReason::InsufficientGain,
            estimated_gain_tokens: 19_999,
            min_gain_tokens: 20_000,
        }),
    };
    let json = serde_json::to_string(&event).unwrap();
    let back: crate::agent::events_v2::ObserveEvent = serde_json::from_str(&json).unwrap();
    match back {
        crate::agent::events_v2::ObserveEvent::CompactEnded {
            outcome,
            deferral: Some(deferral),
            ..
        } => {
            assert_eq!(outcome, CompactOutcome::Deferred);
            assert_eq!(
                deferral.reason,
                peri_acp_types::compact::CompactDeferralReason::InsufficientGain
            );
            assert_eq!(deferral.estimated_gain_tokens, 19_999);
            assert_eq!(deferral.min_gain_tokens, 20_000);
        }
        other => panic!("unexpected roundtrip: {other:?}"),
    }
}

// ── Bash 保留策略 ───────────────────────────────────────────────────────────

#[test]
fn builtin_bash_is_preview_only_and_keeps_its_arguments() {
    let bash = peri_acp_types::builtin_mcp::find("workspace")
        .expect("workspace instance")
        .tools
        .iter()
        .find(|tool| tool.original_name == "Bash")
        .expect("Bash must be declared");
    assert_eq!(
        bash.context_retention,
        crate::tools::ContextRetention::PreviewOnly,
        "Bash 不得再整组省略"
    );

    // PreviewOnly 只缩短结果并保留回查后缀；工具参数逐值保持 canonical。
    let mut transcript = MessageTranscript::new();
    let arguments = serde_json::json!({"command": "printf 'canonical-argument'", "cwd": "/tmp"});
    let call = BaseMessage::ai_with_tool_calls(
        MessageContent::text("run"),
        vec![ToolCallRequest::new("call-bash", "Bash", arguments.clone())],
    );
    let result = BaseMessage::Tool {
        id: MessageId::from(uuid::Uuid::from_u128(0x55)),
        tool_call_id: "call-bash".into(),
        content: MessageContent::text(format!("HEAD{}TAIL", "x".repeat(20_000))),
        is_error: false,
        execution: None,
        subagent_failure: None,
    };
    transcript.append(call.clone());
    transcript.append(result.clone());
    // 仅提供暴露/终态/归档范围事实；不注入 tool_retention，让 planner 使用 builtin 声明。
    let mut facts = MicroPlanningFacts {
        archive_scope: Some(ArchivePlanningScope {
            thread_id: "session".into(),
            committed_tail: result.id(),
        }),
        ..Default::default()
    };
    facts.exposed_message_ids.extend([call.id(), result.id()]);
    facts.closed_call_ids.insert("call-bash".into());
    let plan = super::planner::plan_micro_with_facts(
        &transcript,
        &CompactConfig {
            micro_compact_stale_steps: 0,
            ..Default::default()
        },
        &pressure(160_000, 200_000),
        &facts,
        &ProviderCapabilities::openai(),
    )
    .unwrap();
    assert!(
        !plan.actions.iter().any(|entry| matches!(
            entry.action,
            super::ProjectionAction::OmitToolExchange { .. }
        )),
        "Bash 不得进入整组省略白名单"
    );
    assert!(plan.actions.iter().any(|entry| matches!(
        entry.action,
        super::ProjectionAction::CompactToolResult { .. }
    )));
    let rendered =
        super::projection::render_micro_plan(&transcript, &plan, &ProviderCapabilities::openai())
            .expect("preview must render");
    let call_message = rendered
        .iter()
        .find(|message| message.id() == call.id())
        .expect("call message remains");
    assert_eq!(
        call_message.tool_calls()[0].arguments,
        arguments,
        "预览不得改写工具参数"
    );
    let result_message = rendered
        .iter()
        .find(|message| message.id() == result.id())
        .expect("preview result remains");
    assert!(result_message.content().contains("HEAD"));
    assert!(result_message.content().contains("TAIL"));
    assert!(result_message.content().contains("字符已省略"));
    assert!(result_message
        .content()
        .contains("ReadContextArchive archive_ref="));
}
