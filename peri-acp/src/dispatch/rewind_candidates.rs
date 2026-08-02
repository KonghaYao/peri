//! `session/rewind-candidates` dispatch handler.
//!
//! 查询回退候选：从 session history 提取 user 消息（`BaseMessage::Human`），
//! 排除文本包含 `<system-reminder>` 的系统注入消息（与 TUI `ReminderInfo`
//! 检测口径一致）。返回 `{ messages: [{ id, preview }] }`，id 为服务端权威
//! `MessageId`，preview 截断 200 字符。

use peri_agent::messages::BaseMessage;
use serde_json::{json, Value};

use crate::transport::types::AcpError;

/// 提取回退候选（纯计算，无副作用）。
pub fn rewind_candidates(session_history: &[BaseMessage]) -> Result<Value, AcpError> {
    let messages: Vec<Value> = session_history
        .iter()
        .rev() // P1：最新在前——弹窗第一条 = 最近一次 user 消息 = 回退一步
        .filter(|m| matches!(m, BaseMessage::Human { .. }))
        .filter(|m| !m.content().contains("<system-reminder>"))
        .map(|m| {
            json!({
                "id": m.id().as_uuid().to_string(),
                "preview": m.content().chars().take(200).collect::<String>(),
            })
        })
        .collect();

    Ok(json!({ "messages": messages }))
}

#[cfg(test)]
#[path = "rewind_candidates_test.rs"]
mod tests;
