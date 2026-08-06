//! `session/rewind-candidates` dispatch handler.
//!
//! 查询回退候选：从 session history 提取 user 消息（`BaseMessage::Human`），
//! 排除**纯**系统提醒消息（content 仅含 `<system-reminder>...</system-reminder>`，
//! 无用户真实输入）。返回 `{ messages: [{ id, preview }] }`，id 为服务端权威
//! `MessageId`，preview 截断 200 字符。
//!
//! 注意：Bypass 模式下首轮 user 消息末尾会被追加 `<system-reminder>` 权限通知，
//! 不能直接 `contains` 过滤，否则会连带排除用户真实输入（rewind 候选为空）。

use peri_acp_types::messages::BaseMessage;
use serde_json::{json, Value};

use crate::transport::types::AcpError;

/// 判断 content 是否**纯**系统提醒（不含用户真实输入）。
/// 系统提醒格式：`<system-reminder>...</system-reminder>`，content 完全被此标签包裹。
fn looks_like_pure_system_reminder(content: &str) -> bool {
    let trimmed = content.trim();
    trimmed.starts_with("<system-reminder>") && trimmed.ends_with("</system-reminder>")
}

/// 提取回退候选（纯计算，无副作用）。
pub fn rewind_candidates(session_history: &[BaseMessage]) -> Result<Value, AcpError> {
    let messages: Vec<Value> = session_history
        .iter()
        .rev() // P1：最新在前——弹窗第一条 = 最近一次 user 消息 = 回退一步
        .filter(|m| matches!(m, BaseMessage::Human { .. }))
        .filter(|m| !looks_like_pure_system_reminder(&m.content()))
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
