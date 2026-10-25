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
    // 模拟Full lifecycle：新工作判定发生在追加之前（摘要和reinject具有全新ID，但不是新work）。
    let new_work = state.new_work_since_full(transcript);
    transcript.append(BaseMessage::human("new summary"));
    transcript.append(BaseMessage::human(
        "[激活的 Skill 指令: /skills/a/SKILL.md]",
    ));
    state.record_full_applied(
        new_work,
        ResidentFullSnapshot::capture(
            transcript,
            ResidentHistoryPressure::from_transcript(0, transcript),
        ),
    );
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

fn begin(
    state: &mut CompactBudgetRecovery,
    transcript: &MessageTranscript,
) -> Option<FullBudgetProbe> {
    let new_visible_work =
        CompactBudgetRecovery::has_new_work_after(state.full_watermark, transcript, true);
    state.begin_request(
        new_visible_work,
        transcript.entries().last().map(|entry| entry.id()),
    )
}

fn observe(
    state: &mut CompactBudgetRecovery,
    transcript: &MessageTranscript,
    input_tokens: u32,
) -> AgentResult<()> {
    let probe = begin(state, transcript);
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
    let old_probe = begin(&mut state, &transcript);
    append_full_result(&mut state, &mut transcript);
    let current_probe = begin(&mut state, &transcript);
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
    let probe = begin(&mut state, &transcript);
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

/// [回归测试] 新工作判定按稳定身份（末尾摘要 id）定位，不按条目位置：收缩释放
/// 水印之前的已排除条目会让位置整体前移，但「Full 之后没有新工作」的结论不变；
/// 水印缺席（rewind/重建）等价于旧实现的 `end < full_end`，按有新工作处理。
#[test]
fn test_budget_recovery_watermark_survives_shrink_and_missing_watermark_is_new_work() {
    let prior = BaseMessage::human("prior work");
    let summary = BaseMessage::human("new summary");
    let summary_id = summary.id();
    let mut before_shrink = MessageTranscript::new();
    before_shrink.append(prior.clone());
    before_shrink.append(summary.clone());
    let mut state = CompactBudgetRecovery::default();
    state.record_full_applied(
        true,
        ResidentFullSnapshot::capture(
            &before_shrink,
            ResidentHistoryPressure::from_transcript(0, &before_shrink),
        ),
    );
    assert_eq!(
        state.full_watermark,
        Some(summary_id),
        "水印取提交后末尾条目"
    );

    // 收缩：水印之前的已排除条目被释放（位置前移），水印本身保留。
    let mut after_shrink = MessageTranscript::new();
    after_shrink.append(summary.clone());
    assert!(
        !state.new_work_since_full(&after_shrink),
        "位置前移不得被当成新工作"
    );

    // 水印之后确有新工作时仍判为新工作。
    let mut with_new_work = MessageTranscript::new();
    with_new_work.append(summary.clone());
    with_new_work.append(BaseMessage::tool_result(
        "new-tool",
        MessageContent::text("x"),
    ));
    assert!(state.new_work_since_full(&with_new_work));

    // 水印条目被 rewind/重建释放：旧边界失效，按有新工作处理。
    let mut rebuilt = MessageTranscript::new();
    rebuilt.append(prior);
    assert!(
        state.new_work_since_full(&rebuilt),
        "水印缺席必须按有新工作处理"
    );
}

#[test]
fn test_budget_recovery_previous_request_after_same_full_is_not_current_probe() {
    let mut transcript = MessageTranscript::new();
    let mut state = CompactBudgetRecovery::default();
    append_full_result(&mut state, &mut transcript);
    let previous_request = begin(&mut state, &transcript);
    let current_request = begin(&mut state, &transcript);
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
