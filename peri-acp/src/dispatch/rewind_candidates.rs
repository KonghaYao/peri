//! `session/rewind-candidates` dispatch handler.
//!
//! 查询回退候选：从 session history 提取 user 消息（`BaseMessage::Human`），
//! 排除**纯**系统提醒消息（content 仅含 `<system-reminder>...</system-reminder>`，
//! 无用户真实输入）。返回 `{ messages: [{ id, preview }] }`，id 为服务端权威
//! `MessageId`，preview 截断 200 字符。
//!
//! 注意：Bypass 模式下首轮 user 消息末尾会被追加 `<system-reminder>` 权限通知，
//! 不能直接 `contains` 过滤，否则会连带排除用户真实输入（rewind 候选为空）。
//! preview 生成时经 `strip_system_reminders` 剔除注入块，只保留用户真实文本
//! （TUI 回填/弹窗展示的都是干净的用户输入）。

use peri_acp_types::messages::{strip_system_reminders, BaseMessage};
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
        .filter_map(|m| {
            // 剥离 system-reminder 注入块：Bypass 权限通知/recall 等不应进入
            // 候选预览与输入框回填。剥离后为空（纯注入）的消息不进候选。
            let preview = strip_system_reminders(&m.content());
            let preview = preview.trim();
            if preview.is_empty() {
                return None;
            }
            Some(json!({
                "id": m.id().as_uuid().to_string(),
                "preview": preview.chars().take(200).collect::<String>(),
            }))
        })
        .collect();

    Ok(json!({ "messages": messages }))
}

#[cfg(test)]
#[path = "rewind_candidates_test.rs"]
mod tests;
