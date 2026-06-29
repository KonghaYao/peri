use peri_agent::messages::BaseMessage;

use crate::ui::message_view::{aggregate_tool_groups, ContentBlockView, MessageViewModel};

/// 从规范 BaseMessage[] 构建完整的 MessageViewModel[]。
///
/// **这是唯一的转换入口**——流式渲染和历史恢复都调用此函数。
/// 从已删除的 message_pipeline/transform.rs 提取为独立模块。
pub fn messages_to_view_models(msgs: &[BaseMessage], cwd: &str) -> Vec<MessageViewModel> {
    let mut vms: Vec<MessageViewModel> = Vec::with_capacity(msgs.len());
    let mut prev_ai_tool_calls: Vec<(String, String, serde_json::Value)> = Vec::new();

    for msg in msgs {
        // System 消息（system prompt / compact summary）是内部状态，不应渲染
        if matches!(msg, BaseMessage::System { .. }) {
            continue;
        }

        if let BaseMessage::Ai { tool_calls, .. } = msg {
            prev_ai_tool_calls = tool_calls
                .iter()
                .map(|tc| (tc.id.clone(), tc.name.clone(), tc.arguments.clone()))
                .collect();
        }
        let vm = MessageViewModel::from_base_message_with_cwd(msg, &prev_ai_tool_calls, Some(cwd));
        if let MessageViewModel::AssistantBubble { ref blocks, .. } = &vm {
            let has_visible = blocks.iter().any(|b| match b {
                ContentBlockView::Text { raw, .. } => !raw.trim().is_empty(),
                ContentBlockView::Reasoning { char_count, .. } => *char_count > 0,
                ContentBlockView::ToolUse { .. } => false,
            });
            if !has_visible {
                continue;
            }
        }

        vms.push(vm);
    }

    aggregate_tool_groups(&mut vms);
    vms
}
