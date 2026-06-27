use peri_agent::messages::BaseMessage;

use super::MessagePipeline;
use crate::{
    app::tool_display,
    ui::{
        markdown::parse_markdown_default,
        message_view::{aggregate_tool_groups, tool_color, ContentBlockView, MessageViewModel},
    },
};

impl MessagePipeline {
    /// 构建当前迭代 partial 的 AssistantBubble ViewModel。
    ///
    /// 包含 Reasoning block + 已输出的文本 + 已完成的 tool_use blocks。
    /// 不包含 pending tools——它们在 build_tail_vms 中另行处理。
    pub fn build_streaming_bubble(&self) -> MessageViewModel {
        let mut blocks: Vec<ContentBlockView> = Vec::new();
        if let Some(partial) = self.partial.as_ref() {
            if !partial.reasoning.is_empty() {
                blocks.push(ContentBlockView::Reasoning {
                    char_count: partial.reasoning.chars().count(),
                    text: partial.reasoning.clone(),
                    tail_lines: None,
                });
            }
            if !partial.text.trim().is_empty() {
                let rendered = parse_markdown_default(&partial.text);
                let rendered_prefix_lines = rendered.lines.len();
                let mut scanner = crate::ui::markdown::TableHoldbackScanner::new();
                scanner.set_streaming(true);
                blocks.push(ContentBlockView::Text {
                    raw: partial.text.clone(),
                    rendered,
                    dirty: false,
                    rendered_prefix_len: partial.text.len(),
                    rendered_prefix_lines,
                    holdback_scanner: scanner,
                });
            }
            for tc in &partial.tool_calls {
                if !partial.pending_tools.contains_key(&tc.id) {
                    blocks.push(ContentBlockView::ToolUse {
                        name: tc.name.clone(),
                    });
                }
            }
        }
        let mut vm = MessageViewModel::AssistantBubble {
            blocks,
            is_streaming: true,
            collapsed: false,
            content_hash: 0,
        };
        vm.recompute_hash();
        vm
    }

    /// 从规范 BaseMessage[] 构建完整的 MessageViewModel[]。
    ///
    /// **这是唯一的转换入口**——流式 reconcile 和历史恢复都调用此函数。
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
            let vm =
                MessageViewModel::from_base_message_with_cwd(msg, &prev_ai_tool_calls, Some(cwd));
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

    /// Reconcile：从当前 transcript 状态重建完整的 view_models。
    ///
    /// 在 "finalize 边界"（ToolStart / Done）调用，确保流式最终状态
    /// 与 restore 路径 `messages_to_view_models()` 完全一致。
    pub fn reconcile(&self) -> Vec<MessageViewModel> {
        Self::messages_to_view_models(&self.transcript, &self.cwd)
    }

    /// Finalize 当前迭代的 partial（标记 finalized）。
    /// 真正写入 transcript 由 `commit_iteration` 在 TurnCommitted 到达时完成。
    pub(crate) fn finalize_current_ai(&mut self) {
        if let Some(partial) = self.partial.as_mut() {
            if partial.has_any_content() {
                partial.finalize();
            }
        }
    }

    /// 构建 ToolStart 的 ToolBlock VM（与 from_base_message_with_cwd 的 Tool 路径一致）
    pub(crate) fn build_tool_start_vm(
        &self,
        tool_call_id: &str,
        name: &str,
        input: &serde_json::Value,
    ) -> MessageViewModel {
        let display_name = tool_display::format_tool_name(name);
        let args_display = tool_display::format_tool_args(name, input, Some(&self.cwd));
        let mut vm = MessageViewModel::ToolBlock {
            tool_name: name.to_string(),
            tool_call_id: tool_call_id.to_string(),
            display_name,
            args_display,
            content: String::new(),
            is_error: false,
            collapsed: true,
            color: tool_color(name),
            diff_lines: None,
            content_hash: 0,
        };
        vm.recompute_hash();
        vm
    }
}
