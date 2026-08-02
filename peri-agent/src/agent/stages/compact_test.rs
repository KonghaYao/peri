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

    let output = run_compact(CompactInput {
        context: ctx,
        has_tool_calls: true,
    })
    .await
    .unwrap();

    assert!(
        output.compacted,
        "已应用的 Micro 应使 Compact stage 报告 compacted"
    );
    assert_eq!(
        token_tracker.read().estimated_context_tokens(),
        Some(196_000),
        "Full 失败后不得将已应用 Micro 伪装为 Full completion 并 reset token tracker"
    );
}

#[tokio::test]
async fn test_compact_stage_smart_applied_then_full_failure_is_compacted_without_tracker_reset() {
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

    let output = run_compact(CompactInput {
        context: ctx,
        has_tool_calls: true,
    })
    .await
    .unwrap();

    assert!(
        output.compacted,
        "已应用的 Smart 应使 Compact stage 报告 compacted"
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

    let output = run_compact(CompactInput {
        context: ctx,
        has_tool_calls: true,
    })
    .await
    .unwrap();

    assert!(
        output.compacted,
        "MicroAppliedThenFullFailed 仍有实际 mutation"
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
