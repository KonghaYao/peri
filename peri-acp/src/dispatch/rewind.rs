//! `session/rewind-preview` 与 `session/rewind` dispatch handlers。
//!
//! - `rewind_preview`：只读计算文件回退预算——定位目标消息，提取目标之后
//!   （含目标）被移除消息中的 Write/Edit 工具调用，按时间逆序返回。
//! - `rewind_execute`：执行回退——复用 `RewindCommand`（截断 + 文件复原 +
//!   配对校验 + 持久化删除 + RewindCompleted 事件）。

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::{
    provider::PeriConfig,
    session::{
        command::{extract_file_changes, AgentCommand, CommandContext, RewindCommand},
        event_sink::EventSink,
        executor::PromptStopReason,
    },
    transport::types::AcpError,
};
use peri_controller::Controller;

/// 解析 `session/rewind` 系请求的公共参数。
#[derive(serde::Deserialize)]
pub struct RewindArgs {
    pub target_message_id: String,
    /// 与 command/rewind.rs::RewindArgs 保持同一默认语义（P0 双保险）。
    #[serde(default = "default_true")]
    pub revert_files: bool,
    /// Exact fingerprint returned by `session/rewind-preview`. Execution is a
    /// destructive operation and therefore never accepts an unpreviewed or
    /// stale target.
    #[serde(default)]
    pub preview_fingerprint: Option<String>,
}

fn default_true() -> bool {
    true
}

/// 计算文件回退预算（只读，不修改任何状态）。
///
/// 返回 `{ "file_changes": [{ "path", "kind" }] }`，kind ∈ {"write", "edit"}，
/// 按时间逆序（最新变更在前）。
pub async fn rewind_preview(
    params: &Value,
    session_history: &[peri_acp_types::messages::BaseMessage],
    cwd: &str,
    event_sink: &Arc<dyn EventSink>,
    session_id: &str,
) -> Result<Value, AcpError> {
    let args: RewindArgs = serde_json::from_value(params.clone())
        .map_err(|e| AcpError::new(-32602, format!("rewind-preview 参数解析失败: {e}")))?;

    let target_idx = session_history
        .iter()
        .position(|m| m.id().as_uuid().to_string() == args.target_message_id);

    let target_idx = match target_idx {
        Some(i) => i,
        None => {
            let msg = format!("rewind: 未找到目标消息 {}", args.target_message_id);
            warn!(msg);
            event_sink
                .push_event(
                    session_id,
                    &peri_acp_types::event::ExecutorEvent::RewindError {
                        message: msg.clone(),
                    },
                    0,
                )
                .await;
            return Err(AcpError::new(-32602, msg));
        }
    };

    let (changes, preview_fingerprint) = preview_material(
        session_id,
        &args.target_message_id,
        args.revert_files,
        cwd,
        &session_history[target_idx..],
    )?;

    // 截断语义与 RewindCommand 一致：removed = history[target_idx..]（含目标本身）。
    // 目标为 user 消息不含工具调用，故 extract_file_changes 结果只覆盖目标之后的
    // assistant 工具调用。空预算返回空列表（TUI 据此直接执行、不展示预算视图）。

    Ok(json!({
        "file_changes": changes,
        "preview_fingerprint": preview_fingerprint,
    }))
}

const MAX_FILE_CHANGES: usize = 64;
const MAX_SAFE_PATH_BYTES: usize = 1024;

#[derive(serde::Serialize)]
struct SafeFileChange {
    path: String,
    kind: &'static str,
}

#[derive(serde::Serialize)]
struct PreviewFingerprint<'a> {
    schema: u8,
    session_id: &'a str,
    target_message_id: &'a str,
    revert_files: bool,
    removed_messages: &'a [peri_acp_types::messages::BaseMessage],
    file_changes: &'a [SafeFileChange],
}

fn preview_material(
    session_id: &str,
    target_message_id: &str,
    revert_files: bool,
    cwd: &str,
    removed_messages: &[peri_acp_types::messages::BaseMessage],
) -> Result<(Vec<SafeFileChange>, String), AcpError> {
    let cwd = normalized_absolute(Path::new(cwd))
        .ok_or_else(|| AcpError::new(-32602, "rewind preview requires an absolute session cwd"))?;
    let all_changes = extract_file_changes(removed_messages);
    if all_changes.len() > MAX_FILE_CHANGES {
        return Err(AcpError::new(
            -32602,
            format!("rewind preview exceeds {MAX_FILE_CHANGES} file changes"),
        ));
    }
    let mut changes = Vec::with_capacity(all_changes.len());
    for change in all_changes.iter().rev() {
        let (raw_path, kind) = match change {
            crate::session::command::FileChange::Write { path } => (path, "write"),
            crate::session::command::FileChange::Edit { path, .. } => (path, "edit"),
        };
        let path = safe_project_relative(&cwd, raw_path)?;
        changes.push(SafeFileChange { path, kind });
    }
    let canonical = serde_json::to_vec(&PreviewFingerprint {
        schema: 1,
        session_id,
        target_message_id,
        revert_files,
        removed_messages,
        file_changes: &changes,
    })
    .map_err(|error| AcpError::new(-32603, format!("rewind preview encode failed: {error}")))?;
    let fingerprint = format!("{:x}", Sha256::digest(canonical));
    Ok((changes, fingerprint))
}

fn safe_project_relative(cwd: &Path, raw: &str) -> Result<String, AcpError> {
    if raw.is_empty()
        || raw.len() > MAX_SAFE_PATH_BYTES
        || raw.chars().any(|character| character.is_control())
    {
        return Err(AcpError::new(
            -32602,
            "rewind preview contains an unsafe path",
        ));
    }
    let raw = Path::new(raw);
    let absolute = if raw.is_absolute() {
        normalized_absolute(raw)
    } else {
        normalized_absolute(&cwd.join(raw))
    }
    .ok_or_else(|| AcpError::new(-32602, "rewind preview contains an unsafe path"))?;
    let relative = absolute.strip_prefix(cwd).map_err(|_| {
        AcpError::new(
            -32602,
            "rewind preview contains a path outside the session cwd",
        )
    })?;
    if relative.as_os_str().is_empty() {
        return Err(AcpError::new(
            -32602,
            "rewind preview contains an unsafe path",
        ));
    }
    let relative = relative
        .to_str()
        .ok_or_else(|| AcpError::new(-32602, "rewind preview contains a non-UTF-8 path"))?;
    if relative.len() > MAX_SAFE_PATH_BYTES {
        return Err(AcpError::new(-32602, "rewind preview path is too long"));
    }
    Ok(relative.to_string())
}

fn normalized_absolute(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    Some(normalized)
}

/// 执行回退：复用 `RewindCommand`（Immediate 命令）。
///
/// 参数清单与 `dispatch/execute_command.rs::execute_command` 对齐；
/// 存储访问经 `controller.sessions()`（ARC-BOUNDARY-001 方向）。
#[allow(clippy::too_many_arguments)]
pub async fn rewind_execute(
    params: &Value,
    session_history: Vec<peri_acp_types::messages::BaseMessage>,
    cwd: &str,
    peri_config: &Arc<PeriConfig>,
    event_sink: &Arc<dyn EventSink>,
    auxiliary_model: Option<Arc<dyn peri_model::Model>>,
    cancel_token: &tokio_util::sync::CancellationToken,
    controller: &Controller,
    thread_id: Option<String>,
    bg_event_tx: Option<tokio::sync::mpsc::UnboundedSender<peri_acp_types::event::ExecutorEvent>>,
    task_manager: Option<Arc<dyn peri_acp_types::tasks::TaskManager>>,
    frozen_claude_md: Option<Arc<String>>,
    frozen_claude_local_md: Option<Arc<String>>,
    frozen_skill_summary: Option<Arc<String>>,
    frozen_system_prompt: Option<Arc<String>>,
) -> Result<Value, AcpError> {
    // P0 修复：参数预验证。RewindCommand 内部解析失败只发 RewindError 事件
    // 且本函数仍返回成功——这里前置解析，参数错误直接以 RPC 错误形式返回，
    // TUI 才能感知并展示失败。
    let args: RewindArgs = serde_json::from_value(params.clone())
        .map_err(|e| AcpError::new(-32602, format!("rewind 参数解析失败: {e}")))?;

    let session_id = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?
        .to_string();

    let target_idx = session_history
        .iter()
        .position(|message| message.id().as_uuid().to_string() == args.target_message_id)
        .ok_or_else(|| {
            AcpError::new(
                -32602,
                format!("rewind: 未找到目标消息 {}", args.target_message_id),
            )
        })?;
    let (_, expected_fingerprint) = preview_material(
        &session_id,
        &args.target_message_id,
        args.revert_files,
        cwd,
        &session_history[target_idx..],
    )?;
    let supplied_fingerprint = args.preview_fingerprint.as_deref().ok_or_else(|| {
        AcpError::new(
            -32602,
            "rewind requires preview_fingerprint from session/rewind-preview",
        )
    })?;
    if supplied_fingerprint != expected_fingerprint {
        return Err(AcpError::new(
            -32602,
            "rewind preview is stale; request a new preview before executing",
        ));
    }

    let ctx = CommandContext {
        session_id: session_id.clone(),
        history: session_history,
        cwd: cwd.to_string(),
        // L5：compact 配置由装配点预填（env overrides 每轮重新应用）
        compact_config: crate::host::compact_config::load_compact_config(peri_config),
        auxiliary_model,
        event_sink: Arc::clone(event_sink),
        args: params.to_string(),
        cancel_token: cancel_token.clone(),
        thread_store: Some(controller.sessions()),
        thread_id,
        bg_event_sender: bg_event_tx,
        task_manager,
        frozen_claude_md,
        frozen_claude_local_md,
        frozen_skill_summary,
        frozen_system_prompt,
        bg_spawner: None, // RPC 直调路径无 executor 装配面，/bg 在此路径优雅报错
    };

    let result = RewindCommand.execute(ctx).await;

    // 与 execute-command dispatch 一致：Immediate 命令绕过 agent event pump，
    // 必须手动 signal completion（TRAP: issue_2026-05-29-immediate-command-missing-push-done）。
    // 命令 turn 无 request_id（None）。
    event_sink.push_done(&session_id, "end_turn", None).await;

    if result.stop_reason == PromptStopReason::Cancelled {
        return Err(AcpError::new(-32603, "rewind cancelled"));
    }

    let history = result.messages;
    Ok(json!({
        "status": "executed",
        // P1：携带截断后的 history，调用方（TUI 进程内 ACP server）回写
        // SessionState.history，保证后续候选/预算查询与事件一致。
        "history": history,
    }))
}

#[cfg(test)]
#[path = "rewind_test.rs"]
mod tests;
