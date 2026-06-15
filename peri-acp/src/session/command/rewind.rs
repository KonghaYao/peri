//! `/rewind` 命令 — 回滚对话到指定消息。
//!
//! 接收 JSON 参数（`target_message_id` + `revert_files`），定位目标消息后：
//! 1. 截断 history 到目标消息之前
//! 2. 从被移除的消息中提取 Write/Edit 工具调用，逆向恢复文件
//! 3. 验证保留消息的 ToolUse/ToolResult 配对完整性
//! 4. 从 SQLite 持久化中删除被移除的消息
//! 5. 发送 CompactCompleted 事件通知 TUI 刷新

use std::path::Path;

use peri_agent::agent::events::AgentEvent as ExecutorEvent;
use peri_agent::messages::{BaseMessage, ContentBlock, MessageId};
use tracing::{debug, warn};

use super::{AgentCommand, CommandContext, CommandKind, CommandResult};
use crate::session::executor::PromptStopReason;

/// 回滚命令。
pub struct RewindCommand;

impl RewindCommand {
    pub const NAME: &'static str = "rewind";
}

#[derive(serde::Deserialize)]
struct RewindArgs {
    target_message_id: String,
    revert_files: bool,
}

/// 提取到的文件变更操作。
enum FileChange {
    Write {
        path: String,
        #[allow(dead_code)]
        content: String,
    },
    Edit {
        path: String,
        old_string: String,
        new_string: String,
    },
}

#[async_trait::async_trait]
impl AgentCommand for RewindCommand {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn aliases(&self) -> Vec<&str> {
        vec!["undo"]
    }

    fn description(&self) -> &str {
        "回滚对话到指定消息"
    }

    fn kind(&self) -> CommandKind {
        CommandKind::Immediate
    }

    async fn execute(&self, ctx: CommandContext) -> CommandResult {
        let history = &ctx.history;

        // Step 1: 解析参数
        let args = match serde_json::from_str::<RewindArgs>(ctx.args.trim()) {
            Ok(a) => a,
            Err(e) => {
                let msg = format!("rewind 参数解析失败: {e}");
                warn!(msg);
                ctx.event_sink
                    .push_event(
                        &ctx.session_id,
                        &ExecutorEvent::CompactError { message: msg },
                        0,
                    )
                    .await;
                return CommandResult {
                    messages: ctx.history,
                    stop_reason: PromptStopReason::EndTurn,
                };
            }
        };

        // Step 2: 定位目标消息
        let target_idx = history
            .iter()
            .position(|m| m.id().as_uuid().to_string() == args.target_message_id);

        let target_idx = match target_idx {
            Some(i) => i,
            None => {
                let msg = format!("rewind: 未找到目标消息 {}", args.target_message_id);
                warn!(msg);
                ctx.event_sink
                    .push_event(
                        &ctx.session_id,
                        &ExecutorEvent::CompactError { message: msg },
                        0,
                    )
                    .await;
                return CommandResult {
                    messages: ctx.history,
                    stop_reason: PromptStopReason::EndTurn,
                };
            }
        };

        // 截断：保留目标消息之前的所有消息（不含目标本身）
        // 目标用户消息及其之后的所有消息（AI 回复、工具调用、后续交互）全部移除
        let removed_messages = &history[target_idx..];
        let retained_messages = history[..target_idx].to_vec();

        let removed_count = removed_messages.len();

        // Step 3: 提取文件变更并逆向恢复
        let mut revert_warnings = Vec::new();
        if args.revert_files {
            let changes = extract_file_changes(removed_messages);
            revert_files(&changes, &ctx.cwd, &mut revert_warnings);
        }

        // Step 4: 验证 ToolUse/ToolResult 配对完整性
        validate_tool_pairing(&retained_messages);

        // Step 5: 从持久化中删除被移除的消息
        let removed_ids: Vec<MessageId> = removed_messages.iter().map(|m| m.id()).collect();
        if let (Some(store), Some(tid)) = (&ctx.thread_store, &ctx.thread_id) {
            if !removed_ids.is_empty() {
                match store.delete_messages(tid, &removed_ids).await {
                    Ok(()) => debug!(count = removed_ids.len(), "rewind: 持久化消息已删除"),
                    Err(e) => {
                        let msg = format!("rewind: 持久化删除失败: {e}");
                        warn!(msg);
                        revert_warnings.push(msg);
                    }
                }
            }
        }

        // Step 6: 发送 RewindCompleted 事件
        let mut summary = format!("已回滚 {removed_count} 条消息");
        if !revert_warnings.is_empty() {
            summary.push_str(&format!("（警告: {}）", revert_warnings.join("; ")));
        }
        ctx.event_sink
            .push_event(
                &ctx.session_id,
                &ExecutorEvent::RewindCompleted {
                    summary,
                    messages: retained_messages.clone(),
                },
                0,
            )
            .await;

        CommandResult {
            messages: retained_messages,
            stop_reason: PromptStopReason::EndTurn,
        }
    }
}

/// 从被移除的消息中提取所有 Write/Edit 工具调用。
fn extract_file_changes(messages: &[BaseMessage]) -> Vec<FileChange> {
    let mut changes = Vec::new();
    for msg in messages {
        if let BaseMessage::Ai {
            content,
            tool_calls,
            ..
        } = msg
        {
            // OpenAI 格式: tool_calls 字段
            for tc in tool_calls {
                if tc.name == "Write" || tc.name == "Edit" {
                    if let Some(change) = parse_tool_call(&tc.name, &tc.arguments) {
                        changes.push(change);
                    }
                }
            }

            // Anthropic 格式: ContentBlock::ToolUse（content_blocks 返回 owned Vec）
            for block in content.content_blocks() {
                if let ContentBlock::ToolUse {
                    ref name,
                    ref input,
                    ..
                } = block
                {
                    if name == "Write" || name == "Edit" {
                        if let Some(change) = parse_tool_call(name, input) {
                            changes.push(change);
                        }
                    }
                }
            }
        }
    }
    changes
}

/// 从工具调用参数中解析文件变更。
fn parse_tool_call(name: &str, args: &serde_json::Value) -> Option<FileChange> {
    let path = args.get("file_path")?.as_str()?.to_string();
    match name {
        "Write" => {
            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(FileChange::Write { path, content })
        }
        "Edit" => {
            let old_string = args
                .get("old_string")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let new_string = args
                .get("new_string")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(FileChange::Edit {
                path,
                old_string,
                new_string,
            })
        }
        _ => None,
    }
}

/// 逆向恢复文件变更（最佳努力，失败仅记录警告）。
fn revert_files(changes: &[FileChange], cwd: &str, warnings: &mut Vec<String>) {
    // 逆序遍历，先恢复最近的变更
    for change in changes.iter().rev() {
        match change {
            FileChange::Edit {
                path,
                old_string,
                new_string,
            } => {
                let full_path = Path::new(cwd).join(path);
                match std::fs::read_to_string(&full_path) {
                    Ok(content) => {
                        // 只替换第一次出现（与 Edit 工具行为一致）
                        // 字符级操作：用 replacen 避免字节切片的 UTF-8 边界 panic。
                        // new_string 可能跨多字节 CJK 字符，content.find() 返回字节索引，
                        // &content[..idx] 虽在 char boundary 上技术上安全，但 replacen
                        // 是更稳健且与 filesystem/edit.rs 同构的写法。
                        if content.contains(new_string) {
                            let reverted = content.replacen(new_string, old_string, 1);
                            if let Err(e) = std::fs::write(&full_path, reverted) {
                                warnings.push(format!("Edit 恢复写入失败 {path}: {e}"));
                            }
                        } else {
                            warnings.push(format!("Edit 恢复跳过 {path}: 未找到 new_string"));
                        }
                    }
                    Err(e) => {
                        warnings.push(format!("Edit 恢复读取失败 {path}: {e}"));
                    }
                }
            }
            FileChange::Write { path, .. } => {
                let full_path = Path::new(cwd).join(path);
                // 删除文件
                if let Err(e) = std::fs::remove_file(&full_path) {
                    // 文件可能已不存在，仅 debug
                    debug!("Write 恢复删除文件失败 {path}: {e}");
                }
                // 尝试 git restore 恢复原始版本
                let result = std::process::Command::new("git")
                    .args(["checkout", "HEAD", "--"])
                    .arg(&full_path)
                    .current_dir(cwd)
                    .output();
                match result {
                    Ok(output) if output.status.success() => {
                        debug!("Write 恢复 git checkout 成功: {path}");
                    }
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        // git checkout 失败可能是文件不在 git 中（新文件被删除即可），仅 debug
                        debug!("Write 恢复 git checkout 失败 {path}: {stderr}");
                    }
                    Err(e) => {
                        debug!("Write 恢复 git 执行失败 {path}: {e}");
                    }
                }
            }
        }
    }
}

/// 验证保留消息中 ToolUse/ToolResult 的配对完整性。
fn validate_tool_pairing(messages: &[BaseMessage]) {
    let mut tool_use_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut tool_result_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    for msg in messages {
        match msg {
            BaseMessage::Ai {
                tool_calls,
                content,
                ..
            } => {
                // OpenAI 格式
                for tc in tool_calls {
                    tool_use_ids.insert(tc.id.clone());
                }
                // Anthropic 格式
                for block in content.content_blocks() {
                    if let ContentBlock::ToolUse { id, .. } = block {
                        tool_use_ids.insert(id.clone());
                    }
                }
            }
            BaseMessage::Tool { tool_call_id, .. } => {
                tool_result_ids.insert(tool_call_id.clone());
            }
            _ => {}
        }
    }

    // 检查未配对的 tool_use
    for id in &tool_use_ids {
        if !tool_result_ids.contains(id) {
            warn!(tool_use_id = %id, "rewind: 保留消息中存在未配对的 tool_use");
        }
    }
    // 检查未配对的 tool_result
    for id in &tool_result_ids {
        if !tool_use_ids.contains(id) {
            warn!(tool_call_id = %id, "rewind: 保留消息中存在未配对的 tool_result");
        }
    }
}

#[cfg(test)]
#[path = "rewind_test.rs"]
mod tests;
