//! 从 reason.rs 分离的测试模块
use super::*;
use crate::agent::events_v2::{EventBus, EventBusConfig, ObserveEvent, TurnErrorReason};
use crate::agent::stages::StageContext;
use crate::messages::BaseMessage;
#[cfg(test)]
use crate::messages::MessageContent;
use crate::session::store::FrozenContext;
use crate::session::Session;
use std::sync::Arc;

fn make_context() -> StageContext {
    let cwd: Arc<str> = Arc::from("/tmp/test");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    StageContext::new(turn, session.transcript(), session.queue().clone())
}

/// 验证 run_reason 在多步 turn 中 emit 的 LlmCallEnd.step 与 turn.current_step() 一致
///
/// Top 10 回归锁定：reason.rs:17 `let step = ctx.session.turn.current_step();`，
/// 错误路径（reason.rs:66）与成功路径（reason.rs:88）均必须 emit 此 step。
/// 使用 NullReactLLM（默认 fallback）触发错误路径。
#[tokio::test]
async fn test_run_reason_emits_llm_call_end_with_correct_step() {
    // Arrange：注入可观测的 EventBus，subscribe 后才能收到 broadcast 事件
    let (bus, mut handles) = EventBus::new(EventBusConfig::default());
    let event_bus = Arc::new(bus);

    let cwd: Arc<str> = Arc::from("/tmp/step");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_event_bus(event_bus)
        .build();

    // Act 1：step=0（turn 初始）→ NullReactLLM 触发错误路径 emit
    assert_eq!(ctx.session.turn.current_step(), 0);
    let _ = run_reason(ReasonInput {
        context: ctx.clone(),
        has_tool_calls: false,
    })
    .await;

    // Assert 1：收到 LlmCallStart 与 LlmCallEnd，且 step==0
    let ev_start0 = handles.try_observe().expect("step 0 应收到 LlmCallStart");
    assert!(
        matches!(ev_start0, ObserveEvent::LlmCallStart { step: 0, .. }),
        "LlmCallStart.step 应为 0，实际 {:?}",
        ev_start0
    );
    let ev_end0 = handles.try_observe().expect("step 0 应收到 LlmCallEnd");
    assert!(
        matches!(ev_end0, ObserveEvent::LlmCallEnd { step: 0, .. }),
        "LlmCallEnd.step 应为 0，实际 {:?}",
        ev_end0
    );

    // 排空 step 0 的 TurnError 事件（新增的 TurnError emit 会在同一步中产生 3 个事件）
    let _ = handles.try_observe();

    // Act 2：推进 step → step=1，再次 run_reason
    ctx.session.turn.advance_step();
    assert_eq!(ctx.session.turn.current_step(), 1);
    let _ = run_reason(ReasonInput {
        context: ctx,
        has_tool_calls: false,
    })
    .await;

    // Assert 2：收到 LlmCallEnd 且 step==1（与 step 0 不同）
    let _ev_start1 = handles.try_observe().expect("step 1 应收到 LlmCallStart");
    let ev_end1 = handles.try_observe().expect("step 1 应收到 LlmCallEnd");
    assert!(
        matches!(ev_end1, ObserveEvent::LlmCallEnd { step: 1, .. }),
        "LlmCallEnd.step 应为 1（推进后），实际 {:?}",
        ev_end1
    );
}

#[tokio::test]
async fn test_reason_with_null_llm_returns_interrupted() {
    // 默认 StageContext 用 NullReactLLM，调用返回 Interrupted
    let ctx = make_context();
    let input = ReasonInput {
        context: ctx,
        has_tool_calls: false,
    };
    let result = run_reason(input).await;
    assert!(
        matches!(result, Err(AgentError::Interrupted)),
        "NullReactLLM 应返回 Interrupted，实际 {:?}",
        result
    );
}

#[tokio::test]
async fn test_reason_captures_message_snapshot() {
    // 使用自定义 MockLLM 测试 snapshot
    let ctx = make_context();
    ctx.session
        .transcript
        .write()
        .append(BaseMessage::human(MessageContent::text("user message")));

    // NullReactLLM 即使失败，messages_snapshot 也应该在错误返回前已被捕获
    // 但我们在错误路径中直接 return，所以这个测试只验证 NullReactLLM 行为
    let input = ReasonInput {
        context: ctx,
        has_tool_calls: false,
    };
    let result = run_reason(input).await;
    assert!(result.is_err());
}

/// 验证 LLM 调用失败时 TurnError 事件被 emit 到 EventBus
///
/// 覆盖：reason.rs 错误分支 → TurnErrorReason::LlmFailure → v2_bridge → AgentExecutionFailed
#[tokio::test]
async fn test_run_reason_emits_turn_error_on_llm_failure() {
    let (bus, mut handles) = EventBus::new(EventBusConfig::default());
    let event_bus = Arc::new(bus);

    let cwd: Arc<str> = Arc::from("/tmp/llm_failure");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_event_bus(event_bus)
        .build();

    let _ = run_reason(ReasonInput {
        context: ctx,
        has_tool_calls: false,
    })
    .await;

    // 收集所有 ObserveEvent，检查 TurnError 是否存在
    let mut found_turn_error = false;
    let mut found_llm_call_end = false;
    while let Some(ev) = handles.try_observe() {
        match ev {
            ObserveEvent::TurnError { reason, .. } => {
                assert_eq!(reason, TurnErrorReason::LlmFailure);
                found_turn_error = true;
            }
            ObserveEvent::LlmCallEnd { .. } => {
                found_llm_call_end = true;
            }
            _ => {}
        }
    }
    assert!(found_turn_error, "应在 LLM 失败时 emit TurnError");
    assert!(found_llm_call_end, "应同时 emit LlmCallEnd（现有行为）");
}

// ── 特征化测试：truncated_content 行为基线 ───────────────────────────────

#[test]
#[allow(deprecated)]
fn test_truncated_content_on_text_message() {
    // 超长文本消息被截断到 ~100 字符 + truncation note
    let long_text = "a".repeat(200);
    let msg = BaseMessage::human(MessageContent::text(long_text.clone()));

    let result = msg.truncated_content(100);
    assert!(result.is_some(), "超长文本应返回截断后结果");

    let truncated_msg = result.unwrap();
    let content = match truncated_msg.message_content() {
        MessageContent::Text(t) => t,
        other => panic!("截断后仍应是 Text 内容，实际: {:?}", other),
    };

    // 验证截断后内容：前 100 字符 + truncation note
    assert!(content.contains("a"), "截断后应有原始内容");
    assert!(
        content.contains("[truncated: content shortened by Micro Compact]"),
        "截断后应有 truncation note"
    );
    // 原始 200 字符不应完整存在
    assert!(
        !content.starts_with(&long_text),
        "原始完整文本不应出现在截断后内容中"
    );
}
