use super::projection::{
    render_llm_view, MicroCompactPlan, ProjectionAction, ProjectionActionEntry, ProjectionTarget,
    ProviderCapabilities, PROJECTION_POLICY_VERSION,
};
use crate::messages::{BaseMessage, ContentBlock, MessageContent};
use crate::session::transcript::MessageTranscript;
#[test]
fn test_renderer_preserves_tool_use_for_illegal_block_actions() {
    let first_input = serde_json::json!({"content": "A".repeat(600)});
    let second_input = serde_json::json!({"command": "printf safe"});
    let blocks = vec![
        ContentBlock::text("before"),
        ContentBlock::tool_use("tc_1", "Write", first_input.clone()),
        ContentBlock::text("between"),
        ContentBlock::tool_use("tc_2", "Bash", second_input.clone()),
        ContentBlock::text("after"),
    ];
    let mut transcript = MessageTranscript::new();
    let message = BaseMessage::ai_from_blocks(blocks.clone());
    let message_id = message.id();
    transcript.append(message);
    let plan = MicroCompactPlan {
        policy_version: PROJECTION_POLICY_VERSION,
        actions: vec![
            ProjectionActionEntry {
                message_id,
                target: ProjectionTarget::ContentBlock { index: 1 },
                action: ProjectionAction::Exclude,
            },
            ProjectionActionEntry {
                message_id,
                target: ProjectionTarget::ContentBlock { index: 3 },
                action: ProjectionAction::CompactText { max_chars: 1 },
            },
            ProjectionActionEntry {
                message_id,
                target: ProjectionTarget::ToolCall {
                    tool_call_id: "tc_1".into(),
                },
                action: ProjectionAction::CompactToolInput {
                    fields: vec!["content".into()],
                    keep_head: 1,
                    keep_tail: 1,
                },
            },
        ],
        ..Default::default()
    };

    let projected = render_llm_view(&transcript, &plan, &ProviderCapabilities::default())
        .expect("非法组合应防御性 no-op");
    let BaseMessage::Ai {
        content,
        tool_calls,
        ..
    } = &projected[0]
    else {
        panic!("应为 Ai 消息");
    };
    assert_eq!(content, &MessageContent::Blocks(blocks));
    assert_eq!(tool_calls.len(), 2);
    assert_eq!(tool_calls[0].id, "tc_1");
    assert_eq!(tool_calls[0].name, "Write");
    assert_eq!(tool_calls[0].arguments, first_input);
    assert_eq!(tool_calls[1].id, "tc_2");
    assert_eq!(tool_calls[1].name, "Bash");
    assert_eq!(tool_calls[1].arguments, second_input);
}
