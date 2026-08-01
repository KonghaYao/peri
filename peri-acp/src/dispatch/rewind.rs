//! `session/rewind-preview` 与 `session/rewind` dispatch handlers。
//!
//! - `rewind_preview`：只读计算文件回退预算——定位目标消息，提取目标之后
//!   （含目标）被移除消息中的 Write/Edit 工具调用，按时间逆序返回。
//! - `rewind_execute`：执行回退——复用 `RewindCommand`（截断 + 文件复原 +
//!   配对校验 + 持久化删除 + RewindCompleted 事件）。

use std::sync::Arc;

use serde_json::{json, Value};
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

/// 解析 `session/rewind` 系请求的公共参数。
#[derive(serde::Deserialize)]
pub struct RewindArgs {
    pub target_message_id: String,
}

/// 计算文件回退预算（只读，不修改任何状态）。
///
/// 返回 `{ "file_changes": [{ "path", "kind" }] }`，kind ∈ {"write", "edit"}，
/// 按时间逆序（最新变更在前）。
pub async fn rewind_preview(
    params: &Value,
    session_history: &[peri_agent::messages::BaseMessage],
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
                    &peri_agent::agent::events::ExecutorEvent::CompactError {
                        message: msg.clone(),
                    },
                    0,
                )
                .await;
            return Err(AcpError::new(-32602, msg));
        }
    };

    let removed_messages = &session_history[target_idx..];
    let changes: Vec<Value> = extract_file_changes(removed_messages)
        .iter()
        .rev() // 逆序：最新变更在前
        .map(|fc| {
            let kind = match fc {
                crate::session::command::FileChange::Write { .. } => "write",
                crate::session::command::FileChange::Edit { .. } => "edit",
            };
            let path = match fc {
                crate::session::command::FileChange::Write { path } => path.clone(),
                crate::session::command::FileChange::Edit { path, .. } => path.clone(),
            };
            json!({ "path": path, "kind": kind })
        })
        .collect();

    // 截断语义与 RewindCommand 一致：removed = history[target_idx..]（含目标本身）。
    // 目标为 user 消息不含工具调用，故 extract_file_changes 结果只覆盖目标之后的
    // assistant 工具调用。空预算返回空列表（TUI 据此直接执行、不展示预算视图）。

    Ok(json!({ "file_changes": changes }))
}

/// 执行回退：复用 `RewindCommand`（Immediate 命令）。
///
/// 参数清单与 `dispatch/execute_command.rs::execute_command` 对齐。
#[allow(clippy::too_many_arguments)]
pub async fn rewind_execute(
    params: &Value,
    session_history: Vec<peri_agent::messages::BaseMessage>,
    cwd: &str,
    peri_config: &Arc<PeriConfig>,
    event_sink: &Arc<dyn EventSink>,
    auxiliary_model: Option<Arc<dyn peri_model::Model>>,
    cancel_token: &peri_agent::agent::AgentCancellationToken,
    thread_store: Option<Arc<dyn peri_agent::thread::ThreadStore>>,
    thread_id: Option<String>,
    bg_event_tx: Option<
        tokio::sync::mpsc::UnboundedSender<peri_agent::agent::events::ExecutorEvent>,
    >,
    bg_registry: Option<Arc<peri_middlewares::subagent::BackgroundTaskRegistry>>,
    frozen_claude_md: Option<Arc<String>>,
    frozen_claude_local_md: Option<Arc<String>>,
    frozen_skill_summary: Option<Arc<String>>,
    frozen_system_prompt: Option<Arc<String>>,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?
        .to_string();

    let ctx = CommandContext {
        session_id: session_id.clone(),
        history: session_history,
        cwd: cwd.to_string(),
        peri_config: Arc::clone(peri_config),
        auxiliary_model,
        event_sink: Arc::clone(event_sink),
        args: params.to_string(),
        cancel_token: cancel_token.clone(),
        thread_store,
        thread_id,
        bg_event_sender: bg_event_tx,
        bg_registry,
        frozen_claude_md,
        frozen_claude_local_md,
        frozen_skill_summary,
        frozen_system_prompt,
    };

    let result = RewindCommand.execute(ctx).await;

    // 与 execute-command dispatch 一致：Immediate 命令绕过 agent event pump，
    // 必须手动 signal completion（TRAP: issue_2026-05-29-immediate-command-missing-push-done）。
    event_sink.push_done(&session_id, "end_turn").await;

    if result.stop_reason == PromptStopReason::Cancelled {
        return Err(AcpError::new(-32603, "rewind cancelled"));
    }

    Ok(json!({ "status": "executed" }))
}

#[cfg(test)]
#[path = "rewind_test.rs"]
mod tests;
