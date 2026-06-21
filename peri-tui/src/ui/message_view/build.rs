//! 从 BaseMessage 到 MessageViewModel 的转换逻辑。

use peri_agent::messages::{BaseMessage, ContentBlock};
use ratatui::text::{Line, Text};

use super::tools::{parse_subagent_tool_count, tool_color};
use super::utils::{instance_hash, parse_bg_hash};
use super::{ContentBlockView, MessageViewModel};
use crate::ui::markdown::parse_markdown_default;
use crate::ui::theme;

/// 从工具名和入参构造预渲染的 diff 行（仅 Write/Edit 工具）
pub(crate) fn build_diff_lines(
    name: &str,
    input: &serde_json::Value,
) -> Option<Vec<Line<'static>>> {
    let diff_input = match name {
        "Edit" => {
            let old_string = input
                .get("old_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let new_string = input
                .get("new_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let file_path = input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if old_string.is_empty() || file_path.is_empty() {
                return None;
            }
            Some(peri_widgets::DiffInput {
                file_path: file_path.to_string(),
                old_content: old_string.to_string(),
                new_content: new_string.to_string(),
                is_new_file: false,
                is_deleted_file: false,
                is_binary: false,
            })
        }
        "Write" => {
            let content = input.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let file_path = input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if content.is_empty() || file_path.is_empty() {
                return None;
            }
            Some(peri_widgets::DiffInput {
                file_path: file_path.to_string(),
                old_content: String::new(),
                new_content: content.to_string(),
                is_new_file: true,
                is_deleted_file: false,
                is_binary: false,
            })
        }
        _ => None,
    }?;
    let lines = peri_widgets::diff::render_diff(&diff_input, 80, &peri_widgets::DarkTheme);
    if lines.is_empty() {
        None
    } else {
        Some(lines)
    }
}

impl MessageViewModel {
    /// 从 BaseMessage 转换为视图模型（向后兼容，cwd 为 None）
    ///
    /// `prev_ai_tool_calls` 用于为 Tool 消息提供工具名和参数（BaseMessage::Tool 只存储 tool_use_id）
    pub fn from_base_message(
        msg: &BaseMessage,
        prev_ai_tool_calls: &[(String, String, serde_json::Value)],
    ) -> Self {
        Self::from_base_message_with_cwd(msg, prev_ai_tool_calls, None)
    }

    /// 从 BaseMessage 转换为视图模型（带 cwd 上下文，统一管线入口）
    ///
    /// `cwd` 用于工具参数路径缩短，确保流式和恢复路径产生一致的显示。
    /// 这是统一管线的核心转换函数——`MessagePipeline::messages_to_view_models()` 调用此方法。
    pub fn from_base_message_with_cwd(
        msg: &BaseMessage,
        prev_ai_tool_calls: &[(String, String, serde_json::Value)],
        cwd: Option<&str>,
    ) -> Self {
        match msg {
            BaseMessage::Human { content, .. } => {
                let raw = content.text_content();
                // 精确识别 compact 摘要：<system-reminder> 标签 + CONTINUATION_HINT 续接指令。
                // 单独的 <system-reminder> 标签也用于 goal steering、tool_dispatch 连续失败警告、
                // hooks stop_hook_feedback 等中途纠正消息（CLAUDE.md TRAP），不应误识别为 compact。
                // CONTINUATION_HINT 与 peri-agent::agent::compact::CONTINUATION_HINT 共享——
                // /compact 命令路径（peri-acp invariant.rs）与 auto-compact 路径
                // （peri-middlewares compact_middleware.rs）都注入此标记。
                let hint = peri_agent::agent::compact::CONTINUATION_HINT;
                let (display_text, system_reminder) =
                    if raw.contains("<system-reminder>") && raw.contains(hint) {
                        let cleaned = raw
                            .replacen("<system-reminder>\n", "", 1)
                            .replacen("\n</system-reminder>", "", 1)
                            .trim()
                            .to_string();
                        (cleaned, true)
                    } else {
                        (raw, false)
                    };
                let rendered = parse_markdown_default(&display_text);
                let mut vm = MessageViewModel::UserBubble {
                    content: display_text,
                    rendered,
                    content_hash: 0,
                    system_reminder,
                };
                vm.recompute_hash();
                vm
            }
            BaseMessage::Ai {
                content,
                tool_calls,
                ..
            } => {
                // 先处理 content 中的 blocks
                let mut blocks: Vec<ContentBlockView> = content
                    .content_blocks()
                    .into_iter()
                    .map(|block| match block {
                        ContentBlock::Text { text } => {
                            let rendered = parse_markdown_default(&text);
                            let rendered_prefix_lines = rendered.lines.len();
                            ContentBlockView::Text {
                                raw: text.clone(),
                                rendered,
                                dirty: false,
                                rendered_prefix_len: text.len(),
                                rendered_prefix_lines,
                                holdback_scanner: Default::default(),
                            }
                        }
                        ContentBlock::Reasoning { text, .. } => ContentBlockView::Reasoning {
                            char_count: text.chars().count(),
                            text: text.clone(),
                            tail_lines: None,
                        },
                        ContentBlock::ToolUse { name, .. } => ContentBlockView::ToolUse { name },
                        ContentBlock::Image { .. } => ContentBlockView::Text {
                            raw: "[Image]".to_string(),
                            rendered: Text::raw("[Image]"),
                            dirty: false,
                            rendered_prefix_len: 7,
                            rendered_prefix_lines: 1,
                            holdback_scanner: Default::default(),
                        },
                        ContentBlock::Document { title, .. } => {
                            let label = title.as_deref().unwrap_or("Document");
                            let raw = format!("[Document: {}]", label);
                            let len = raw.len();
                            ContentBlockView::Text {
                                raw,
                                rendered: Text::raw(format!("[Document: {}]", label)),
                                dirty: false,
                                rendered_prefix_len: len,
                                rendered_prefix_lines: 1,
                                holdback_scanner: Default::default(),
                            }
                        }
                        ContentBlock::Unknown(v) => {
                            let type_name =
                                v.get("type").and_then(|t| t.as_str()).unwrap_or("unknown");
                            let raw = format!("[{}]", type_name);
                            let len = raw.len();
                            ContentBlockView::Text {
                                raw,
                                rendered: Text::raw(format!("[{}]", type_name)),
                                dirty: false,
                                rendered_prefix_len: len,
                                rendered_prefix_lines: 1,
                                holdback_scanner: Default::default(),
                            }
                        }
                        // ToolResult 在 Ai 消息中不常见，静默跳过
                        _ => ContentBlockView::Text {
                            raw: String::new(),
                            rendered: Text::raw(""),
                            dirty: false,
                            rendered_prefix_len: 0,
                            rendered_prefix_lines: 0,
                            holdback_scanner: Default::default(),
                        },
                    })
                    .collect();

                // 补充 tool_calls 字段中的工具调用（当 content 中没有对应的 ToolUse block 时）
                // 避免重复：如果 content_blocks 中已有同名 ToolUse，跳过
                let existing_tool_names: std::collections::HashSet<String> = blocks
                    .iter()
                    .filter_map(|b| {
                        if let ContentBlockView::ToolUse { name } = b {
                            Some(name.clone())
                        } else {
                            None
                        }
                    })
                    .collect();

                for tc in tool_calls {
                    if !existing_tool_names.contains(&tc.name) {
                        blocks.push(ContentBlockView::ToolUse {
                            name: tc.name.clone(),
                        });
                    }
                }

                let mut vm = MessageViewModel::AssistantBubble {
                    blocks,
                    is_streaming: false,
                    collapsed: false,
                    content_hash: 0,
                };
                vm.recompute_hash();
                vm
            }
            BaseMessage::Tool {
                tool_call_id,
                content,
                is_error,
                ..
            } => {
                // 从前一条 Ai 消息的 tool_calls 中查找工具名和参数
                let (tool_name, input) = prev_ai_tool_calls
                    .iter()
                    .find(|(id, _, _)| id == tool_call_id)
                    .map(|(_, name, input)| (name.clone(), input.clone()))
                    .unwrap_or_else(|| (tool_call_id.clone(), serde_json::Value::Null));
                let raw_content = content.text_content();
                // Agent 工具恢复为 SubAgentGroup（完成状态，展开显示 final_result）
                if tool_name == "Agent" {
                    let agent_id = input
                        .get("subagent_type")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .unwrap_or("fork")
                        .to_string();
                    let task_preview = input["prompt"]
                        .as_str()
                        .unwrap_or("")
                        .chars()
                        .take(40)
                        .collect::<String>();
                    // 检测是否为后台 agent（从 result 字符串检测 "Background task" 前缀）
                    let is_background = raw_content.starts_with("Background task");
                    // 解析 bg_hash（后台任务从 result 解析，前台从 tool_call_id 生成）
                    let bg_hash = if is_background {
                        parse_bg_hash(&raw_content)
                    } else {
                        Some(instance_hash(tool_call_id))
                    };
                    let mut vm = MessageViewModel::SubAgentGroup {
                        agent_id,
                        task_preview,
                        total_steps: parse_subagent_tool_count(&raw_content),
                        recent_messages: Vec::new(), // 子 agent 内部消息不持久化
                        is_running: false,
                        collapsed: false, // 展开显示 final_result
                        final_result: Some(raw_content),
                        is_error: *is_error,
                        is_background,
                        bg_hash,
                        batch_agents: Vec::new(),
                        instance_id: None,
                        content_hash: 0,
                    };
                    vm.recompute_hash();
                    return vm;
                }
                // 使用统一格式化函数生成 display_name 和 args_display
                // cwd 参数确保流式和恢复路径产生一致的路径显示
                let display_name = crate::app::tool_display::format_tool_name(&tool_name);
                let args_display =
                    crate::app::tool_display::format_tool_args(&tool_name, &input, cwd);
                let color = if *is_error {
                    theme::ERROR
                } else {
                    tool_color(&tool_name)
                };
                // diff_lines：从工具入参构造（仅成功的 Write/Edit）
                let diff_lines = if *is_error {
                    None
                } else {
                    build_diff_lines(&tool_name, &input)
                };
                // AgentResult / ExecuteExtraTool 结果自动展开（关键产出不折叠）
                let auto_expand =
                    crate::app::tool_display::should_auto_expand_tool(&tool_name, *is_error);
                let mut vm = MessageViewModel::ToolBlock {
                    tool_name,
                    tool_call_id: tool_call_id.clone(),
                    display_name,
                    args_display,
                    content: raw_content,
                    is_error: *is_error,
                    collapsed: !auto_expand,
                    color,
                    diff_lines,
                    content_hash: 0,
                };
                vm.recompute_hash();
                vm
            }
            BaseMessage::System { content, .. } => {
                let mut vm = MessageViewModel::SystemNote {
                    content: content.text_content(),
                    content_hash: 0,
                };
                vm.recompute_hash();
                vm
            }
        }
    }
}
