use super::*;
use crate::messages::MessageContent;
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};

fn make_usage(input_tokens: u32) -> TokenUsage {
    TokenUsage {
        input_tokens,
        output_tokens: 10,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: Some(input_tokens / 2),
    }
}

fn append_full_result(state: &mut CompactBudgetRecovery, transcript: &mut MessageTranscript) {
    let before_len = transcript.len();
    // 模拟Full lifecycle的追加边界：摘要和reinject具有全新ID，但不是新work。
    transcript.append(BaseMessage::human("new summary"));
    transcript.append(BaseMessage::human(
        "[激活的 Skill 指令: /skills/a/SKILL.md]",
    ));
    state.record_full_applied(transcript, before_len);
}

fn append_reminder(transcript: &mut MessageTranscript) {
    transcript.append_system_reminder(
        TrustedSystemReminderFactory::for_producer()
            .construct(SystemReminder {
                version: SYSTEM_REMINDER_VERSION,
                category: ReminderCategory::Task,
                source: ReminderSource("compact_progress_test".into()),
                kind: "steering".into(),
                severity: ReminderSeverity::Info,
                delivery: ReminderDelivery::Configurable,
                audiences: ReminderAudiences(vec![ReminderAudience::Model]),
                body: "继续完成目标".repeat(100),
                summary: None,
                metadata: serde_json::json!({}),
            })
            .unwrap(),
    );
}

fn observe(
    state: &mut CompactBudgetRecovery,
    transcript: &MessageTranscript,
    input_tokens: u32,
) -> AgentResult<()> {
    let probe = state.begin_request(transcript);
    state.observe_response(
        probe,
        Some(&make_usage(input_tokens)),
        &CompactConfig::default(),
        &ContextBudget::new(100_000),
    )
}

/// [回归测试] 新summary/AI/Reminder不能使同一内容无限重新获得Full机会。
#[test]
fn test_budget_recovery_reminders_and_generated_messages_do_not_reset_attempts() {
    let mut transcript = MessageTranscript::new();
    transcript.append(BaseMessage::human("original task"));
    append_reminder(&mut transcript);
    let mut state = CompactBudgetRecovery::default();
    append_full_result(&mut state, &mut transcript);
    observe(&mut state, &transcript, 96_000).unwrap();
    transcript.append(BaseMessage::ai("继续执行"));
    append_reminder(&mut transcript);
    append_full_result(&mut state, &mut transcript);
    let error = observe(&mut state, &transcript, 96_000).unwrap_err();
    assert!(matches!(
        error,
        AgentError::CompactBudgetUnrecovered {
            input_tokens: 96_000,
            context_window: 100_000,
            full_attempts: 2,
        }
    ));
    let failure = peri_acp_types::session::ExecutionFailure::from_agent_error(&error);
    assert!(failure
        .public_message
        .contains("did not restore the context budget"));
    assert!(failure.public_message.contains("2 attempts"));
    assert_eq!(
        transcript
            .entries()
            .iter()
            .filter(|entry| entry.as_message().is_none())
            .count(),
        2,
        "预算失败不能丢弃保留的控制指令"
    );
}

#[test]
fn test_budget_recovery_below_full_threshold_resets_attempts() {
    let mut transcript = MessageTranscript::new();
    let mut state = CompactBudgetRecovery::default();
    append_full_result(&mut state, &mut transcript);
    observe(&mut state, &transcript, 96_000).unwrap();
    append_full_result(&mut state, &mut transcript);
    observe(&mut state, &transcript, 80_000).unwrap();
    append_full_result(&mut state, &mut transcript);
    observe(&mut state, &transcript, 96_000).unwrap();
    assert_eq!(state.unrecovered_observations, 1, "Micro区间仍有可用预算");
}

#[test]
fn test_budget_recovery_new_tool_output_allows_another_full_epoch() {
    let mut transcript = MessageTranscript::new();
    let mut state = CompactBudgetRecovery::default();
    append_full_result(&mut state, &mut transcript);
    observe(&mut state, &transcript, 96_000).unwrap();
    transcript.append(BaseMessage::tool_result(
        "new-tool",
        MessageContent::text("x".repeat(80_000)),
    ));
    append_full_result(&mut state, &mut transcript);
    observe(&mut state, &transcript, 96_000).unwrap();
    assert_eq!(
        state.unrecovered_observations, 1,
        "真实新工具结果允许重新压缩"
    );
}

#[test]
fn test_budget_recovery_request_after_new_user_work_is_not_full_probe() {
    let mut transcript = MessageTranscript::new();
    let mut state = CompactBudgetRecovery::default();
    append_full_result(&mut state, &mut transcript);
    observe(&mut state, &transcript, 96_000).unwrap();
    append_full_result(&mut state, &mut transcript);
    transcript.append(BaseMessage::human("new user work"));
    observe(&mut state, &transcript, 96_000).unwrap();
    assert_eq!(
        state.unrecovered_observations, 0,
        "新用户输入使请求不再代表Full输出"
    );
}

#[test]
fn test_budget_recovery_stale_request_and_repeated_response_are_ignored() {
    let mut transcript = MessageTranscript::new();
    let mut state = CompactBudgetRecovery::default();
    append_full_result(&mut state, &mut transcript);
    let old_probe = state.begin_request(&transcript);
    append_full_result(&mut state, &mut transcript);
    let current_probe = state.begin_request(&transcript);
    let config = CompactConfig::default();
    let budget = ContextBudget::new(100_000);
    let usage = make_usage(96_000);
    state
        .observe_response(old_probe, Some(&usage), &config, &budget)
        .unwrap();
    assert_eq!(state.unrecovered_observations, 0);
    state
        .observe_response(current_probe, Some(&usage), &config, &budget)
        .unwrap();
    state
        .observe_response(current_probe, Some(&usage), &config, &budget)
        .unwrap();
    assert_eq!(state.unrecovered_observations, 1, "同一请求不能被观察两次");
}

#[test]
fn test_budget_recovery_missing_and_zero_usage_never_borrow_stale_pressure() {
    let mut transcript = MessageTranscript::new();
    let mut state = CompactBudgetRecovery::default();
    append_full_result(&mut state, &mut transcript);
    observe(&mut state, &transcript, 96_000).unwrap();
    append_full_result(&mut state, &mut transcript);
    let probe = state.begin_request(&transcript);
    let config = CompactConfig::default();
    let budget = ContextBudget::new(100_000);
    state
        .observe_response(probe, None, &config, &budget)
        .unwrap();
    state
        .observe_response(probe, Some(&make_usage(0)), &config, &budget)
        .unwrap();
    assert_eq!(state.unrecovered_observations, 1);
    observe(&mut state, &transcript, 40_000).unwrap();
    assert_eq!(state.unrecovered_observations, 0);
}

#[test]
fn test_budget_recovery_previous_request_after_same_full_is_not_current_probe() {
    let mut transcript = MessageTranscript::new();
    let mut state = CompactBudgetRecovery::default();
    append_full_result(&mut state, &mut transcript);
    let previous_request = state.begin_request(&transcript);
    let current_request = state.begin_request(&transcript);
    let config = CompactConfig::default();
    let budget = ContextBudget::new(100_000);
    let usage = make_usage(96_000);
    state
        .observe_response(previous_request, Some(&usage), &config, &budget)
        .unwrap();
    assert_eq!(
        state.unrecovered_observations, 0,
        "同一次Full的旧请求也不能作为新请求证据"
    );
    state
        .observe_response(current_request, Some(&usage), &config, &budget)
        .unwrap();
    assert_eq!(state.unrecovered_observations, 1);
}
