//! Rewind 指令消费者——REWIND_ACTION_TX channel → acp_client RPC。
//!
//! 两段式流程（Rewind v2）：
//!
//! 1. `Preview { target_message_id, target_text }`（候选列表 Enter）：
//!    暂存目标文本到 REWIND_TARGET_TEXT → 查询 `session/rewind-preview` 预算
//!    → 预算空：立即执行 `session/rewind`（写 REWIND_BUDGET_STATE=Executing）
//!    → 预算非空：写 REWIND_BUDGET_STATE=Files(预算)，弹窗切预算视图
//! 2. `Confirm { target_message_id }`（预算确认 Enter）：
//!    执行 `session/rewind`（REWIND_BUDGET_STATE=Executing）
//!    执行完成由 RewindCompleted 事件驱动（handle_rewind_completed），
//!    失败路径清理 REWIND_TARGET_TEXT / REWIND_BUDGET_STATE。

use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::acp_client::AcpTuiClient;
use crate::kit::atoms::{
    ACTIVE_SESSION_ID, REWIND_BUDGET_STATE, REWIND_TARGET_TEXT, RewindBudgetState, RewindFileChange,
};

/// Rewind 用户操作——由 RewindPopup 通过 REWIND_ACTION_TX 发送。
#[derive(Debug, Clone)]
pub enum RewindAction {
    /// 候选列表 Enter：触发预算查询。
    Preview {
        target_message_id: String,
        target_text: String,
    },
    /// 预算确认 Enter：执行回退（恒 revert_files=true）。
    Confirm { target_message_id: String },
}

/// 构造预算查询参数。
pub fn build_preview_params(sid: &str, target_message_id: &str) -> Value {
    json!({
        "sessionId": sid,
        "target_message_id": target_message_id,
    })
}

/// 构造执行参数。
///
/// P0 修复：显式携带 `revert_files: true`。服务端虽已有 `#[serde(default)]`
/// 兜底，但客户端显式声明意图，双保险避免旧路径静默失败。
pub fn build_execute_params(sid: &str, target_message_id: &str) -> Value {
    json!({
        "sessionId": sid,
        "target_message_id": target_message_id,
        "revert_files": true,
    })
}

/// 解析预算响应为文件改动列表。
pub fn parse_budget_response(resp: &Value) -> Result<Vec<RewindFileChange>, String> {
    let changes = resp
        .get("file_changes")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "rewind-preview 响应缺少 file_changes 数组".to_string())?;
    changes
        .iter()
        .map(|c| {
            Ok(RewindFileChange {
                path: c
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "预算项缺少 path".to_string())?
                    .to_string(),
                kind: c
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect()
}

/// 启动 rewind 指令消费者后台任务。
pub fn spawn_rewind_consumer(
    acp_client: AcpTuiClient,
    mut rx: mpsc::UnboundedReceiver<RewindAction>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        info!("kit rewind_consumer: started");
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    info!("kit rewind_consumer: shutdown signal received, exiting");
                    break;
                }
                msg = rx.recv() => {
                    match msg {
                        None => {
                            info!("kit rewind_consumer: REWIND_ACTION_TX dropped, exiting");
                            break;
                        }
                        Some(action) => {
                            if let Err(e) = handle_action(&acp_client, action).await {
                                error!(error = %e, "kit rewind_consumer: rewind RPC failed");
                                // 失败清理：目标文本与预算状态不留残
                                *REWIND_TARGET_TEXT.state().write() = None;
                                *REWIND_BUDGET_STATE.state().write() = RewindBudgetState::Idle;
                                crate::kit::atoms::RENDER_HEARTBEAT.set(
                                    crate::kit::atoms::RENDER_HEARTBEAT.get().wrapping_add(1),
                                );
                            }
                        }
                    }
                }
            }
        }
    })
}

async fn handle_action(
    acp_client: &AcpTuiClient,
    action: RewindAction,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if !acp_client.has_session() {
        warn!("kit rewind_consumer: no active session, skipping rewind");
        return Ok(());
    }
    let sid = ACTIVE_SESSION_ID.state().read().clone();

    match action {
        RewindAction::Preview {
            target_message_id,
            target_text,
        } => {
            *REWIND_TARGET_TEXT.state().write() = Some(target_text);
            info!(
                target_message_id = %target_message_id,
                "kit rewind_consumer: querying rewind budget"
            );
            let resp = acp_client
                .send_raw_request(
                    "session/rewind-preview",
                    build_preview_params(&sid, &target_message_id),
                )
                .await?;
            let changes = parse_budget_response(&resp)?;
            if changes.is_empty() {
                // 无文件改动 → 直接执行
                *REWIND_BUDGET_STATE.state().write() = RewindBudgetState::Executing;
                crate::kit::atoms::RENDER_HEARTBEAT
                    .set(crate::kit::atoms::RENDER_HEARTBEAT.get().wrapping_add(1));
                execute_rewind(acp_client, &sid, &target_message_id).await?;
            } else {
                *REWIND_BUDGET_STATE.state().write() = RewindBudgetState::Files(changes);
                crate::kit::atoms::RENDER_HEARTBEAT
                    .set(crate::kit::atoms::RENDER_HEARTBEAT.get().wrapping_add(1));
            }
        }
        RewindAction::Confirm { target_message_id } => {
            *REWIND_BUDGET_STATE.state().write() = RewindBudgetState::Executing;
            crate::kit::atoms::RENDER_HEARTBEAT
                .set(crate::kit::atoms::RENDER_HEARTBEAT.get().wrapping_add(1));
            execute_rewind(acp_client, &sid, &target_message_id).await?;
        }
    }
    Ok(())
}

async fn execute_rewind(
    acp_client: &AcpTuiClient,
    sid: &str,
    target_message_id: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    info!(target_message_id = %target_message_id, "kit rewind_consumer: executing /rewind");
    acp_client
        .send_raw_request(
            "session/rewind",
            build_execute_params(sid, target_message_id),
        )
        .await
        .map_err(|e| {
            warn!(error = %e, "kit rewind_consumer: /rewind RPC failed");
            Box::new(e) as Box<dyn std::error::Error + Send + Sync>
        })?;
    Ok(())
}

#[cfg(test)]
#[path = "rewind_action_test.rs"]
mod tests;
