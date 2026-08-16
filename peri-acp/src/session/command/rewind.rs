//! `/rewind` 命令 — 回滚对话到指定消息。
//!
//! 参数形态（Phase 5 Step 5 由 serde_json 迁入 ArgsSchema）：
//! `/rewind <target_message_id> [--no-revert-files]`；`--no-revert-files`
//! 缺省时 revert_files=true（现状 serde default_true 语义保持）。
//! 定位目标消息后：
//! 1. 截断 history 到目标消息之前
//! 2. 从被移除的消息中提取 Write/Edit 工具调用，逆向恢复文件
//! 3. 验证保留消息的 ToolUse/ToolResult 配对完整性
//! 4. 从 SQLite 持久化中删除被移除的消息
//! 5. 发送 RewindCompleted 事件通知 TUI 刷新（重建信号，保留原样）
//!
//! RPC 路径（`dispatch/rewind.rs`）复用本模块的 [`execute_rewind`] 共享执行体，
//! RPC wire（serde RewindArgs + preview_fingerprint）保持不变。

mod events;

use std::path::Path;

use peri_acp_types::command::{ArgKind, ArgSpec, ArgsSchema, FlagSpec};
use peri_acp_types::messages::{BaseMessage, ContentBlock, MessageId};
use tracing::{debug, warn};

use super::{CommandContext, CommandFeedback, CommandResult, FeedbackChannel, FeedbackLevel};
use crate::session::executor::PromptStopReason;

/// 回滚命令。
#[derive(Default)]
pub struct RewindCommand;

impl RewindCommand {
    pub const NAME: &'static str = "rewind";
    /// 别名（注册条目挂载，命令声明单一事实源；旧 AgentCommand impl 已删）。
    pub const ALIASES: &'static [&'static str] = &["undo"];
    /// 描述（注册条目挂载）。
    pub const DESCRIPTION: &'static str = "回滚对话到指定消息";

    /// 参数 schema（Phase 5 Step 5：slash 形态 `/rewind <target_message_id>
    /// [--no-revert-files]`）。与 Phase 1 FlagSpec 形态一致（P1-1 延伸）；
    /// 现状 serde default_true 语义 = 无该 flag 时 revert_files=true
    /// （原 `ArgFlag { name: "revert-files", default: true }` 形态与语义自相矛盾，
    /// 以 `--no-revert-files` 为准）。
    pub fn args_schema() -> ArgsSchema {
        ArgsSchema {
            positionals: vec![ArgSpec {
                name: "target_message_id".to_string(),
                kind: ArgKind::String,
                required: true,
                description: None,
            }],
            flags: vec![FlagSpec {
                name: "no-revert-files".to_string(),
                short: None,
                description: None,
            }],
            named: vec![],
        }
    }
}

/// 提取到的文件变更操作。
///
/// `Write` 变体的原 `content` 字段已删除：rewind 的文件恢复依赖 `git checkout HEAD`
/// （见 `revert_files`），从未读取 `Write.content`。保留字段只会徒增内存 + 编译器静音。
pub(crate) enum FileChange {
    Write {
        path: String,
    },
    Edit {
        path: String,
        old_string: String,
        new_string: String,
    },
}

// 新契约主实现（Phase 5 Step 5/6）：参数由 ArgsSchema 声明（注册条目挂载，
// 拦截层 Step 6 统一解析）；执行体为共享函数 [`execute_rewind`]（slash 与
// RPC 双入口复用）；反馈经 CommandFeedback 双通道（UiOnly，不进会话）——
// 事件发射统一收敛到编排层 emit_command_feedback。
#[async_trait::async_trait]
impl peri_acp_types::command::CommandHandler for RewindCommand {
    async fn execute(&self, ctx: CommandContext) -> peri_acp_types::command::CommandOutcome {
        // 参数由 ArgsSchema 统一解析（P1-1）：slash 拦截层与 execute-command
        // RPC 路径均在构造 CommandContext 前完成权威校验（失败 → 不进入
        // handler），结果经 `ctx.parsed_args` 传入——本处不再重复解析
        // （验收标准第 2 条：解析由 ArgsSchema 统一执行，handler 仅消费）。
        let parsed = ctx.parsed_args.as_ref();
        let target_message_id = parsed
            .and_then(|p| p.positionals.first())
            .cloned()
            .unwrap_or_default();
        // flags 命中 `no-revert-files` → revert_files=false（缺省
        // revert_files=true 的 serde default_true 语义保持）。
        let revert_files = !parsed
            .map(|p| p.flags.iter().any(|f| f == "no-revert-files"))
            .unwrap_or(false);

        if target_message_id.is_empty() {
            // 防御兜底（正常路径不进入：两条入口均在构造 ctx 前完成 required
            // 校验；直调 handler 的测试路径 parsed_args=None 时回落）。
            let msg = "rewind 参数解析失败: missing target_message_id".to_string();
            warn!(msg);
            return peri_acp_types::command::CommandOutcome::Done(CommandResult {
                messages: ctx.history,
                stop_reason: PromptStopReason::EndTurn,
                feedback: Some(CommandFeedback {
                    level: FeedbackLevel::Error,
                    message: msg,
                    channel: FeedbackChannel::UiOnly,
                }),
            });
        }

        peri_acp_types::command::CommandOutcome::Done(
            execute_rewind(ctx, target_message_id, revert_files).await,
        )
    }
}

/// 共享执行体（slash 与 RPC 双入口复用；Phase 5 Step 5 新增 pub(crate) 函数）。
///
/// 原 `RewindCommand.execute` 的 Step 2-6 逻辑原样迁入；错误分支（解析失败 /
/// 未找到目标 / 持久化删除失败）改 `feedback(Error, UiOnly)`（原 RewindError
/// 事件通道已删除）；成功 summary → `feedback(Info, UiOnly)`；
/// `RewindCompleted` 事件保留（TUI 重建信号）。
pub(crate) async fn execute_rewind(
    ctx: CommandContext,
    target_message_id: String,
    revert_files: bool,
) -> CommandResult {
    let history = &ctx.history;

    // Step 2: 定位目标消息
    let target_idx = history
        .iter()
        .position(|m| m.id().as_uuid().to_string() == target_message_id);

    let target_idx = match target_idx {
        Some(i) => i,
        None => {
            let msg = format!("rewind: 未找到目标消息 {target_message_id}");
            warn!(msg);
            return CommandResult {
                messages: ctx.history,
                stop_reason: PromptStopReason::EndTurn,
                feedback: Some(CommandFeedback {
                    level: FeedbackLevel::Error,
                    message: msg,
                    channel: FeedbackChannel::UiOnly,
                }),
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
    if revert_files {
        let changes = extract_file_changes(removed_messages);
        // P1 修复：revert_files 内含同步 git checkout 子进程，直接调用会
        // 阻塞 tokio worker（tokio worker_threads=4）——移出 async 上下文。
        // [shadow] 参数名 revert_files 遮蔽同名函数，经 self:: 显式路径调用。
        let cwd_owned = ctx.cwd.clone();
        revert_warnings = tokio::task::spawn_blocking(move || {
            let mut warnings = Vec::new();
            self::revert_files(&changes, &cwd_owned, &mut warnings);
            warnings
        })
        .await
        .unwrap_or_else(|e| {
            warn!("rewind: spawn_blocking join 失败: {e}");
            Vec::new()
        });
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
                    return CommandResult {
                        messages: ctx.history,
                        stop_reason: PromptStopReason::EndTurn,
                        feedback: Some(CommandFeedback {
                            level: FeedbackLevel::Error,
                            message: msg,
                            channel: FeedbackChannel::UiOnly,
                        }),
                    };
                }
            }
        }
    }

    // Step 6: 发送 RewindCompleted 事件（重建信号，保留原样）
    let mut summary = format!("已回滚 {removed_count} 条消息");
    if !revert_warnings.is_empty() {
        summary.push_str(&format!("（警告: {}）", revert_warnings.join("; ")));
    }
    events::emit_rewind_completed(
        &ctx.event_sink,
        &ctx.session_id,
        summary.clone(),
        retained_messages.clone(),
    )
    .await;

    CommandResult {
        messages: retained_messages,
        stop_reason: PromptStopReason::EndTurn,
        feedback: Some(CommandFeedback {
            level: FeedbackLevel::Info,
            message: summary,
            channel: FeedbackChannel::UiOnly,
        }),
    }
}

/// 从被移除的消息中提取所有 Write/Edit 工具调用。
pub(crate) fn extract_file_changes(messages: &[BaseMessage]) -> Vec<FileChange> {
    let mut changes = Vec::new();
    // P1 修复：BaseMessage::ai_from_blocks 会把 ContentBlock::ToolUse 同步到
    // tool_calls 字段——同一调用在两条路径各出现一次。按 ToolUse id 去重，
    // 不依赖消息构造路径（OpenAI 反序列化只有 tool_calls、Anthropic 双路径）。
    let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for msg in messages {
        if let BaseMessage::Ai {
            content,
            tool_calls,
            ..
        } = msg
        {
            // OpenAI 格式: tool_calls 字段
            for tc in tool_calls {
                // 短路求值：仅 Write/Edit 才插入 id（副作用只在首次出现时发生）
                if (tc.name == "Write" || tc.name == "Edit") && seen_ids.insert(tc.id.clone()) {
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
                    ref id,
                    ..
                } = block
                {
                    // 短路求值：仅 Write/Edit 才插入 id（副作用只在首次出现时发生）
                    if (name == "Write" || name == "Edit") && seen_ids.insert(id.clone()) {
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
pub(crate) fn parse_tool_call(name: &str, args: &serde_json::Value) -> Option<FileChange> {
    let path = args.get("file_path")?.as_str()?.to_string();
    match name {
        "Write" => {
            // content 字段不再保留：rewind 不读取它（依赖 git checkout 恢复原始内容）
            Some(FileChange::Write { path })
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
