use super::projection::{
    estimate_projection_chars, render_llm_view, MicroCompactPlan, ProjectionAction,
    ProjectionActionEntry, ProjectionTarget, ProviderCapabilities,
};
use crate::messages::{BaseMessage, ContentBlock};
use crate::session::transcript::MessageTranscript;
fn transcript_with_tool_exchange(
    tool_call_id: &str,
    tool_input: serde_json::Value,
    tool_result_text: &str,
    is_error: bool,
) -> MessageTranscript {
    let mut t = MessageTranscript::new();
    let blocks = vec![
        ContentBlock::text("I'll use a tool"),
        ContentBlock::tool_use(tool_call_id, "Bash", tool_input),
    ];
    let ai_msg = BaseMessage::ai_from_blocks(blocks);
    t.append(ai_msg);
    if is_error {
        t.append(BaseMessage::tool_error(tool_call_id, tool_result_text));
    } else {
        t.append(BaseMessage::tool_result(tool_call_id, tool_result_text));
    }
    t
}
#[test]
fn estimate_projection_chars_ignores_directive_targeting_reminder() {
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };
    let mut transcript = MessageTranscript::new();
    let reminder = TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSource("estimate_test".into()),
            kind: "status".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: "control".into(),
            summary: None,
            metadata: serde_json::json!({}),
        })
        .unwrap();
    let id = transcript.append_system_reminder(reminder);
    let action = ProjectionActionEntry {
        message_id: id,
        target: ProjectionTarget::Message,
        action: ProjectionAction::CompactToolResult {
            keep_head: 1,
            keep_tail: 1,
            preserve_recovery_handle: true,
        },
    };

    assert_eq!(estimate_projection_chars(&transcript, &[action]), (0, 0));
    assert_eq!(transcript.flags(id), Default::default());
}

#[test]
fn test_estimate_projection_chars_counts_only_actually_truncated_values() {
    let selected = "x".repeat(600);
    let unselected = "y".repeat(700);
    let transcript = transcript_with_tool_exchange(
        "tc_1",
        serde_json::json!({
            "selected": selected,
            "unselected": unselected,
            "short": "keep",
        }),
        "short result",
        false,
    );
    let visible = transcript.visible_messages();
    let actions = vec![
        ProjectionActionEntry {
            message_id: visible[0].id(),
            target: ProjectionTarget::ToolCall {
                tool_call_id: "tc_1".into(),
            },
            action: ProjectionAction::CompactToolInput {
                fields: vec!["missing".into(), "selected".into(), "short".into()],
                keep_head: 10,
                keep_tail: 5,
            },
        },
        ProjectionActionEntry {
            message_id: visible[1].id(),
            target: ProjectionTarget::Message,
            action: ProjectionAction::CompactToolResult {
                keep_head: 10,
                keep_tail: 5,
                preserve_recovery_handle: false,
            },
        },
    ];

    assert_eq!(
        estimate_projection_chars(&transcript, &actions),
        (0, 0),
        "ToolUse 输入受硬保护，不应计入可回收字符"
    );
}

#[test]
fn test_estimate_projection_chars_deduplicates_repeated_tool_input_fields() {
    let prompt = "x".repeat(600);
    let transcript = transcript_with_tool_exchange(
        "tc_1",
        serde_json::json!({"prompt": prompt}),
        "short result",
        false,
    );
    let message_id = transcript.visible_messages()[0].id();
    let single_field_action = ProjectionActionEntry {
        message_id,
        target: ProjectionTarget::ToolCall {
            tool_call_id: "tc_1".into(),
        },
        action: ProjectionAction::CompactToolInput {
            fields: vec!["prompt".into()],
            keep_head: 350,
            keep_tail: 100,
        },
    };
    let repeated_field_action = ProjectionActionEntry {
        message_id,
        target: ProjectionTarget::ToolCall {
            tool_call_id: "tc_1".into(),
        },
        action: ProjectionAction::CompactToolInput {
            fields: vec!["prompt".into(), "prompt".into()],
            keep_head: 350,
            keep_tail: 100,
        },
    };

    let single_estimate = estimate_projection_chars(&transcript, &[single_field_action]);
    let repeated_estimate = estimate_projection_chars(&transcript, &[repeated_field_action]);

    assert_eq!(repeated_estimate.0, single_estimate.0);
    assert_eq!(repeated_estimate.1, single_estimate.1);
    assert_eq!(single_estimate, (0, 0));
}

#[test]
fn duplicate_or_conflicting_tool_result_actions_fail_closed_for_render_and_estimate() {
    let transcript = transcript_with_tool_exchange(
        "tc_conflict",
        serde_json::json!({"path": "f"}),
        &"x".repeat(600),
        false,
    );
    let message_id = transcript.visible_messages()[1].id();
    let compact = ProjectionAction::CompactToolResult {
        keep_head: 10,
        keep_tail: 10,
        preserve_recovery_handle: false,
    };

    for actions in [
        vec![
            ProjectionActionEntry {
                message_id,
                target: ProjectionTarget::Message,
                action: compact.clone(),
            },
            ProjectionActionEntry {
                message_id,
                target: ProjectionTarget::Message,
                action: compact.clone(),
            },
        ],
        vec![
            ProjectionActionEntry {
                message_id,
                target: ProjectionTarget::ContentBlock { index: 0 },
                action: compact.clone(),
            },
            ProjectionActionEntry {
                message_id,
                target: ProjectionTarget::Message,
                action: ProjectionAction::CompactToolResult {
                    keep_head: 1_000,
                    keep_tail: 1_000,
                    preserve_recovery_handle: false,
                },
            },
        ],
    ] {
        let plan = MicroCompactPlan {
            actions: actions.clone(),
            ..Default::default()
        };
        let rendered =
            render_llm_view(&transcript, &plan, &ProviderCapabilities::default()).unwrap();
        assert_eq!(rendered[1].content(), "x".repeat(600));
        assert_eq!(estimate_projection_chars(&transcript, &actions), (0, 0));
    }
}

#[test]
fn test_estimator_fails_closed_for_duplicate_tool_result_actions() {
    let mut transcript = MessageTranscript::new();
    let result = BaseMessage::tool_result("tc", "x".repeat(600));
    let result_id = result.id();
    transcript.append(result);
    let action = ProjectionActionEntry {
        message_id: result_id,
        target: ProjectionTarget::Message,
        action: ProjectionAction::CompactToolResult {
            keep_head: 10,
            keep_tail: 10,
            preserve_recovery_handle: false,
        },
    };

    assert_eq!(
        estimate_projection_chars(&transcript, &[action.clone(), action]),
        (0, 0)
    );
}
