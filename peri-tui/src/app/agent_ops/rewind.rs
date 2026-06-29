//! Rewind 弹窗操作：打开、确认、取消。
//!
//! 用户双击 ESC 触发 rewind 选择器，可选择回退到历史中的某个用户消息节点。
//! 回退通过向 ACP server 发送 `/rewind` 命令实现。

use peri_agent::messages::{BaseMessage, ContentBlock}; // P4b: type-dependency

use super::*;
use crate::app::{rewind_prompt::RewindMode, InteractionPrompt};

impl App {
    /// 打开 rewind 选择器弹窗。
    ///
    /// 从 `origin_messages` 中提取所有 Human 消息作为可回退节点，
    /// 解析每个节点之后的文件变更信息用于确认弹窗展示。
    pub(crate) fn open_rewind_prompt(&mut self) {
        let origin_messages = &self.session_mgr.current().agent.origin_messages;

        tracing::debug!(
            origin_messages_len = origin_messages.len(),
            "open_rewind_prompt: checking origin_messages"
        );

        if origin_messages.is_empty() {
            self.push_system_note("没有可回退的对话轮次".to_string());
            return;
        }

        let mut items: Vec<crate::app::rewind_prompt::RewindItem> = Vec::new();
        let human_indices: Vec<usize> = origin_messages
            .iter()
            .enumerate()
            .filter(|(_, m)| matches!(m, BaseMessage::Human { .. }))
            .map(|(i, _)| i)
            .collect();

        for &human_idx in &human_indices {
            let msg = &origin_messages[human_idx];
            let text = msg.content();
            let summary: String = text.chars().take(60).collect();
            // 该消息及其之后的消息数量
            let count_after = origin_messages.len() - human_idx;
            // 文件变更从该消息开始提取（含目标消息之后的 AI 回复中的工具调用）
            let file_changes = extract_file_changes_from_messages(&origin_messages[human_idx..]);

            items.push(crate::app::rewind_prompt::RewindItem {
                message_id: msg.id().as_uuid().to_string(),
                summary,
                message_count_after: count_after,
                file_changes,
            });
        }

        if items.is_empty() {
            self.push_system_note("没有可回退的对话轮次".to_string());
            return;
        }

        // 默认选中最后一个（最近的）用户消息
        let cursor = items.len().saturating_sub(1);

        self.session_mgr.current_mut().agent.interaction_prompt = Some(InteractionPrompt::Rewind(
            crate::app::rewind_prompt::RewindPrompt {
                items,
                cursor,
                mode: RewindMode::MessagesOnly,
            },
        ));
    }

    /// 关闭 rewind 弹窗。
    pub(crate) fn cancel_rewind(&mut self) {
        self.session_mgr.current_mut().agent.interaction_prompt = None;
    }

    /// Rewind 弹窗光标上移。
    pub(crate) fn rewind_cursor_up(&mut self) {
        if let Some(InteractionPrompt::Rewind(prompt)) =
            &mut self.session_mgr.current_mut().agent.interaction_prompt
        {
            if prompt.cursor > 0 {
                prompt.cursor -= 1;
            }
        }
    }

    /// Rewind 弹窗光标下移。
    pub(crate) fn rewind_cursor_down(&mut self) {
        if let Some(InteractionPrompt::Rewind(prompt)) =
            &mut self.session_mgr.current_mut().agent.interaction_prompt
        {
            if prompt.cursor < prompt.items.len().saturating_sub(1) {
                prompt.cursor += 1;
            }
        }
    }

    /// Tab 切换回退模式（MessagesOnly ↔ MessagesAndFiles）。
    pub(crate) fn rewind_toggle_files(&mut self) {
        if let Some(InteractionPrompt::Rewind(prompt)) =
            &mut self.session_mgr.current_mut().agent.interaction_prompt
        {
            prompt.mode = match prompt.mode {
                RewindMode::MessagesOnly => RewindMode::MessagesAndFiles,
                RewindMode::MessagesAndFiles | RewindMode::ConfirmRevert => {
                    RewindMode::MessagesOnly
                }
            };
        }
    }

    /// 确认 rewind 操作。
    ///
    /// MessagesOnly 模式直接执行；MessagesAndFiles 模式进入二次确认（ConfirmRevert）；
    /// ConfirmRevert 模式执行实际回退。
    pub(crate) fn rewind_confirm(&mut self) {
        let (target_id, revert_files, go, rewound_text) = {
            let session = self.session_mgr.current();
            if let Some(InteractionPrompt::Rewind(prompt)) = &session.agent.interaction_prompt {
                let item = &prompt.items[prompt.cursor];
                // 从 origin_messages 查找目标消息的完整文本（此时 origin_messages 尚未被 rewind 修改）
                let full_text = session
                    .agent
                    .origin_messages
                    .iter()
                    .find(|m| m.id().as_uuid().to_string() == item.message_id)
                    .map(|m| m.content().to_string());
                match prompt.mode {
                    RewindMode::MessagesAndFiles => {
                        (item.message_id.clone(), true, false, full_text)
                    }
                    RewindMode::ConfirmRevert => (item.message_id.clone(), true, true, full_text),
                    RewindMode::MessagesOnly => (item.message_id.clone(), false, true, full_text),
                }
            } else {
                return;
            }
        };

        if !go {
            // MessagesAndFiles → 进入二次确认
            if let Some(InteractionPrompt::Rewind(prompt)) =
                &mut self.session_mgr.current_mut().agent.interaction_prompt
            {
                prompt.mode = RewindMode::ConfirmRevert;
            }
            return;
        }

        // 关闭弹窗
        self.session_mgr.current_mut().agent.interaction_prompt = None;

        // 暂存被撤回消息的文本，待 handle_rewind_completed 回填到输入框
        self.session_mgr.current_mut().ui.pending_rewind_text = rewound_text;

        // 构造 /rewind 命令并发送（复用 submit_message 的完整提交流程）
        let args = serde_json::json!({
            "target_message_id": target_id,
            "revert_files": revert_files,
        })
        .to_string();

        let command_text = format!("/rewind {}", args);
        self.submit_message(command_text);
    }
}

/// 从消息段中提取文件变更信息（按路径去重，保留最后一次操作）。
fn extract_file_changes_from_messages(
    messages: &[BaseMessage],
) -> Vec<crate::app::rewind_prompt::FileChangeInfo> {
    let mut changes = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();

    for msg in messages {
        // 从 Ai 消息的 tool_calls 提取
        if let BaseMessage::Ai { tool_calls, .. } = msg {
            for tc in tool_calls {
                if tc.name == "Write" || tc.name == "Edit" {
                    if let Some(path) = tc.arguments.get("file_path").and_then(|v| v.as_str()) {
                        if seen_paths.insert(path.to_string()) {
                            changes.push(crate::app::rewind_prompt::FileChangeInfo {
                                path: path.to_string(),
                                operation: tc.name.clone(),
                            });
                        }
                    }
                }
            }
        }

        // 从 content blocks 中的 ToolUse 提取（部分 provider 不填充 tool_calls）
        if msg.tool_calls().is_empty() {
            for block in msg.content_blocks() {
                if let ContentBlock::ToolUse { name, input, .. } = block {
                    if name == "Write" || name == "Edit" {
                        if let Some(path) = input.get("file_path").and_then(|v| v.as_str()) {
                            if seen_paths.insert(path.to_string()) {
                                changes.push(crate::app::rewind_prompt::FileChangeInfo {
                                    path: path.to_string(),
                                    operation: name.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    changes
}
