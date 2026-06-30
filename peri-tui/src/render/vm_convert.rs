//! v1 MessageViewModel → v2 ViewModel 转换。
//!
//! ## 背景
//!
//! v2 ViewCommit 中 `SubAgentGroupData.view_models` 永久为空（ACP 层
//! `view_mapper::convert_agent_tool` 生成 placeholder）。子 Agent 的真实
//! 内容由 TUI 的 v1 `view_messages` 维护（`MessageViewModel::SubAgentGroup
//! { recent_messages, .. }`）。
//!
//! 本模块把 v1 子内容转换为 v2 `ViewModel`，供 `render_subagent_group`
//! 通过 thread-local status probe 注入显示。这是 Phase 2.6（删除
//! `view_messages`）的 prerequisite —— 完整切换前用此桥接保证 UX。
//!
//! ## 设计权衡
//!
//! - `UserBubble` / `SystemNote` / `CacheWarning`：1:1 字段映射，无信息丢失
//! - `AssistantBubble`：从 `blocks: Vec<ContentBlockView>` 提取 Text +
//!   Reasoning（丢弃 `rendered` Text 缓存，v2 重新解析 markdown）
//! - `ToolBlock`：`tool_call_id` 作为 `tool_id`，`display_name` 拼接
//!   `args_display` 作为 `input_summary`，`content` 作为 `output_summary`
//! - `ToolCallGroup`：展开为多个 `ToolCard`（保留每个工具的可见性）
//! - `SubAgentGroup`：**不嵌套**（避免递归爆炸）；返回 None，外层显示
//!   final_result 摘要即可

use peri_acp_types::view_model::{
    AssistantBubbleData, NoteLevel, ReasoningBlock, SystemNoteData, ToolCardData, UserBubbleData,
    ViewModel,
};

use crate::ui::message_view::{ContentBlockView, MessageViewModel};

/// 转换单个 v1 `MessageViewModel` 为 v2 `ViewModel`。
///
/// 返回 `None` 表示该变体在 v2 中无对应（`SubAgentGroup` 不嵌套）。
pub fn message_view_model_to_v2(vm: &MessageViewModel) -> Option<ViewModel> {
    match vm {
        MessageViewModel::UserBubble { content, .. } => {
            Some(ViewModel::UserBubble(UserBubbleData {
                text: content.clone(),
            }))
        }
        MessageViewModel::AssistantBubble { blocks, .. } => {
            let mut text_parts: Vec<String> = Vec::new();
            let mut reasoning_parts: Vec<String> = Vec::new();
            for block in blocks {
                match block {
                    ContentBlockView::Text { raw, .. } if !raw.is_empty() => {
                        text_parts.push(raw.clone());
                    }
                    ContentBlockView::Reasoning { text, .. } if !text.is_empty() => {
                        reasoning_parts.push(text.clone());
                    }
                    _ => {} // ToolUse 在 v2 中是 sibling ToolCard，不是子内容
                }
            }
            // 如果没有 text 也没有 reasoning，跳过（避免空 bubble）
            if text_parts.is_empty() && reasoning_parts.is_empty() {
                return None;
            }
            Some(ViewModel::AssistantBubble(AssistantBubbleData {
                text: text_parts.join("\n\n"),
                reasoning: if reasoning_parts.is_empty() {
                    None
                } else {
                    Some(ReasoningBlock {
                        text: reasoning_parts.join("\n\n"),
                        collapsed: true,
                    })
                },
                tool_card_ids: Vec::new(),
            }))
        }
        MessageViewModel::ToolBlock {
            tool_call_id,
            display_name,
            args_display,
            content,
            is_error,
            ..
        } => Some(ViewModel::ToolCard(ToolCardData {
            tool_id: tool_call_id.clone(),
            tool_name: display_name.clone(),
            input_summary: args_display.clone().unwrap_or_default(),
            output_summary: content.clone(),
            is_error: *is_error,
            diff: None,
        })),
        MessageViewModel::SystemNote { content, .. } => {
            Some(ViewModel::SystemNote(SystemNoteData {
                text: content.clone(),
                level: NoteLevel::Info,
            }))
        }
        MessageViewModel::CacheWarning { content, .. } => {
            Some(ViewModel::SystemNote(SystemNoteData {
                text: content.clone(),
                level: NoteLevel::Warning,
            }))
        }
        MessageViewModel::ToolCallGroup { tools, .. } => {
            // ToolCallGroup 展开为首个 ToolCard（保留最关键信息），
            // 其余工具作为后续 ToolCard。返回首个，调用方负责展开整组。
            // 实际上这里返回 None，由 `message_view_models_to_v2` 中的
            // 扁平化逻辑处理（一个 ToolCallGroup → 多个 ToolCard）。
            let _ = tools;
            None
        }
        MessageViewModel::SubAgentGroup { .. } => {
            // 不嵌套（避免递归爆炸）。外层 SubAgentGroup 的 final_result
            // 摘要已足够表达子 Agent 结果。
            None
        }
    }
}

/// 批量转换 v1 → v2，扁平化 `ToolCallGroup`。
pub fn message_view_models_to_v2(vms: &[MessageViewModel]) -> Vec<ViewModel> {
    let mut out = Vec::with_capacity(vms.len());
    for vm in vms {
        match vm {
            MessageViewModel::ToolCallGroup { tools, .. } => {
                for entry in tools {
                    out.push(ViewModel::ToolCard(ToolCardData {
                        tool_id: entry.tool_name.clone(),
                        tool_name: entry.display_name.clone(),
                        input_summary: entry.args_display.clone().unwrap_or_default(),
                        output_summary: entry.content.clone(),
                        is_error: entry.is_error,
                        diff: None,
                    }));
                }
            }
            other => {
                if let Some(v2) = message_view_model_to_v2(other) {
                    out.push(v2);
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::message_view::ToolEntry;

    fn user_bubble(text: &str) -> MessageViewModel {
        MessageViewModel::user(text.to_string())
    }

    #[test]
    fn test_convert_user_bubble() {
        let vm = user_bubble("hello");
        let v2 = message_view_model_to_v2(&vm).expect("UserBubble 应转换");
        match v2 {
            ViewModel::UserBubble(d) => assert_eq!(d.text, "hello"),
            _ => panic!("expected UserBubble"),
        }
    }

    #[test]
    fn test_convert_system_note() {
        let vm = MessageViewModel::system("note".to_string());
        let v2 = message_view_model_to_v2(&vm).expect("SystemNote 应转换");
        match v2 {
            ViewModel::SystemNote(d) => {
                assert_eq!(d.text, "note");
                assert!(matches!(d.level, NoteLevel::Info));
            }
            _ => panic!("expected SystemNote"),
        }
    }

    #[test]
    fn test_convert_subagent_group_returns_none() {
        // SubAgentGroup 不嵌套
        let vm = MessageViewModel::SubAgentGroup {
            agent_id: "fork".into(),
            instance_id: None,
            task_preview: "task".into(),
            is_running: false,
            is_background: false,
            total_steps: 0,
            recent_messages: Vec::new(),
            collapsed: false,
            bg_hash: None,
            final_result: None,
            is_error: false,
            batch_agents: Vec::new(),
            content_hash: 0,
        };
        assert!(message_view_model_to_v2(&vm).is_none());
    }

    #[test]
    fn test_convert_batch_with_tool_call_group_flattens() {
        // ToolCallGroup 应展开为多个 ToolCard
        let group = MessageViewModel::ToolCallGroup {
            category: crate::ui::message_view::ToolCategory::Read,
            tools: vec![
                ToolEntry {
                    tool_name: "Read".into(),
                    display_name: "Read".into(),
                    args_display: Some("file1.rs".into()),
                    content: "content1".into(),
                    is_error: false,
                },
                ToolEntry {
                    tool_name: "Read".into(),
                    display_name: "Read".into(),
                    args_display: Some("file2.rs".into()),
                    content: "content2".into(),
                    is_error: false,
                },
            ],
            collapsed: false,
            content_hash: 0,
        };
        let vms = vec![group];
        let v2 = message_view_models_to_v2(&vms);
        assert_eq!(v2.len(), 2, "ToolCallGroup 应展开为 2 个 ToolCard");
        for vm in &v2 {
            assert!(matches!(vm, ViewModel::ToolCard(_)));
        }
    }

    #[test]
    fn test_convert_batch_preserves_order() {
        let vms = vec![
            user_bubble("first"),
            MessageViewModel::system("mid".to_string()),
            user_bubble("last"),
        ];
        let v2 = message_view_models_to_v2(&vms);
        assert_eq!(v2.len(), 3);
        assert!(matches!(v2[0], ViewModel::UserBubble(_)));
        assert!(matches!(v2[1], ViewModel::SystemNote(_)));
        assert!(matches!(v2[2], ViewModel::UserBubble(_)));
    }

    #[test]
    fn test_convert_empty_assistant_bubble_returns_none() {
        // 没有 text 也没有 reasoning → None（避免空 bubble）
        let vm = MessageViewModel::AssistantBubble {
            blocks: Vec::new(),
            is_streaming: false,
            collapsed: false,
            content_hash: 0,
        };
        assert!(message_view_model_to_v2(&vm).is_none());
    }
}
