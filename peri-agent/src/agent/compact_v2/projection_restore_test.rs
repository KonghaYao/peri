use super::projection::{
    render_llm_view, MessageProjectionDirective, PersistedDirectiveRestore, ProjectionAction,
    ProjectionActionEntry, ProjectionTarget, ProviderCapabilities, PROJECTION_POLICY_VERSION,
};
use crate::messages::{BaseMessage, ContentBlock, MessageId};
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
fn test_plan_from_persisted_directives_empty_transcript_is_absent() {
    let transcript = MessageTranscript::new();
    assert!(matches!(
        super::projection::plan_from_persisted_directives(&transcript, PROJECTION_POLICY_VERSION,),
        PersistedDirectiveRestore::Absent
    ));
}

#[test]
fn test_plan_from_persisted_directives_version_mismatch_is_invalid() {
    let mut transcript = MessageTranscript::new();
    let message = BaseMessage::human("hello");
    let message_id = message.id();
    transcript.append(message);
    transcript.set_flags_projection(
        message_id,
        MessageProjectionDirective {
            policy_version: PROJECTION_POLICY_VERSION + 1,
            entries: vec![],
            superseded_by: None,
            ..Default::default()
        },
    );

    assert!(matches!(
        super::projection::plan_from_persisted_directives(&transcript, PROJECTION_POLICY_VERSION,),
        PersistedDirectiveRestore::Invalid
    ));
}

#[test]
fn excluded_legacy_directive_does_not_invalidate_visible_history() {
    let mut transcript = MessageTranscript::new();
    let excluded = transcript.append(BaseMessage::human("hidden legacy message"));
    transcript.set_flags_projection(
        excluded,
        MessageProjectionDirective {
            policy_version: PROJECTION_POLICY_VERSION + 1,
            entries: vec![],
            superseded_by: None,
            ..Default::default()
        },
    );
    transcript.set_excluded(excluded, true);
    let visible = transcript.append(BaseMessage::human("visible message"));

    assert!(matches!(
        super::projection::plan_from_persisted_directives(&transcript, PROJECTION_POLICY_VERSION),
        PersistedDirectiveRestore::Absent
    ));
    let projected = render_llm_view(&transcript, &[], &ProviderCapabilities::default()).unwrap();
    assert_eq!(projected.len(), 1);
    assert_eq!(projected[0].id(), visible);
    assert_eq!(projected[0].content(), "visible message");
    assert_eq!(transcript.len(), 2);
}

#[test]
fn test_plan_from_persisted_directives_truncated_without_directive_is_invalid() {
    let mut transcript = MessageTranscript::new();
    let message = BaseMessage::human("legacy content");
    let message_id = message.id();
    transcript.append(message);
    transcript.set_truncated(message_id, true);

    assert!(matches!(
        super::projection::plan_from_persisted_directives(&transcript, PROJECTION_POLICY_VERSION,),
        PersistedDirectiveRestore::Invalid
    ));
}

#[test]
fn test_plan_from_persisted_directives_wrong_message_id_is_invalid() {
    let mut transcript = MessageTranscript::new();
    let message = BaseMessage::tool_result("tc", "x".repeat(600));
    let message_id = message.id();
    transcript.append(message);
    transcript.set_flags_projection(
        message_id,
        MessageProjectionDirective {
            policy_version: PROJECTION_POLICY_VERSION,
            entries: vec![ProjectionActionEntry {
                message_id: MessageId::new(),
                target: ProjectionTarget::Message,
                action: ProjectionAction::CompactToolResult {
                    keep_head: 10,
                    keep_tail: 10,
                    preserve_recovery_handle: false,
                },
            }],
            superseded_by: None,
            ..Default::default()
        },
    );

    assert!(matches!(
        super::projection::plan_from_persisted_directives(&transcript, PROJECTION_POLICY_VERSION,),
        PersistedDirectiveRestore::Invalid
    ));
}

#[test]
fn test_persisted_restore_keeps_only_independent_tool_result_action() {
    let mut transcript = transcript_with_tool_exchange(
        "tc_legacy",
        serde_json::json!({"prompt": "x".repeat(600)}),
        &"R".repeat(600),
        false,
    );
    let (ai_id, result_id, canonical_content, canonical_tool_calls) = {
        let visible = transcript.visible_messages();
        let (canonical_content, canonical_tool_calls) = match &visible[0] {
            BaseMessage::Ai {
                content,
                tool_calls,
                ..
            } => (content.clone(), tool_calls.clone()),
            _ => unreachable!(),
        };
        (
            visible[0].id(),
            visible[1].id(),
            canonical_content,
            canonical_tool_calls,
        )
    };
    transcript.set_flags_projection(
        ai_id,
        MessageProjectionDirective {
            policy_version: 2,
            entries: vec![
                ProjectionActionEntry {
                    message_id: ai_id,
                    target: ProjectionTarget::ToolCall {
                        tool_call_id: "tc_legacy".into(),
                    },
                    action: ProjectionAction::CompactToolInput {
                        fields: vec!["prompt".into()],
                        keep_head: 10,
                        keep_tail: 10,
                    },
                },
                ProjectionActionEntry {
                    message_id: ai_id,
                    target: ProjectionTarget::Message,
                    action: ProjectionAction::Exclude,
                },
                ProjectionActionEntry {
                    message_id: ai_id,
                    target: ProjectionTarget::ContentBlock { index: 1 },
                    action: ProjectionAction::CompactText { max_chars: 1 },
                },
            ],
            superseded_by: None,
            ..Default::default()
        },
    );
    transcript.set_flags_projection(
        result_id,
        MessageProjectionDirective {
            policy_version: 2,
            entries: vec![ProjectionActionEntry {
                message_id: result_id,
                target: ProjectionTarget::Message,
                action: ProjectionAction::CompactToolResult {
                    keep_head: 20,
                    keep_tail: 20,
                    preserve_recovery_handle: false,
                },
            }],
            superseded_by: None,
            ..Default::default()
        },
    );

    let PersistedDirectiveRestore::Valid(plan) =
        super::projection::plan_from_persisted_directives(&transcript, PROJECTION_POLICY_VERSION)
    else {
        panic!("当前可解码 legacy directive 应恢复为安全子集");
    };
    assert!(matches!(
        &plan.actions[..],
        [ProjectionActionEntry {
            message_id,
            target: ProjectionTarget::Message,
            action: ProjectionAction::CompactToolResult { .. },
        }] if *message_id == result_id
    ));

    let projected = render_llm_view(&transcript, &plan.actions, &ProviderCapabilities::default())
        .expect("安全子集应可渲染");
    let BaseMessage::Ai {
        content,
        tool_calls,
        ..
    } = &projected[0]
    else {
        panic!("第一条消息应为 Ai");
    };
    assert_eq!(tool_calls, &canonical_tool_calls);
    assert_eq!(content, &canonical_content);
    assert!(projected[1].content().contains("字符已省略"));
}

#[test]
fn test_render_llm_view_from_persisted_tool_result_directive() {
    let mut transcript = transcript_with_tool_exchange(
        "tc_e2e",
        serde_json::json!({"cmd": "ls -la"}),
        &"A".repeat(800),
        false,
    );
    let result_id = transcript.visible_messages()[1].id();
    transcript.set_flags_projection(
        result_id,
        MessageProjectionDirective {
            policy_version: PROJECTION_POLICY_VERSION,
            entries: vec![ProjectionActionEntry {
                message_id: result_id,
                target: ProjectionTarget::Message,
                action: ProjectionAction::CompactToolResult {
                    keep_head: 50,
                    keep_tail: 50,
                    preserve_recovery_handle: false,
                },
            }],
            superseded_by: None,
            ..Default::default()
        },
    );

    let PersistedDirectiveRestore::Valid(plan) =
        super::projection::plan_from_persisted_directives(&transcript, PROJECTION_POLICY_VERSION)
    else {
        panic!("应恢复 ToolResult action");
    };
    let projected = render_llm_view(&transcript, &plan.actions, &ProviderCapabilities::default())
        .expect("render_llm_view 应成功");
    assert!(projected[1].content().contains("字符已省略"));
}

/// 验收（OPT-01）：纯恢复路径只读已提交 directive，不得调用字符估算。
///
/// 估算只属于规划路径（`plan_micro` 决定是否达回收目标）；恢复路径若重新估算，
/// 长会话每次 Reason 都会为已提交 directive 再扫一遍正文。
#[test]
fn test_plan_from_persisted_directives_does_not_estimate_chars() {
    let mut transcript = transcript_with_tool_exchange(
        "tc_no_estimate",
        serde_json::json!({"prompt": "x".repeat(600)}),
        &"R".repeat(4_000),
        false,
    );
    let result_id = transcript.visible_messages()[1].id();
    transcript.set_flags_projection(
        result_id,
        MessageProjectionDirective {
            policy_version: PROJECTION_POLICY_VERSION,
            entries: vec![ProjectionActionEntry {
                message_id: result_id,
                target: ProjectionTarget::Message,
                action: ProjectionAction::CompactToolResult {
                    keep_head: 200,
                    keep_tail: 100,
                    preserve_recovery_handle: false,
                },
            }],
            superseded_by: None,
            ..Default::default()
        },
    );

    super::projection::reset_estimate_calls();
    let restored =
        super::projection::plan_from_persisted_directives(&transcript, PROJECTION_POLICY_VERSION);

    assert!(
        matches!(restored, PersistedDirectiveRestore::Valid(_)),
        "已提交且版本匹配的 directive 应可直接恢复"
    );
    assert_eq!(
        super::projection::estimate_call_count(),
        0,
        "纯恢复路径不得调用字符估算"
    );
}

fn append_model_reminder(transcript: &mut MessageTranscript) -> MessageId {
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };
    let reminder = TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSource("projection_restore_test".into()),
            kind: "status".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: "canonical reminder must not leak through invalid projection".into(),
            summary: None,
            metadata: serde_json::json!({}),
        })
        .unwrap();
    transcript.append_system_reminder(reminder)
}

#[test]
fn persisted_reminder_metadata_is_validated_before_rendering() {
    for case in [
        "format",
        "policy",
        "missing",
        "action",
        "legacy_action",
        "identity",
    ] {
        let mut transcript = MessageTranscript::new();
        let id = append_model_reminder(&mut transcript);
        transcript.set_truncated(id, true);
        if case != "missing" {
            let mut directive = MessageProjectionDirective {
                policy_version: PROJECTION_POLICY_VERSION,
                ..Default::default()
            };
            match case {
                "format" => directive.format_version += 1,
                "policy" => directive.policy_version += 1,
                _ => {
                    if case == "legacy_action" {
                        directive.policy_version = 2;
                    }
                    directive.entries.push(ProjectionActionEntry {
                        message_id: if case == "identity" {
                            MessageId::new()
                        } else {
                            id
                        },
                        target: ProjectionTarget::Message,
                        action: ProjectionAction::CompactText { max_chars: 1 },
                    });
                }
            }
            transcript.set_flags_projection(id, directive);
        }
        assert!(
            matches!(
                super::projection::plan_from_persisted_directives(
                    &transcript,
                    PROJECTION_POLICY_VERSION
                ),
                PersistedDirectiveRestore::Invalid
            ),
            "invalid reminder metadata must fail closed: {case}"
        );
        assert!(
            super::projection::render_persisted_llm_view(
                &transcript,
                &ProviderCapabilities::default(),
            )
            .is_err(),
            "invalid reminder must never fall back to its full body: {case}"
        );
    }
}

#[test]
fn excluded_reminder_metadata_is_ignored_and_empty_visible_metadata_is_preserved() {
    let mut transcript = MessageTranscript::new();
    let hidden = append_model_reminder(&mut transcript);
    transcript.set_flags_projection(
        hidden,
        MessageProjectionDirective {
            format_version: 99,
            policy_version: 99,
            ..Default::default()
        },
    );
    transcript.set_truncated(hidden, true);
    transcript.set_excluded(hidden, true);
    let hidden_without_directive = append_model_reminder(&mut transcript);
    transcript.set_truncated(hidden_without_directive, true);
    transcript.set_excluded(hidden_without_directive, true);
    let visible = append_model_reminder(&mut transcript);
    let canonical = transcript.visible_model_messages().unwrap();
    transcript.set_flags_projection(
        visible,
        MessageProjectionDirective {
            policy_version: PROJECTION_POLICY_VERSION,
            ..Default::default()
        },
    );

    let PersistedDirectiveRestore::Valid(restored) =
        super::projection::plan_from_persisted_directives(&transcript, PROJECTION_POLICY_VERSION)
    else {
        panic!("valid empty reminder metadata must restore despite excluded corrupt history");
    };
    assert!(restored.actions.is_empty());
    let rendered =
        super::projection::render_persisted_llm_view(&transcript, &ProviderCapabilities::default())
            .unwrap();
    assert_eq!(
        serde_json::to_value(&rendered).unwrap(),
        serde_json::to_value(&canonical).unwrap()
    );
    assert_eq!(rendered.len(), 1);
    assert_eq!(rendered[0].id(), visible);
}
