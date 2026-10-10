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
        },
    );
    transcript.set_excluded(excluded, true);
    let visible = transcript.append(BaseMessage::human("visible message"));

    assert!(matches!(
        super::projection::plan_from_persisted_directives(&transcript, PROJECTION_POLICY_VERSION),
        PersistedDirectiveRestore::Absent
    ));
    let projected = render_llm_view(
        &transcript,
        &super::projection::MicroCompactPlan::default(),
        &ProviderCapabilities::default(),
    )
    .unwrap();
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
            policy_version: PROJECTION_POLICY_VERSION,
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
        },
    );
    transcript.set_flags_projection(
        result_id,
        MessageProjectionDirective {
            policy_version: PROJECTION_POLICY_VERSION,
            entries: vec![ProjectionActionEntry {
                message_id: result_id,
                target: ProjectionTarget::Message,
                action: ProjectionAction::CompactToolResult {
                    keep_head: 20,
                    keep_tail: 20,
                    preserve_recovery_handle: false,
                },
            }],
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

    let projected = render_llm_view(&transcript, &plan, &ProviderCapabilities::default())
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
        },
    );

    let PersistedDirectiveRestore::Valid(plan) =
        super::projection::plan_from_persisted_directives(&transcript, PROJECTION_POLICY_VERSION)
    else {
        panic!("应恢复 ToolResult action");
    };
    let projected = render_llm_view(&transcript, &plan, &ProviderCapabilities::default())
        .expect("render_llm_view 应成功");
    assert!(projected[1].content().contains("字符已省略"));
}
